use claw_types::canonical_intent::PubkeyBytes;
use serde::{Deserialize, Serialize};

/// The two independently authoritative clocks sampled for one watcher pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockSnapshot {
    pub now_ms: i64,
    pub current_confirmed_slot: u64,
}

/// Initial clocks before account reads.
///
/// The confirmed slot is optional so database-only blockers remain visible
/// when RPC configuration or the first slot read is unavailable. A candidate
/// can never advance to chain reads while this value is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightClockSnapshot {
    pub now_ms: i64,
    pub current_confirmed_slot: Option<u64>,
}

impl From<ClockSnapshot> for PreflightClockSnapshot {
    fn from(value: ClockSnapshot) -> Self {
        Self {
            now_ms: value.now_ms,
            current_confirmed_slot: Some(value.current_confirmed_slot),
        }
    }
}

/// SDK-neutral projection of the decoded Solend reserve and its account owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReserveFacts {
    pub address: PubkeyBytes,
    pub account_owner: PubkeyBytes,
    pub last_update_slot: u64,
    pub last_update_stale: bool,
    pub lending_market: PubkeyBytes,
    pub liquidity_mint: PubkeyBytes,
    pub liquidity_mint_decimals: u8,
    pub liquidity_supply: PubkeyBytes,
    pub pyth_oracle: PubkeyBytes,
    pub switchboard_oracle: PubkeyBytes,
    pub collateral_mint: PubkeyBytes,
    pub collateral_supply: PubkeyBytes,
}

/// SDK-neutral projection of the decoded target obligation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObligationFacts {
    pub address: PubkeyBytes,
    pub account_owner: PubkeyBytes,
    pub lending_market: PubkeyBytes,
    pub obligation_owner: PubkeyBytes,
}

/// SDK-neutral projection of a classic SPL token account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenAccountFacts {
    pub address: PubkeyBytes,
    pub account_owner: PubkeyBytes,
    pub mint: PubkeyBytes,
    pub token_owner: PubkeyBytes,
    pub amount_raw: u64,
    pub initialized: bool,
    pub frozen: bool,
}

/// Deterministic protocol addresses produced by the binary's audited adapter.
///
/// The adapter derives both ATAs with `claw-protocols`; this crate then checks
/// the derivations against the funding row and decoded account facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivedAccounts {
    pub token_program: PubkeyBytes,
    pub source_liquidity_ata: PubkeyBytes,
    pub collateral_ata: PubkeyBytes,
}

/// All immutable chain observations needed after static candidate validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainFacts {
    pub reserve: ReserveFacts,
    pub obligation: ObligationFacts,
    pub source_liquidity: Option<TokenAccountFacts>,
    pub collateral: Option<TokenAccountFacts>,
    pub derived: DerivedAccounts,
    /// Exact Solend native supply APR calculated by the existing WAD routine.
    pub native_supply_apr_wad: u128,
}
