//! Pure Phase 2 draft-intent validation and canonical draft hashing.
//!
//! Adapted from predecessor path
//! `crates/gateway/src/stage2_phase5c_draft.rs` at commit
//! `ade3d6e942f403bf71960acdf1880bc6a48ccf12`; the frozen hash contract
//! originated at commit `3d30617d1c13231b05e6dd5b072f471b1979eeee`.
//!
//! The predecessor's audit/session-only `DraftIntent` fields are deliberately
//! cropped: `draft_id`, `parser_source`, `warnings`, `review_copy`,
//! `created_at_ms`, and `session_id_hex`. Its in-memory `DraftIntentStore` is
//! also omitted: this MCP slice keeps no state at all. The raw user message is
//! accepted only long enough to compute `original_user_message_hash`; it is
//! never returned or persisted.
//!
//! `stage2_llm_intent_extractor` is intentionally not migrated. The MCP host
//! LLM supplies these schemars-typed arguments directly. The separate
//! `claw_types::CanonicalIntent/canonical_hash` Borsh domain belongs to
//! finalized immediate actions and is neither copied nor substituted for this
//! backward-compatible, sorted-JSON draft hash.

use std::{collections::BTreeMap, str::FromStr};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use solana_sdk::{hash::hash as sha256, pubkey::Pubkey};

pub(crate) const MIN_AMOUNT_RAW: u64 = 100_000;
pub(crate) const MAX_AMOUNT_RAW: u64 = 1_000_000;
pub(crate) const EXPIRY_SECONDS_AFTER_FINALIZE: u64 = 180;
pub(crate) const MAX_THRESHOLD_BPS: u32 = 10_000;

const USDC_DECIMALS: u32 = 6;
const USDC_RAW_PER_WHOLE: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DraftActionParam {
    Deposit,
}

impl DraftActionParam {
    fn as_str(self) -> &'static str {
        match self {
            Self::Deposit => "deposit",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DraftProtocolParam {
    Solend,
}

impl DraftProtocolParam {
    fn as_str(self) -> &'static str {
        match self {
            Self::Solend => "solend",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
pub(crate) enum DraftAssetParam {
    #[serde(rename = "USDC")]
    Usdc,
}

impl DraftAssetParam {
    fn as_str(self) -> &'static str {
        match self {
            Self::Usdc => "USDC",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DraftDisplaySourceParam {
    Save,
}

impl DraftDisplaySourceParam {
    fn as_str(self) -> &'static str {
        match self {
            Self::Save => "save",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DraftComparisonParam {
    Gt,
}

impl DraftComparisonParam {
    fn as_str(self) -> &'static str {
        match self {
            Self::Gt => "gt",
        }
    }
}

/// Typed MCP input. Closed enums keep the Phase 5c-lite v1 policy pins in the
/// generated JSON Schema; semantic ranges are revalidated in Rust.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProposeIntentParams {
    /// Only `deposit` is supported by this slice.
    pub(crate) action: DraftActionParam,
    /// Only `solend` is supported by this slice.
    pub(crate) protocol: DraftProtocolParam,
    /// Only `USDC` is supported by this slice.
    pub(crate) asset: DraftAssetParam,
    /// Save/Solend APY display source; pinned to `save`.
    pub(crate) display_source: DraftDisplaySourceParam,
    /// Conditional comparison; pinned to `gt` (greater than).
    pub(crate) comparison: DraftComparisonParam,
    /// Decimal USDC amount, parsed exactly without floating point.
    pub(crate) amount: String,
    /// APY threshold in basis points; valid range is 1 through 10,000.
    pub(crate) threshold_bps: u32,
    /// Funding expiry window after finalize; v1 is exactly 180 seconds.
    pub(crate) expiry_seconds_after_finalize: u64,
    /// Controlled-wallet Solana public key.
    pub(crate) controlled_wallet: String,
    /// Controlled wallet's USDC associated-token-account public key.
    pub(crate) controlled_usdc_ata: String,
    /// Exact user request text. It is hashed, then immediately discarded.
    pub(crate) original_user_message: String,
}

/// Canonical-only subset of the predecessor `DraftIntent`.
///
/// Audit/session fields are absent by construction. `amount_raw` remains a
/// `u64` here, matching the predecessor, and is projected to a decimal string
/// only in the frozen hash preimage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DraftIntent {
    pub(crate) action: &'static str,
    pub(crate) protocol: &'static str,
    pub(crate) asset: &'static str,
    pub(crate) display_source: &'static str,
    pub(crate) comparison: &'static str,
    pub(crate) threshold_bps: u32,
    pub(crate) amount_raw: u64,
    pub(crate) expiry_seconds_after_finalize: u64,
    pub(crate) controlled_wallet: String,
    pub(crate) controlled_usdc_ata: String,
    pub(crate) original_user_message_hash: String,
}

impl DraftIntent {
    fn canonical_preimage(&self) -> CanonicalDraftPreimage {
        CanonicalDraftPreimage {
            action: self.action.to_owned(),
            amount_raw: self.amount_raw.to_string(),
            asset: self.asset.to_owned(),
            comparison: self.comparison.to_owned(),
            controlled_usdc_ata: self.controlled_usdc_ata.clone(),
            controlled_wallet: self.controlled_wallet.clone(),
            display_source: self.display_source.to_owned(),
            expiry_seconds_after_finalize: self.expiry_seconds_after_finalize,
            original_user_message_hash: self.original_user_message_hash.clone(),
            protocol: self.protocol.to_owned(),
            threshold_bps: self.threshold_bps,
        }
    }
}

/// Frozen predecessor hash preimage.
///
/// The field names, types, and declaration order below are protocol identity.
/// Do not add, remove, rename, or reorder them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CanonicalDraftPreimage {
    pub(crate) action: String,
    pub(crate) amount_raw: String,
    pub(crate) asset: String,
    pub(crate) comparison: String,
    pub(crate) controlled_usdc_ata: String,
    pub(crate) controlled_wallet: String,
    pub(crate) display_source: String,
    pub(crate) expiry_seconds_after_finalize: u64,
    pub(crate) original_user_message_hash: String,
    pub(crate) protocol: String,
    pub(crate) threshold_bps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum AmountParseError {
    #[error("amount is empty")]
    Empty,
    #[error("amount must be non-negative")]
    Negative,
    #[error("amount contains non-decimal characters")]
    InvalidChar,
    #[error("amount has more than one decimal point")]
    MultipleDots,
    #[error("amount has more than 6 decimal places")]
    TooManyDecimals,
    #[error("amount overflows u64 when scaled to raw units")]
    Overflow,
    #[error("amount must be greater than zero")]
    Zero,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ProposeIntentError {
    #[error("{0}")]
    AmountFormat(#[from] AmountParseError),
    #[error("amount must be between 0.10 and 1.00 USDC")]
    AmountOutOfRange,
    #[error("threshold_bps must be in the inclusive range 1..=10000")]
    ThresholdOutOfRange,
    #[error("expiry_seconds_after_finalize must be exactly 180")]
    UnsupportedExpiry,
    #[error("controlled_wallet must be a valid Solana public key")]
    InvalidControlledWallet,
    #[error("controlled_usdc_ata must be a valid Solana public key")]
    InvalidControlledUsdcAta,
    #[error("original_user_message must not be empty")]
    EmptyOriginalUserMessage,
}

impl ProposeIntentError {
    fn code(&self) -> &'static str {
        match self {
            Self::AmountFormat(_) => "amount_format_invalid",
            Self::AmountOutOfRange => "amount_out_of_range",
            Self::ThresholdOutOfRange => "threshold_bps_out_of_range",
            Self::UnsupportedExpiry => "expiry_unsupported",
            Self::InvalidControlledWallet => "controlled_wallet_invalid",
            Self::InvalidControlledUsdcAta => "controlled_usdc_ata_invalid",
            Self::EmptyOriginalUserMessage => "original_user_message_empty",
        }
    }
}

pub(crate) fn propose_intent_json(params: &ProposeIntentParams) -> Value {
    match validate_draft(params) {
        Ok(draft) => {
            let draft_hash = compute_draft_hash(&draft.canonical_preimage());
            json!({
                "status": "ok",
                "draft_hash": draft_hash,
                "draft": draft,
                "persistence": {
                    "db_row_exists": false,
                    "note": "No DB row exists at this point",
                },
                "side_effects": {
                    "database_writes": 0,
                    "network_calls": 0,
                    "signatures": 0,
                },
            })
        }
        Err(error) => json!({
            "status": "invalid_input",
            "error_code": error.code(),
            "reason": error.to_string(),
            "persistence": {
                "db_row_exists": false,
                "note": "No DB row exists at this point",
            },
        }),
    }
}

fn validate_draft(params: &ProposeIntentParams) -> Result<DraftIntent, ProposeIntentError> {
    let amount_raw = parse_usdc_amount_to_raw(&params.amount)?;
    if !(MIN_AMOUNT_RAW..=MAX_AMOUNT_RAW).contains(&amount_raw) {
        return Err(ProposeIntentError::AmountOutOfRange);
    }
    if !(1..=MAX_THRESHOLD_BPS).contains(&params.threshold_bps) {
        return Err(ProposeIntentError::ThresholdOutOfRange);
    }
    if params.expiry_seconds_after_finalize != EXPIRY_SECONDS_AFTER_FINALIZE {
        return Err(ProposeIntentError::UnsupportedExpiry);
    }
    Pubkey::from_str(&params.controlled_wallet)
        .map_err(|_| ProposeIntentError::InvalidControlledWallet)?;
    Pubkey::from_str(&params.controlled_usdc_ata)
        .map_err(|_| ProposeIntentError::InvalidControlledUsdcAta)?;
    if params.original_user_message.trim().is_empty() {
        return Err(ProposeIntentError::EmptyOriginalUserMessage);
    }

    Ok(DraftIntent {
        action: params.action.as_str(),
        protocol: params.protocol.as_str(),
        asset: params.asset.as_str(),
        display_source: params.display_source.as_str(),
        comparison: params.comparison.as_str(),
        threshold_bps: params.threshold_bps,
        amount_raw,
        expiry_seconds_after_finalize: params.expiry_seconds_after_finalize,
        controlled_wallet: params.controlled_wallet.clone(),
        controlled_usdc_ata: params.controlled_usdc_ata.clone(),
        original_user_message_hash: sha256_hex(&params.original_user_message),
    })
}

/// Exact predecessor hash algorithm: compact sorted JSON, UTF-8, SHA-256,
/// lowercase hex.
pub(crate) fn compute_draft_hash(preimage: &CanonicalDraftPreimage) -> String {
    let mut fields: BTreeMap<&'static str, serde_json::Value> = BTreeMap::new();
    fields.insert("action", serde_json::Value::String(preimage.action.clone()));
    fields.insert(
        "amount_raw",
        serde_json::Value::String(preimage.amount_raw.clone()),
    );
    fields.insert("asset", serde_json::Value::String(preimage.asset.clone()));
    fields.insert(
        "comparison",
        serde_json::Value::String(preimage.comparison.clone()),
    );
    fields.insert(
        "controlled_usdc_ata",
        serde_json::Value::String(preimage.controlled_usdc_ata.clone()),
    );
    fields.insert(
        "controlled_wallet",
        serde_json::Value::String(preimage.controlled_wallet.clone()),
    );
    fields.insert(
        "display_source",
        serde_json::Value::String(preimage.display_source.clone()),
    );
    fields.insert(
        "expiry_seconds_after_finalize",
        serde_json::Value::Number(preimage.expiry_seconds_after_finalize.into()),
    );
    fields.insert(
        "original_user_message_hash",
        serde_json::Value::String(preimage.original_user_message_hash.clone()),
    );
    fields.insert(
        "protocol",
        serde_json::Value::String(preimage.protocol.clone()),
    );
    fields.insert(
        "threshold_bps",
        serde_json::Value::Number(preimage.threshold_bps.into()),
    );

    let bytes = serde_json::to_vec(&fields).expect("BTreeMap of JSON values must serialize");
    hex_lower_32(sha256(&bytes).to_bytes())
}

fn sha256_hex(value: &str) -> String {
    hex_lower_32(sha256(value.as_bytes()).to_bytes())
}

fn hex_lower_32(bytes: [u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

/// Exact decimal-to-raw USDC conversion from the predecessor. This parser
/// intentionally accepts `.5`, `1.`, whitespace, and leading zeroes; the
/// supported Phase 2 amount band is enforced separately.
pub(crate) fn parse_usdc_amount_to_raw(value: &str) -> Result<u64, AmountParseError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AmountParseError::Empty);
    }
    if value.starts_with('-') {
        return Err(AmountParseError::Negative);
    }
    if value.starts_with('+') {
        return Err(AmountParseError::InvalidChar);
    }

    let mut parts = value.split('.');
    let integer_part = parts.next().unwrap_or("");
    let fractional_part = parts.next();
    if parts.next().is_some() {
        return Err(AmountParseError::MultipleDots);
    }
    if !integer_part
        .chars()
        .all(|character| character.is_ascii_digit())
    {
        return Err(AmountParseError::InvalidChar);
    }
    if let Some(fractional_part) = fractional_part {
        if !fractional_part
            .chars()
            .all(|character| character.is_ascii_digit())
        {
            return Err(AmountParseError::InvalidChar);
        }
        if fractional_part.len() > USDC_DECIMALS as usize {
            return Err(AmountParseError::TooManyDecimals);
        }
    }

    let integer_value: u64 = if integer_part.is_empty() {
        0
    } else {
        integer_part
            .parse()
            .map_err(|_| AmountParseError::Overflow)?
    };
    let fractional_part = fractional_part.unwrap_or("");
    let mut padded_fraction = String::with_capacity(USDC_DECIMALS as usize);
    padded_fraction.push_str(fractional_part);
    while padded_fraction.len() < USDC_DECIMALS as usize {
        padded_fraction.push('0');
    }
    let fractional_value: u64 = padded_fraction
        .parse()
        .map_err(|_| AmountParseError::Overflow)?;
    let raw = integer_value
        .checked_mul(USDC_RAW_PER_WHOLE)
        .and_then(|scaled| scaled.checked_add(fractional_value))
        .ok_or(AmountParseError::Overflow)?;
    if raw == 0 {
        return Err(AmountParseError::Zero);
    }
    Ok(raw)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const CONTROLLED_WALLET: &str = "BPfDMmeMBmCbMC1rWh7hwigMBoKGBrKwXxSeUu9hhs5L";
    const CONTROLLED_USDC_ATA: &str = "7LFdKcSV7JQYi3or5y9phHVPjhGigu5DDjUakAFbBmk3";
    const GOLDEN_CANONICAL_JSON: &str = concat!(
        r#"{"action":"deposit","amount_raw":"500000","asset":"USDC","#,
        r#""comparison":"gt","controlled_usdc_ata":"#,
        r#""7LFdKcSV7JQYi3or5y9phHVPjhGigu5DDjUakAFbBmk3","#,
        r#""controlled_wallet":"#,
        r#""BPfDMmeMBmCbMC1rWh7hwigMBoKGBrKwXxSeUu9hhs5L","#,
        r#""display_source":"save","expiry_seconds_after_finalize":180,"#,
        r#""original_user_message_hash":"#,
        r#""0000000000000000000000000000000000000000000000000000000000000000","#,
        r#""protocol":"solend","threshold_bps":50}"#
    );
    const GOLDEN_DRAFT_HASH: &str =
        "be4f4017348ed4b2e9d49b5a02f89f74903f831f593129cdec56c56b18ba43f2";

    pub(crate) fn valid_params() -> ProposeIntentParams {
        ProposeIntentParams {
            action: DraftActionParam::Deposit,
            protocol: DraftProtocolParam::Solend,
            asset: DraftAssetParam::Usdc,
            display_source: DraftDisplaySourceParam::Save,
            comparison: DraftComparisonParam::Gt,
            amount: "0.5".to_owned(),
            threshold_bps: 50,
            expiry_seconds_after_finalize: EXPIRY_SECONDS_AFTER_FINALIZE,
            controlled_wallet: CONTROLLED_WALLET.to_owned(),
            controlled_usdc_ata: CONTROLLED_USDC_ATA.to_owned(),
            original_user_message: "If Save APY > 0.5%, deposit 0.5 USDC".to_owned(),
        }
    }

    fn golden_preimage() -> CanonicalDraftPreimage {
        CanonicalDraftPreimage {
            action: "deposit".to_owned(),
            amount_raw: "500000".to_owned(),
            asset: "USDC".to_owned(),
            comparison: "gt".to_owned(),
            controlled_usdc_ata: CONTROLLED_USDC_ATA.to_owned(),
            controlled_wallet: CONTROLLED_WALLET.to_owned(),
            display_source: "save".to_owned(),
            expiry_seconds_after_finalize: 180,
            original_user_message_hash: "0".repeat(64),
            protocol: "solend".to_owned(),
            threshold_bps: 50,
        }
    }

    #[test]
    fn predecessor_golden_preimage_and_hash_are_exact() {
        let preimage = golden_preimage();

        assert_eq!(
            serde_json::to_string(&preimage).expect("serialize golden preimage"),
            GOLDEN_CANONICAL_JSON
        );
        assert_eq!(GOLDEN_CANONICAL_JSON.len(), 406);
        assert_eq!(compute_draft_hash(&preimage), GOLDEN_DRAFT_HASH);
    }

    #[test]
    fn identical_typed_input_produces_identical_hash() {
        let params = valid_params();

        let first = propose_intent_json(&params);
        let second = propose_intent_json(&params);

        assert_eq!(first["status"], "ok");
        assert_eq!(first["draft_hash"], second["draft_hash"]);
        assert_eq!(first["draft_hash"].as_str().expect("draft hash").len(), 64);
        assert_eq!(first["draft"]["amount_raw"], 500_000);
        assert_eq!(
            first["persistence"]["note"],
            "No DB row exists at this point"
        );
        assert_eq!(first["persistence"]["db_row_exists"], false);
        assert_eq!(first["side_effects"]["database_writes"], 0);
        assert_eq!(first["side_effects"]["network_calls"], 0);
        assert_eq!(first["side_effects"]["signatures"], 0);
        assert!(
            !first.to_string().contains(&params.original_user_message),
            "raw user message must not be returned"
        );
    }

    #[test]
    fn invalid_amount_formats_are_rejected_by_category() {
        for amount in ["", "-0.5", "+0.5", "abc", "1.2.3", "0.1234567", "0"] {
            let mut params = valid_params();
            params.amount = amount.to_owned();

            let response = propose_intent_json(&params);

            assert_eq!(response["status"], "invalid_input", "amount={amount:?}");
            assert_eq!(
                response["error_code"], "amount_format_invalid",
                "amount={amount:?}"
            );
            assert_eq!(response["persistence"]["db_row_exists"], false);
        }
    }

    #[test]
    fn amounts_outside_supported_band_are_rejected() {
        for amount in ["0.09", "1.01"] {
            let mut params = valid_params();
            params.amount = amount.to_owned();

            let response = propose_intent_json(&params);

            assert_eq!(response["status"], "invalid_input", "amount={amount}");
            assert_eq!(response["error_code"], "amount_out_of_range");
        }
    }

    #[test]
    fn threshold_outside_inclusive_bounds_is_rejected() {
        for threshold_bps in [0, MAX_THRESHOLD_BPS + 1] {
            let mut params = valid_params();
            params.threshold_bps = threshold_bps;

            let response = propose_intent_json(&params);

            assert_eq!(
                response["status"], "invalid_input",
                "threshold_bps={threshold_bps}"
            );
            assert_eq!(response["error_code"], "threshold_bps_out_of_range");
        }
    }

    #[test]
    fn expiry_must_match_frozen_post_finalize_window() {
        for expiry_seconds_after_finalize in [0, 179, 181] {
            let mut params = valid_params();
            params.expiry_seconds_after_finalize = expiry_seconds_after_finalize;

            let response = propose_intent_json(&params);

            assert_eq!(
                response["status"], "invalid_input",
                "expiry={expiry_seconds_after_finalize}"
            );
            assert_eq!(response["error_code"], "expiry_unsupported");
        }
    }

    #[test]
    fn predecessor_decimal_parser_edge_cases_remain_compatible() {
        assert_eq!(parse_usdc_amount_to_raw(".5"), Ok(500_000));
        assert_eq!(parse_usdc_amount_to_raw("1."), Ok(1_000_000));
        assert_eq!(parse_usdc_amount_to_raw(" 0.123456 "), Ok(123_456));
        assert_eq!(
            parse_usdc_amount_to_raw("0.1234567"),
            Err(AmountParseError::TooManyDecimals)
        );
    }

    #[test]
    fn pure_calculation_module_imports_no_known_side_effect_capability() {
        const SOURCE: &str = include_str!("propose.rs");
        let forbidden = [
            ["Data", "base"].concat(),
            ["Repository", "::"].concat(),
            ["req", "west"].concat(),
            ["Rpc", "Pool"].concat(),
            [".", "post("].concat(),
            [".", "send("].concat(),
            ["write", "_all("].concat(),
            ["fs", "::write("].concat(),
            ["sqlx", "::query"].concat(),
            ["Keypair", "::new("].concat(),
            ["send_", "transaction"].concat(),
            ["sign_", "transaction"].concat(),
            ["broadcast_", "transaction"].concat(),
        ];

        for needle in forbidden {
            assert!(
                !SOURCE.contains(&needle),
                "pure proposal calculation module must not contain `{needle}`"
            );
        }
    }
}
