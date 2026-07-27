//! Shared development-only state-store fixtures.
//!
//! This module is compiled by tests and the ignored `tests/dev_seed.rs`
//! development entry point. It is not linked into the release MCP server.

use claw_state_store::{
    Database, NewW5hFundingIntent, Stage2W5hFundingIntentRepository, StoreError, W5hFundingIntent,
    W5hIntentStatus,
};

pub const DEV_INTENT_ID: &str = "11223344556677889900aabbccddeeff";
pub const DEV_INTENT_UUID: &str = "11223344-5566-7788-9900-aabbccddeeff";
pub const DEV_CANONICAL_HASH: &str =
    "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
pub const DEV_AMOUNT_RAW: u64 = 250_000;
pub const DEV_CREATED_AT_MS: i64 = 1_700_000_000_000;
pub const DEV_EXPIRES_AT_MS: i64 = 1_700_000_180_000;

pub fn funding_intent_fixture() -> NewW5hFundingIntent {
    NewW5hFundingIntent {
        intent_id: DEV_INTENT_ID.to_string(),
        rule_id_hex: DEV_INTENT_ID.to_string(),
        canonical_rule_hash_hex: DEV_CANONICAL_HASH.to_string(),
        user_wallet: "C4QQjzWxnJ5QFAbkzhQJ3wTzyX6nw1vyFvJwbPXJGPNW".to_string(),
        user_usdc_ata: "TestUserUsdcAta1111111111111111111111111111".to_string(),
        controlled_wallet: "BPfDMmeMBmCbMC1rWh7hwigMBoKGBrKwXxSeUu9hhs5L".to_string(),
        controlled_usdc_ata: "7LFdKcSV7JQYi3or5y9phHVPjhGigu5DDjUakAFbBmk3".to_string(),
        amount_raw: DEV_AMOUNT_RAW,
        threshold_bps: 100,
        save_display_apy_bps_at_creation: 210,
        native_onchain_apr_bps_at_creation: 165,
        created_at_ms: DEV_CREATED_AT_MS,
        expires_at_ms: DEV_EXPIRES_AT_MS,
    }
}

pub async fn seed_intent_status_fixture(db: &Database) -> Result<W5hFundingIntent, StoreError> {
    let repo = Stage2W5hFundingIntentRepository::new(db.pool().clone());
    if let Some(existing) = repo.get(DEV_INTENT_ID).await? {
        validate_existing_fixture(&existing)?;
        return Ok(existing);
    }

    repo.insert(&funding_intent_fixture()).await?;
    repo.get(DEV_INTENT_ID)
        .await?
        .ok_or_else(|| StoreError::NotFound {
            entity: "stage2_w5h_funding_intent".to_string(),
            id: DEV_INTENT_ID.to_string(),
        })
}

fn validate_existing_fixture(intent: &W5hFundingIntent) -> Result<(), StoreError> {
    let expected = funding_intent_fixture();
    let matches_fixture = intent.intent_id == expected.intent_id
        && intent.rule_id_hex == expected.rule_id_hex
        && intent.canonical_rule_hash_hex == expected.canonical_rule_hash_hex
        && intent.user_wallet == expected.user_wallet
        && intent.user_usdc_ata == expected.user_usdc_ata
        && intent.controlled_wallet == expected.controlled_wallet
        && intent.controlled_usdc_ata == expected.controlled_usdc_ata
        && intent.amount_raw == expected.amount_raw
        && intent.threshold_bps == expected.threshold_bps
        && intent.save_display_apy_bps_at_creation == expected.save_display_apy_bps_at_creation
        && intent.native_onchain_apr_bps_at_creation == expected.native_onchain_apr_bps_at_creation
        && intent.created_at_ms == expected.created_at_ms
        && intent.expires_at_ms == expected.expires_at_ms
        && intent.status == W5hIntentStatus::FundingRequired
        && intent.funding_signature.is_none()
        && intent.funding_finalized_slot.is_none()
        && intent.execution_signature.is_none()
        && intent.refund_signature.is_none()
        && intent.last_error.is_none();

    if matches_fixture {
        Ok(())
    } else {
        Err(StoreError::IntegrityCheckFailed(format!(
            "development seed id {DEV_INTENT_ID} already exists with unexpected data; \
             use a fresh database"
        )))
    }
}
