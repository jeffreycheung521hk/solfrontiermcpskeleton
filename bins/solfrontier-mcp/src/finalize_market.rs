//! Read-only market-data seam used while finalizing a bounded intent.
//!
//! Provenance:
//! - RPC/Save fetch ordering and the four Save identity pins are adapted from
//!   predecessor `crates/gateway/src/stage2_demo_apr_bridge.rs` at commit
//!   `88591065a44eeb611840065535f8e0391f0786fa`.
//! - The deployed-mainnet 619-byte reserve offsets and integer-only WAD APR
//!   calculation are adapted from predecessor
//!   `crates/gateway/src/integrations/solend/raw.rs` and
//!   `crates/gateway/src/stage2_evaluator.rs` at commit
//!   `61d353a3fa2a35f053d7337b9b51661ad1df88e6`.
//! - The on-chain layout is pinned to Solend's `mainnet` branch commit
//!   `d04ce00bbf4356c4fd32b3be38eb9760b696bb3e`.
//!
//! This module deliberately omits the predecessor's controlled-ATA balance
//! read: finalize never consumed that value. It also omits reserve fields not
//! used by native supply-APR calculation. All provider errors are collapsed
//! to fixed, sanitized variants before crossing this boundary; raw RPC URLs,
//! query strings, response bodies, and transport errors are never logged or
//! returned.

use std::{str::FromStr, time::Duration};

use claw_solana_core::{rpc::EndpointConfig, RpcPool, RpcPoolConfig};
use serde_json::Value;
use solana_sdk::{account::Account, commitment_config::CommitmentConfig, pubkey::Pubkey};

const RPC_ENDPOINT_LABEL: &str = "configured-read-rpc";
const MARKET_READ_TIMEOUT: Duration = Duration::from_secs(8);

pub(crate) const MAIN_POOL_USDC_RESERVE_BS58: &str = "BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw";
pub(crate) const MAIN_POOL_LENDING_MARKET_BS58: &str =
    "4UpD2fh7xH3VP9QQaXtsS1YY3bxzWhtfpks7FatyKvdY";
pub(crate) const USDC_MINT_BS58: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
pub(crate) const MAIN_POOL_USDC_COLLATERAL_MINT_BS58: &str =
    "993dVFL2uXWYeoXuEBFXR4BijeXdTv4s6BzsCjJZuwqk";

const SAVE_API_BASE_URL: &str = "https://api.solend.fi";
const SAVE_API_SCOPE: &str = "solend";
const APR_BPS_SANITY_MAX: u64 = 10_000;
const SAVE_APY_PERCENT_SANITY_MAX: f64 = 1_000.0;

const RESERVE_LEN: usize = 619;
const RES_LAST_UPDATE_STALE_OFF: usize = 9;
const RES_LIQ_AVAILABLE_AMOUNT_OFF: usize = 171;
const RES_LIQ_BORROWED_AMOUNT_WADS_OFF: usize = 179;
const RES_CONFIG_OPTIMAL_UTILIZATION_RATE_OFF: usize = 299;
const RES_CONFIG_MIN_BORROW_RATE_OFF: usize = 303;
const RES_CONFIG_OPTIMAL_BORROW_RATE_OFF: usize = 304;
const RES_CONFIG_MAX_BORROW_RATE_OFF: usize = 305;
const RES_CONFIG_PROTOCOL_TAKE_RATE_OFF: usize = 372;
const RES_CONFIG_MAX_UTILIZATION_RATE_OFF: usize = 470;
const RES_CONFIG_SUPER_MAX_BORROW_RATE_OFF: usize = 471;

const SOLEND_WAD: u128 = 1_000_000_000_000_000_000;
const BPS_WAD: u128 = 100_000_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FinalizeMarketSnapshot {
    /// Confirmed slot returned immediately before the reserve read.
    pub(crate) last_checked_slot: u64,
    /// Solend native supply APR derived from the on-chain reserve.
    pub(crate) native_onchain_apr_bps: u32,
    /// Save UI display APY derived from the pinned REST response.
    pub(crate) save_display_apy_bps: u32,
}

/// One confirmed, read-only Solend reserve observation.
///
/// The account is returned intact so downstream read-only consumers can first
/// verify its owner, identity, and freshness, then pass these exact bytes to
/// [`native_supply_apr_from_reserve_data`]. They must not issue a second
/// reserve read and accidentally mix two ledger snapshots.
#[derive(Debug, Clone)]
pub(crate) struct ConfirmedReserveObservation {
    /// Confirmed slot returned immediately before the reserve read.
    pub(crate) current_confirmed_slot: u64,
    /// The exact reserve pubkey requested from RPC.
    pub(crate) reserve_pubkey: Pubkey,
    /// The exact confirmed account to validate and evaluate.
    pub(crate) reserve_account: Account,
}

/// Fixed error classes safe to return through MCP.
///
/// No variant retains an upstream error, endpoint, response body, or mismatched
/// identifier. Callers may expose `Display` and [`Self::category`] safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum FinalizeMarketReadError {
    #[error("Solana RPC pool is unavailable")]
    RpcPoolUnavailable,
    #[error("Solana confirmed-slot request failed")]
    SlotRequestFailed,
    #[error("Solana reserve-account request failed")]
    ReserveRequestFailed,
    #[error("Solana account request failed")]
    AccountRequestFailed,
    #[error("the requested Solend reserve account was not found")]
    ReserveAccountMissing,
    #[error("the pinned Solend reserve account could not be decoded")]
    ReserveDecodeFailed,
    #[error("Solend native supply APR calculation failed")]
    AprComputationFailed,
    #[error("Solend native supply APR was outside the accepted range")]
    AprOutOfRange,
    #[error("Save market-data HTTP client is unavailable")]
    SaveClientUnavailable,
    #[error("Save market-data request timed out")]
    SaveTimeout,
    #[error("Save market-data request failed")]
    SaveRequestFailed,
    #[error("Save market-data API returned HTTP status {0}")]
    SaveHttpStatus(u16),
    #[error("Save market-data response was invalid")]
    SaveInvalidResponse,
    #[error("Save market-data response did not match the pinned Main Pool USDC reserve")]
    SaveIdentityMismatch,
    #[error("Save display APY was invalid")]
    SaveRateInvalid,
}

impl FinalizeMarketReadError {
    pub(crate) const fn category(self) -> &'static str {
        match self {
            Self::RpcPoolUnavailable => "rpc_pool_unavailable",
            Self::SlotRequestFailed => "slot_request_failed",
            Self::ReserveRequestFailed => "reserve_request_failed",
            Self::AccountRequestFailed => "account_request_failed",
            Self::ReserveAccountMissing => "reserve_account_missing",
            Self::ReserveDecodeFailed => "reserve_decode_failed",
            Self::AprComputationFailed => "apr_computation_failed",
            Self::AprOutOfRange => "apr_out_of_range",
            Self::SaveClientUnavailable => "save_client_unavailable",
            Self::SaveTimeout => "save_timeout",
            Self::SaveRequestFailed => "save_request_failed",
            Self::SaveHttpStatus(_) => "save_http_status",
            Self::SaveInvalidResponse => "save_invalid_response",
            Self::SaveIdentityMismatch => "save_identity_mismatch",
            Self::SaveRateInvalid => "save_rate_invalid",
        }
    }
}

/// Narrow read-only seam. Production uses [`RpcSaveFinalizeMarketSource`];
/// finalize tests inject a deterministic offline implementation.
#[allow(async_fn_in_trait)]
pub(crate) trait FinalizeMarketDataSource: Send + Sync {
    async fn fetch_snapshot(&self) -> Result<FinalizeMarketSnapshot, FinalizeMarketReadError>;
}

/// Production source backed by the core RPC pool and the public Save API.
///
/// RPC is read through `RpcPool::read_client`; the endpoint is held only by
/// the pool. The Save base URL is a fixed public endpoint and is likewise
/// never logged or copied into an error.
#[derive(Clone)]
pub(crate) struct RpcSaveFinalizeMarketSource {
    rpc_pool: RpcPool,
    reserve_pubkey: Pubkey,
    http: Option<reqwest::Client>,
}

impl RpcSaveFinalizeMarketSource {
    fn new(rpc_url: String) -> Self {
        let rpc_pool = RpcPool::new(RpcPoolConfig {
            endpoints: vec![EndpointConfig {
                url: rpc_url,
                label: RPC_ENDPOINT_LABEL.to_owned(),
                is_write_endpoint: false,
            }],
            request_timeout: MARKET_READ_TIMEOUT,
            ..RpcPoolConfig::default()
        });
        let http = reqwest::Client::builder()
            .timeout(MARKET_READ_TIMEOUT)
            .build()
            .ok();
        Self {
            rpc_pool,
            reserve_pubkey: Pubkey::from_str(MAIN_POOL_USDC_RESERVE_BS58)
                .expect("hard-coded Main Pool USDC reserve must parse"),
            http,
        }
    }

    async fn fetch_save_display_apy_bps(&self) -> Result<u32, FinalizeMarketReadError> {
        let http = self
            .http
            .as_ref()
            .ok_or(FinalizeMarketReadError::SaveClientUnavailable)?;
        let endpoint = format!("{SAVE_API_BASE_URL}/v1/reserves");
        let response = tokio::time::timeout(
            MARKET_READ_TIMEOUT,
            http.get(endpoint)
                .query(&[
                    ("scope", SAVE_API_SCOPE),
                    ("ids", MAIN_POOL_USDC_RESERVE_BS58),
                ])
                .send(),
        )
        .await
        .map_err(|_| FinalizeMarketReadError::SaveTimeout)?
        .map_err(|_| FinalizeMarketReadError::SaveRequestFailed)?;

        let status = response.status();
        if !status.is_success() {
            return Err(FinalizeMarketReadError::SaveHttpStatus(status.as_u16()));
        }

        let body = tokio::time::timeout(MARKET_READ_TIMEOUT, response.json::<Value>())
            .await
            .map_err(|_| FinalizeMarketReadError::SaveTimeout)?
            .map_err(|_| FinalizeMarketReadError::SaveInvalidResponse)?;
        parse_save_display_apy_bps(&body)
    }

    /// Read any account at confirmed commitment without retaining an endpoint
    /// or upstream error in the returned result.
    ///
    /// `Ok(None)` is deliberately distinct from transport failure so the
    /// dry-run executor can classify a missing ATA without turning it into an
    /// RPC protocol error.
    pub(crate) async fn fetch_confirmed_account(
        &self,
        pubkey: &Pubkey,
    ) -> Result<Option<Account>, FinalizeMarketReadError> {
        let client = self
            .rpc_pool
            .read_client()
            .map_err(|_| FinalizeMarketReadError::RpcPoolUnavailable)?;
        match client
            .get_account_with_commitment(pubkey, CommitmentConfig::confirmed())
            .await
        {
            Ok(response) => {
                self.rpc_pool.record_success(&client.url());
                Ok(response.value)
            }
            Err(_) => {
                self.rpc_pool.record_failure(&client.url());
                Err(FinalizeMarketReadError::AccountRequestFailed)
            }
        }
    }

    /// Read the current slot at confirmed commitment.
    ///
    /// Dry-run watch samples this once for the initial database classification
    /// and again after all account reads. The second sample closes the
    /// deadline TOCTOU window before an unsigned plan is reported as ready.
    pub(crate) async fn fetch_confirmed_slot(&self) -> Result<u64, FinalizeMarketReadError> {
        let client = self
            .rpc_pool
            .read_client()
            .map_err(|_| FinalizeMarketReadError::RpcPoolUnavailable)?;
        match client
            .get_slot_with_commitment(CommitmentConfig::confirmed())
            .await
        {
            Ok(slot) => {
                self.rpc_pool.record_success(&client.url());
                Ok(slot)
            }
            Err(_) => {
                self.rpc_pool.record_failure(&client.url());
                Err(FinalizeMarketReadError::SlotRequestFailed)
            }
        }
    }

    /// Read one reserve at confirmed commitment and calculate its native APR
    /// using the single WAD implementation in this module.
    ///
    /// The slot is fetched immediately before exactly one reserve-account
    /// request, preserving finalize's established observation ordering.
    pub(crate) async fn fetch_confirmed_reserve_observation(
        &self,
        reserve_pubkey: Pubkey,
    ) -> Result<ConfirmedReserveObservation, FinalizeMarketReadError> {
        let client = self
            .rpc_pool
            .read_client()
            .map_err(|_| FinalizeMarketReadError::RpcPoolUnavailable)?;

        let current_confirmed_slot = match client
            .get_slot_with_commitment(CommitmentConfig::confirmed())
            .await
        {
            Ok(slot) => {
                self.rpc_pool.record_success(&client.url());
                slot
            }
            Err(_) => {
                self.rpc_pool.record_failure(&client.url());
                return Err(FinalizeMarketReadError::SlotRequestFailed);
            }
        };

        let account_response = match client
            .get_account_with_commitment(&reserve_pubkey, CommitmentConfig::confirmed())
            .await
        {
            Ok(response) => {
                self.rpc_pool.record_success(&client.url());
                response
            }
            Err(_) => {
                self.rpc_pool.record_failure(&client.url());
                return Err(FinalizeMarketReadError::ReserveRequestFailed);
            }
        };
        let reserve_account = account_response
            .value
            .ok_or(FinalizeMarketReadError::ReserveAccountMissing)?;

        Ok(ConfirmedReserveObservation {
            current_confirmed_slot,
            reserve_pubkey,
            reserve_account,
        })
    }
}

/// A missing, blank, or non-Unicode RPC setting leaves the server available.
/// The finalize adapter maps `None` to its normal `config_missing` JSON result.
pub(crate) fn configured_finalize_market_source_from_env() -> Option<RpcSaveFinalizeMarketSource> {
    let rpc_url = std::env::var_os("SOLFRONTIER_RPC_URL")?
        .into_string()
        .ok()?;
    let rpc_url = rpc_url.trim();
    if rpc_url.is_empty() {
        return None;
    }
    Some(RpcSaveFinalizeMarketSource::new(rpc_url.to_owned()))
}

impl FinalizeMarketDataSource for RpcSaveFinalizeMarketSource {
    async fn fetch_snapshot(&self) -> Result<FinalizeMarketSnapshot, FinalizeMarketReadError> {
        let observation = self
            .fetch_confirmed_reserve_observation(self.reserve_pubkey)
            .await?;
        let (_, native_onchain_apr_bps) =
            native_supply_apr_from_reserve_data(&observation.reserve_account.data)?;

        // Preserve predecessor ordering and fail closed: Save is read only
        // after the native snapshot succeeds, and there is no native fallback.
        let save_display_apy_bps = self.fetch_save_display_apy_bps().await?;

        Ok(FinalizeMarketSnapshot {
            last_checked_slot: observation.current_confirmed_slot,
            native_onchain_apr_bps,
            save_display_apy_bps,
        })
    }
}

/// Evaluate a previously validated reserve account with the one canonical
/// integer-only WAD implementation shared by finalize and dry-run watch.
pub(crate) fn native_supply_apr_from_reserve_data(
    reserve_data: &[u8],
) -> Result<(u128, u32), FinalizeMarketReadError> {
    let reserve = decode_reserve_rate_snapshot(reserve_data)
        .map_err(|_| FinalizeMarketReadError::ReserveDecodeFailed)?;
    let native_onchain_apr_wad =
        supply_apr_wad(&reserve).map_err(|_| FinalizeMarketReadError::AprComputationFailed)?;
    let native_onchain_apr_bps = apr_bps_from_wad(native_onchain_apr_wad);
    if native_onchain_apr_bps > APR_BPS_SANITY_MAX {
        return Err(FinalizeMarketReadError::AprOutOfRange);
    }
    let native_onchain_apr_bps = u32::try_from(native_onchain_apr_bps)
        .map_err(|_| FinalizeMarketReadError::AprOutOfRange)?;

    Ok((native_onchain_apr_wad, native_onchain_apr_bps))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReserveRateSnapshot {
    available_amount: u64,
    borrowed_amount_wads: u128,
    min_borrow_rate_pct: u8,
    optimal_borrow_rate_pct: u8,
    max_borrow_rate_pct: u8,
    super_max_borrow_rate_pct: u64,
    optimal_utilization_rate_pct: u8,
    max_utilization_rate_pct: u8,
    protocol_take_rate_pct: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReserveDecodeError {
    WrongSize,
    StaleBitInvalid,
}

fn decode_reserve_rate_snapshot(data: &[u8]) -> Result<ReserveRateSnapshot, ReserveDecodeError> {
    if data.len() != RESERVE_LEN {
        return Err(ReserveDecodeError::WrongSize);
    }
    if !matches!(data[RES_LAST_UPDATE_STALE_OFF], 0 | 1) {
        return Err(ReserveDecodeError::StaleBitInvalid);
    }

    Ok(ReserveRateSnapshot {
        available_amount: read_u64_le(data, RES_LIQ_AVAILABLE_AMOUNT_OFF),
        borrowed_amount_wads: read_u128_le(data, RES_LIQ_BORROWED_AMOUNT_WADS_OFF),
        min_borrow_rate_pct: data[RES_CONFIG_MIN_BORROW_RATE_OFF],
        optimal_borrow_rate_pct: data[RES_CONFIG_OPTIMAL_BORROW_RATE_OFF],
        max_borrow_rate_pct: data[RES_CONFIG_MAX_BORROW_RATE_OFF],
        super_max_borrow_rate_pct: read_u64_le(data, RES_CONFIG_SUPER_MAX_BORROW_RATE_OFF),
        optimal_utilization_rate_pct: data[RES_CONFIG_OPTIMAL_UTILIZATION_RATE_OFF],
        max_utilization_rate_pct: data[RES_CONFIG_MAX_UTILIZATION_RATE_OFF],
        protocol_take_rate_pct: data[RES_CONFIG_PROTOCOL_TAKE_RATE_OFF],
    })
}

fn read_u64_le(data: &[u8], offset: usize) -> u64 {
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&data[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

fn read_u128_le(data: &[u8], offset: usize) -> u128 {
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&data[offset..offset + 16]);
    u128::from_le_bytes(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AprMathError {
    InvalidConfig,
    Overflow,
    DivisionByZero,
}

fn supply_apr_wad(reserve: &ReserveRateSnapshot) -> Result<u128, AprMathError> {
    if !is_reserve_config_valid(reserve) {
        return Err(AprMathError::InvalidConfig);
    }

    let utilization = utilization_wad(reserve)?;
    let borrow_rate = current_borrow_rate_wad(reserve)?;
    let take_rate = pct_to_wad(u64::from(reserve.protocol_take_rate_pct))?;
    let one_minus_take = SOLEND_WAD
        .checked_sub(take_rate)
        .ok_or(AprMathError::Overflow)?;
    let after_take = mul_wad(utilization, one_minus_take)?;
    mul_wad(borrow_rate, after_take)
}

fn is_reserve_config_valid(reserve: &ReserveRateSnapshot) -> bool {
    let min = u64::from(reserve.min_borrow_rate_pct);
    let optimal = u64::from(reserve.optimal_borrow_rate_pct);
    let max = u64::from(reserve.max_borrow_rate_pct);
    if !(min <= optimal && optimal <= max && max <= reserve.super_max_borrow_rate_pct) {
        return false;
    }
    if !(reserve.optimal_utilization_rate_pct <= reserve.max_utilization_rate_pct
        && reserve.max_utilization_rate_pct <= 100)
    {
        return false;
    }
    reserve.protocol_take_rate_pct <= 100
}

fn utilization_wad(reserve: &ReserveRateSnapshot) -> Result<u128, AprMathError> {
    if reserve.borrowed_amount_wads == 0 {
        return Ok(0);
    }
    let available_wads = u128::from(reserve.available_amount)
        .checked_mul(SOLEND_WAD)
        .ok_or(AprMathError::Overflow)?;
    let denominator = reserve
        .borrowed_amount_wads
        .checked_add(available_wads)
        .ok_or(AprMathError::Overflow)?;
    if denominator == 0 {
        return Ok(0);
    }
    div_wad(reserve.borrowed_amount_wads, denominator)
}

fn current_borrow_rate_wad(reserve: &ReserveRateSnapshot) -> Result<u128, AprMathError> {
    let utilization = utilization_wad(reserve)?;
    let optimal_utilization = pct_to_wad(u64::from(reserve.optimal_utilization_rate_pct))?;
    let max_utilization = pct_to_wad(u64::from(reserve.max_utilization_rate_pct))?;

    if utilization <= optimal_utilization {
        let min_rate = pct_to_wad(u64::from(reserve.min_borrow_rate_pct))?;
        if optimal_utilization == 0 {
            return Ok(min_rate);
        }
        let normalized = div_wad(utilization, optimal_utilization)?;
        let rate_range = u64::from(reserve.optimal_borrow_rate_pct)
            .checked_sub(u64::from(reserve.min_borrow_rate_pct))
            .ok_or(AprMathError::Overflow)?;
        let scaled = mul_wad(normalized, pct_to_wad(rate_range)?)?;
        return scaled.checked_add(min_rate).ok_or(AprMathError::Overflow);
    }

    if utilization <= max_utilization {
        let weight_numerator = utilization
            .checked_sub(optimal_utilization)
            .ok_or(AprMathError::Overflow)?;
        let weight_denominator = max_utilization
            .checked_sub(optimal_utilization)
            .ok_or(AprMathError::Overflow)?;
        if weight_denominator == 0 {
            return pct_to_wad(u64::from(reserve.optimal_borrow_rate_pct));
        }
        let weight = div_wad(weight_numerator, weight_denominator)?;
        let optimal_rate = pct_to_wad(u64::from(reserve.optimal_borrow_rate_pct))?;
        let max_rate = pct_to_wad(u64::from(reserve.max_borrow_rate_pct))?;
        let rate_range = max_rate
            .checked_sub(optimal_rate)
            .ok_or(AprMathError::Overflow)?;
        let scaled = mul_wad(weight, rate_range)?;
        return scaled
            .checked_add(optimal_rate)
            .ok_or(AprMathError::Overflow);
    }

    let weight_numerator = utilization
        .checked_sub(max_utilization)
        .ok_or(AprMathError::Overflow)?;
    let weight_denominator = SOLEND_WAD
        .checked_sub(max_utilization)
        .ok_or(AprMathError::Overflow)?;
    if weight_denominator == 0 {
        return pct_to_wad(reserve.super_max_borrow_rate_pct);
    }
    let weight = div_wad(weight_numerator, weight_denominator)?;
    let max_rate = pct_to_wad(u64::from(reserve.max_borrow_rate_pct))?;
    let super_max_rate = pct_to_wad(reserve.super_max_borrow_rate_pct)?;
    let rate_range = super_max_rate
        .checked_sub(max_rate)
        .ok_or(AprMathError::Overflow)?;
    let scaled = mul_wad(weight, rate_range)?;
    scaled.checked_add(max_rate).ok_or(AprMathError::Overflow)
}

fn pct_to_wad(percent: u64) -> Result<u128, AprMathError> {
    u128::from(percent)
        .checked_mul(10_u128.pow(16))
        .ok_or(AprMathError::Overflow)
}

fn mul_wad(left: u128, right: u128) -> Result<u128, AprMathError> {
    mul_div_floor_u128(left, right, SOLEND_WAD)
}

fn div_wad(numerator: u128, denominator: u128) -> Result<u128, AprMathError> {
    if denominator == 0 {
        return Err(AprMathError::DivisionByZero);
    }
    mul_div_floor_u128(numerator, SOLEND_WAD, denominator)
}

/// `floor((left * right) / divisor)` with the same 256-bit intermediate used
/// by the legacy evaluator and its on-chain verifier.
fn mul_div_floor_u128(left: u128, right: u128, divisor: u128) -> Result<u128, AprMathError> {
    if divisor == 0 {
        return Err(AprMathError::DivisionByZero);
    }
    if let Some(product) = left.checked_mul(right) {
        return Ok(product / divisor);
    }

    let left_low = left as u64 as u128;
    let left_high = left >> 64;
    let right_low = right as u64 as u128;
    let right_high = right >> 64;

    let low_low = left_low * right_low;
    let low_high = left_low * right_high;
    let high_low = left_high * right_low;
    let high_high = left_high * right_high;

    let middle = low_high.wrapping_add(high_low);
    let middle_carry = if middle < low_high { 1_u128 << 64 } else { 0 };
    let middle_low = middle << 64;
    let middle_high = middle >> 64;
    let (low, low_carry) = low_low.overflowing_add(middle_low);
    let mut high = high_high
        .checked_add(middle_high)
        .ok_or(AprMathError::Overflow)?
        .checked_add(low_carry as u128)
        .ok_or(AprMathError::Overflow)?;
    high = high
        .checked_add(middle_carry)
        .ok_or(AprMathError::Overflow)?;

    let mut quotient = 0_u128;
    let mut quotient_overflow = false;
    let mut remainder = 0_u128;
    for index in (0..256).rev() {
        let remainder_overflow = (remainder >> 127) & 1 == 1;
        remainder = (remainder << 1) | bit_of_u256(high, low, index as u32);
        let subtract = remainder_overflow || remainder >= divisor;
        if subtract {
            remainder = remainder.wrapping_sub(divisor);
        }
        if (quotient >> 127) & 1 == 1 {
            quotient_overflow = true;
        }
        quotient = (quotient << 1) | (subtract as u128);
    }
    if quotient_overflow {
        return Err(AprMathError::Overflow);
    }
    Ok(quotient)
}

#[inline]
fn bit_of_u256(high: u128, low: u128, index: u32) -> u128 {
    if index < 128 {
        (low >> index) & 1
    } else {
        (high >> (index - 128)) & 1
    }
}

fn apr_bps_from_wad(wad: u128) -> u64 {
    u64::try_from(wad / BPS_WAD).unwrap_or(u64::MAX)
}

fn parse_save_display_apy_bps(body: &Value) -> Result<u32, FinalizeMarketReadError> {
    let result = body
        .get("results")
        .and_then(Value::as_array)
        .and_then(|results| results.first())
        .ok_or(FinalizeMarketReadError::SaveInvalidResponse)?;

    require_pinned_string(
        result.pointer("/reserve/pubkey"),
        MAIN_POOL_USDC_RESERVE_BS58,
    )?;
    require_pinned_string(
        result.pointer("/reserve/lendingMarket"),
        MAIN_POOL_LENDING_MARKET_BS58,
    )?;
    require_pinned_string(
        result.pointer("/reserve/liquidity/mintPubkey"),
        USDC_MINT_BS58,
    )?;
    require_pinned_string(
        result.pointer("/reserve/collateral/mintPubkey"),
        MAIN_POOL_USDC_COLLATERAL_MINT_BS58,
    )?;

    let supply_interest = result
        .pointer("/rates/supplyInterest")
        .and_then(Value::as_str)
        .ok_or(FinalizeMarketReadError::SaveInvalidResponse)?;
    parse_percent_string_to_bps(supply_interest)
}

fn require_pinned_string(
    actual: Option<&Value>,
    expected: &str,
) -> Result<(), FinalizeMarketReadError> {
    let actual = actual
        .and_then(Value::as_str)
        .ok_or(FinalizeMarketReadError::SaveInvalidResponse)?;
    if actual != expected {
        return Err(FinalizeMarketReadError::SaveIdentityMismatch);
    }
    Ok(())
}

fn parse_percent_string_to_bps(value: &str) -> Result<u32, FinalizeMarketReadError> {
    let percent = value
        .trim()
        .parse::<f64>()
        .map_err(|_| FinalizeMarketReadError::SaveRateInvalid)?;
    if !percent.is_finite() || !(0.0..=SAVE_APY_PERCENT_SANITY_MAX).contains(&percent) {
        return Err(FinalizeMarketReadError::SaveRateInvalid);
    }
    let bps = (percent * 100.0).round();
    if !(0.0..=f64::from(u32::MAX)).contains(&bps) {
        return Err(FinalizeMarketReadError::SaveRateInvalid);
    }
    Ok(bps as u32)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;

    struct MockMarketSource {
        result: Mutex<Result<FinalizeMarketSnapshot, FinalizeMarketReadError>>,
        calls: Mutex<u32>,
    }

    impl MockMarketSource {
        fn returning(result: Result<FinalizeMarketSnapshot, FinalizeMarketReadError>) -> Self {
            Self {
                result: Mutex::new(result),
                calls: Mutex::new(0),
            }
        }
    }

    impl FinalizeMarketDataSource for MockMarketSource {
        async fn fetch_snapshot(&self) -> Result<FinalizeMarketSnapshot, FinalizeMarketReadError> {
            *self.calls.lock().expect("calls lock") += 1;
            *self.result.lock().expect("result lock")
        }
    }

    fn rate_snapshot(available_amount: u64, borrowed_amount: u64) -> ReserveRateSnapshot {
        ReserveRateSnapshot {
            available_amount,
            borrowed_amount_wads: u128::from(borrowed_amount) * SOLEND_WAD,
            min_borrow_rate_pct: 0,
            optimal_borrow_rate_pct: 4,
            max_borrow_rate_pct: 30,
            super_max_borrow_rate_pct: 100,
            optimal_utilization_rate_pct: 80,
            max_utilization_rate_pct: 90,
            protocol_take_rate_pct: 5,
        }
    }

    fn reserve_fixture() -> Vec<u8> {
        let snapshot = rate_snapshot(1_000_000, 1_000_000);
        let mut bytes = vec![0_u8; RESERVE_LEN];
        bytes[0] = 1;
        bytes[1..9].copy_from_slice(&415_083_795_u64.to_le_bytes());
        bytes[RES_LAST_UPDATE_STALE_OFF] = 0;
        bytes[RES_LIQ_AVAILABLE_AMOUNT_OFF..RES_LIQ_AVAILABLE_AMOUNT_OFF + 8]
            .copy_from_slice(&snapshot.available_amount.to_le_bytes());
        bytes[RES_LIQ_BORROWED_AMOUNT_WADS_OFF..RES_LIQ_BORROWED_AMOUNT_WADS_OFF + 16]
            .copy_from_slice(&snapshot.borrowed_amount_wads.to_le_bytes());
        bytes[RES_CONFIG_OPTIMAL_UTILIZATION_RATE_OFF] = snapshot.optimal_utilization_rate_pct;
        bytes[RES_CONFIG_MIN_BORROW_RATE_OFF] = snapshot.min_borrow_rate_pct;
        bytes[RES_CONFIG_OPTIMAL_BORROW_RATE_OFF] = snapshot.optimal_borrow_rate_pct;
        bytes[RES_CONFIG_MAX_BORROW_RATE_OFF] = snapshot.max_borrow_rate_pct;
        bytes[RES_CONFIG_PROTOCOL_TAKE_RATE_OFF] = snapshot.protocol_take_rate_pct;
        bytes[RES_CONFIG_MAX_UTILIZATION_RATE_OFF] = snapshot.max_utilization_rate_pct;
        bytes[RES_CONFIG_SUPER_MAX_BORROW_RATE_OFF..RES_CONFIG_SUPER_MAX_BORROW_RATE_OFF + 8]
            .copy_from_slice(&snapshot.super_max_borrow_rate_pct.to_le_bytes());
        bytes
    }

    fn save_fixture() -> Value {
        json!({
            "results": [{
                "reserve": {
                    "pubkey": MAIN_POOL_USDC_RESERVE_BS58,
                    "lendingMarket": MAIN_POOL_LENDING_MARKET_BS58,
                    "liquidity": {"mintPubkey": USDC_MINT_BS58},
                    "collateral": {
                        "mintPubkey": MAIN_POOL_USDC_COLLATERAL_MINT_BS58
                    }
                },
                "rates": {"supplyInterest": "2.10"},
                "rewards": []
            }],
            "next": null
        })
    }

    #[tokio::test]
    async fn trait_seam_returns_exact_offline_snapshot() {
        let expected = FinalizeMarketSnapshot {
            last_checked_slot: 415_083_795,
            native_onchain_apr_bps: 118,
            save_display_apy_bps: 210,
        };
        let source = MockMarketSource::returning(Ok(expected));

        assert_eq!(source.fetch_snapshot().await, Ok(expected));
        assert_eq!(*source.calls.lock().expect("calls lock"), 1);
    }

    #[test]
    fn fixed_619_byte_reserve_decodes_and_matches_golden_apr() {
        let bytes = reserve_fixture();
        let reserve = decode_reserve_rate_snapshot(&bytes).expect("decode fixture");

        assert_eq!(bytes.len(), 619);
        assert_eq!(reserve.available_amount, 1_000_000);
        assert_eq!(reserve.borrowed_amount_wads, 1_000_000_u128 * SOLEND_WAD);
        assert_eq!(reserve.optimal_utilization_rate_pct, 80);
        assert_eq!(reserve.min_borrow_rate_pct, 0);
        assert_eq!(reserve.optimal_borrow_rate_pct, 4);
        assert_eq!(reserve.max_borrow_rate_pct, 30);
        assert_eq!(reserve.protocol_take_rate_pct, 5);
        assert_eq!(reserve.max_utilization_rate_pct, 90);
        assert_eq!(reserve.super_max_borrow_rate_pct, 100);

        let apr_wad = supply_apr_wad(&reserve).expect("compute APR");
        assert_eq!(apr_wad, 11_875_000_000_000_000);
        assert_eq!(apr_bps_from_wad(apr_wad), 118);
    }

    #[test]
    fn confirmed_observation_preserves_account_and_wad_uses_the_same_bytes() {
        let current_confirmed_slot = 415_083_795;
        let reserve_pubkey = Pubkey::new_from_array([0x41; 32]);
        let reserve_owner = Pubkey::new_from_array([0x42; 32]);
        let reserve_account = Account {
            lamports: 12_345,
            data: reserve_fixture(),
            owner: reserve_owner,
            executable: false,
            rent_epoch: 99,
        };

        let observation = ConfirmedReserveObservation {
            current_confirmed_slot,
            reserve_pubkey,
            reserve_account: reserve_account.clone(),
        };
        let (apr_wad, apr_bps) =
            native_supply_apr_from_reserve_data(&observation.reserve_account.data)
                .expect("fixture must produce the native APR");

        assert_eq!(observation.current_confirmed_slot, current_confirmed_slot);
        assert_eq!(observation.reserve_pubkey, reserve_pubkey);
        assert_eq!(observation.reserve_account.owner, reserve_owner);
        assert_eq!(observation.reserve_account.data, reserve_account.data);
        assert_eq!(
            observation.reserve_account.lamports,
            reserve_account.lamports
        );
        assert_eq!(
            observation.reserve_account.rent_epoch,
            reserve_account.rent_epoch
        );
        assert_eq!(
            observation.reserve_account.executable,
            reserve_account.executable
        );
        assert_eq!(apr_wad, 11_875_000_000_000_000);
        assert_eq!(apr_bps, 118);
    }

    #[test]
    fn native_apr_adapter_maps_decode_and_math_failures_to_fixed_classes() {
        assert_eq!(
            native_supply_apr_from_reserve_data(&vec![0_u8; RESERVE_LEN - 1]),
            Err(FinalizeMarketReadError::ReserveDecodeFailed)
        );

        let mut invalid_stale = reserve_fixture();
        invalid_stale[RES_LAST_UPDATE_STALE_OFF] = 2;
        assert_eq!(
            native_supply_apr_from_reserve_data(&invalid_stale),
            Err(FinalizeMarketReadError::ReserveDecodeFailed)
        );

        let mut invalid_curve = reserve_fixture();
        invalid_curve[RES_CONFIG_MIN_BORROW_RATE_OFF] = 200;
        invalid_curve[RES_CONFIG_OPTIMAL_BORROW_RATE_OFF] = 1;
        assert_eq!(
            native_supply_apr_from_reserve_data(&invalid_curve),
            Err(FinalizeMarketReadError::AprComputationFailed)
        );
    }

    #[test]
    fn reserve_decoder_rejects_size_and_noncanonical_stale_bit() {
        assert_eq!(
            decode_reserve_rate_snapshot(&vec![0_u8; RESERVE_LEN - 1]),
            Err(ReserveDecodeError::WrongSize)
        );

        let mut bytes = reserve_fixture();
        bytes[RES_LAST_UPDATE_STALE_OFF] = 2;
        assert_eq!(
            decode_reserve_rate_snapshot(&bytes),
            Err(ReserveDecodeError::StaleBitInvalid)
        );
    }

    #[test]
    fn legacy_three_region_rate_curve_is_preserved_exactly() {
        let region_one = rate_snapshot(50, 50);
        assert_eq!(
            apr_bps_from_wad(supply_apr_wad(&region_one).expect("region 1")),
            118
        );

        let mut region_two = rate_snapshot(15, 85);
        region_two.protocol_take_rate_pct = 0;
        assert_eq!(
            apr_bps_from_wad(supply_apr_wad(&region_two).expect("region 2")),
            1_445
        );

        let mut region_three = rate_snapshot(5, 95);
        region_three.protocol_take_rate_pct = 0;
        assert_eq!(
            apr_bps_from_wad(supply_apr_wad(&region_three).expect("region 3")),
            6_175
        );
    }

    #[test]
    fn invalid_config_and_arithmetic_overflow_fail_closed() {
        let mut invalid = rate_snapshot(50, 50);
        invalid.min_borrow_rate_pct = 5;
        invalid.optimal_borrow_rate_pct = 4;
        assert_eq!(supply_apr_wad(&invalid), Err(AprMathError::InvalidConfig));

        let mut overflow = rate_snapshot(u64::MAX, 0);
        overflow.borrowed_amount_wads = u128::MAX;
        assert_eq!(utilization_wad(&overflow), Err(AprMathError::Overflow));
    }

    #[test]
    fn bps_conversion_truncates_toward_zero() {
        assert_eq!(apr_bps_from_wad(119 * BPS_WAD), 119);
        assert_eq!(apr_bps_from_wad(BPS_WAD / 2), 0);
        assert_eq!(apr_bps_from_wad(1_000 * BPS_WAD + BPS_WAD / 2), 1_000);
    }

    #[test]
    fn save_fixture_verifies_all_four_pins_and_converts_percent_to_bps() {
        assert_eq!(parse_save_display_apy_bps(&save_fixture()), Ok(210));

        for pointer in [
            "/results/0/reserve/pubkey",
            "/results/0/reserve/lendingMarket",
            "/results/0/reserve/liquidity/mintPubkey",
            "/results/0/reserve/collateral/mintPubkey",
        ] {
            let mut body = save_fixture();
            *body
                .pointer_mut(pointer)
                .expect("fixture identity path must exist") =
                json!("11111111111111111111111111111111");
            assert_eq!(
                parse_save_display_apy_bps(&body),
                Err(FinalizeMarketReadError::SaveIdentityMismatch),
                "identity path {pointer} must be pinned"
            );
        }
    }

    #[test]
    fn save_parser_rejects_missing_and_invalid_rate_without_fallback() {
        for body in [
            json!({}),
            json!({"results": []}),
            json!({"results": [{"reserve": {}}]}),
        ] {
            assert_eq!(
                parse_save_display_apy_bps(&body),
                Err(FinalizeMarketReadError::SaveInvalidResponse)
            );
        }

        for rate in ["NaN", "-0.01", "1000.01", "not-a-number"] {
            let mut body = save_fixture();
            *body
                .pointer_mut("/results/0/rates/supplyInterest")
                .expect("fixture rate path") = json!(rate);
            assert_eq!(
                parse_save_display_apy_bps(&body),
                Err(FinalizeMarketReadError::SaveRateInvalid),
                "rate {rate:?} must fail closed"
            );
        }
    }

    #[test]
    fn every_exposed_error_is_fixed_and_sanitized() {
        let errors = [
            FinalizeMarketReadError::RpcPoolUnavailable,
            FinalizeMarketReadError::SlotRequestFailed,
            FinalizeMarketReadError::ReserveRequestFailed,
            FinalizeMarketReadError::AccountRequestFailed,
            FinalizeMarketReadError::ReserveAccountMissing,
            FinalizeMarketReadError::ReserveDecodeFailed,
            FinalizeMarketReadError::AprComputationFailed,
            FinalizeMarketReadError::AprOutOfRange,
            FinalizeMarketReadError::SaveClientUnavailable,
            FinalizeMarketReadError::SaveTimeout,
            FinalizeMarketReadError::SaveRequestFailed,
            FinalizeMarketReadError::SaveHttpStatus(503),
            FinalizeMarketReadError::SaveInvalidResponse,
            FinalizeMarketReadError::SaveIdentityMismatch,
            FinalizeMarketReadError::SaveRateInvalid,
        ];
        for error in errors {
            let exposed = format!("{}:{}", error.category(), error);
            for forbidden in [
                "rpc-secret-sentinel",
                "provider-body-sentinel",
                "raw-reqwest-error",
                "?api-key=",
            ] {
                assert!(
                    !exposed.contains(forbidden),
                    "error must not contain {forbidden:?}"
                );
            }
        }
    }

    #[test]
    fn source_has_no_write_sign_or_raw_error_capability() {
        const SOURCE: &str = include_str!("finalize_market.rs");
        let forbidden = [
            [".", "post("].concat(),
            ["write_", "client("].concat(),
            [".send_", "transaction("].concat(),
            [".send_raw_", "transaction("].concat(),
            ["Keypair", "::new("].concat(),
            ["private", "_key"].concat(),
            ["error ", "= %"].concat(),
            ["rpc_url ", "= %"].concat(),
        ];
        for needle in forbidden {
            assert!(
                !SOURCE.contains(&needle),
                "market source must not contain {needle:?}"
            );
        }
    }
}
