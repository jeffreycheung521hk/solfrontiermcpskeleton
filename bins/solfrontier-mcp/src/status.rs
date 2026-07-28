use claw_state_store::{
    Stage2W5hFundingIntentRepository, Stage2WatchRuleRepository, StoreError, StoredWatchRule,
    W5hFundingIntent,
};
use claw_types::{canonical_rule_hash, ActionSpec};
use serde::Serialize;

use crate::{
    finalize::hex_lower,
    sidecar::{HashLookup, IntentSidecar},
};

#[derive(Debug, Serialize)]
pub(crate) struct IntentStatusResponse {
    pub(crate) intent_ref: String,
    pub(crate) status: String,
    pub(crate) is_terminal: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) resolved_intent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) canonical_rule_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) watch_rule_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) amount_raw: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) input_mint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) output_mint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) created_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) expires_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) updated_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) created_at_slot: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) expires_at_slot: Option<u64>,
}

#[derive(Debug)]
struct ResolvedIntentRef {
    intent_id: String,
    rule_id: [u8; 16],
}

#[derive(Debug)]
enum UnsupportedIntentRef {
    CanonicalHashUnavailable,
    CanonicalHashInconsistent,
    InvalidFormat,
}

pub(crate) async fn query_intent_status(
    funding_intents: &Stage2W5hFundingIntentRepository,
    watch_rules: &Stage2WatchRuleRepository,
    sidecar: &IntentSidecar,
    intent_ref: &str,
) -> Result<IntentStatusResponse, StoreError> {
    let candidate = intent_ref.trim();
    let canonical_hash =
        if candidate.len() == 64 && candidate.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Some(candidate.to_ascii_lowercase())
        } else {
            None
        };

    let resolved = if let Some(hash) = canonical_hash.as_deref() {
        match sidecar.lookup_hash(hash).await {
            HashLookup::Found { intent_id } => match resolve_uuid_ref(&intent_id) {
                Ok(resolved) => resolved,
                Err(_) => {
                    return Ok(unsupported_response(
                        intent_ref,
                        UnsupportedIntentRef::CanonicalHashInconsistent,
                    ));
                }
            },
            HashLookup::Missing | HashLookup::Unavailable => {
                return Ok(unsupported_response(
                    intent_ref,
                    UnsupportedIntentRef::CanonicalHashUnavailable,
                ));
            }
        }
    } else {
        match resolve_uuid_ref(intent_ref) {
            Ok(resolved) => resolved,
            Err(reason) => return Ok(unsupported_response(intent_ref, reason)),
        }
    };

    let funding_intent = funding_intents.get(&resolved.intent_id).await?;
    let watch_rule = watch_rules.get(&resolved.rule_id).await?;

    if let Some(hash) = canonical_hash.as_deref() {
        if !canonical_mapping_matches_main_store(
            hash,
            &resolved,
            funding_intent.as_ref(),
            watch_rule.as_ref(),
        ) {
            return Ok(unsupported_response(
                intent_ref,
                UnsupportedIntentRef::CanonicalHashInconsistent,
            ));
        }
    }

    Ok(project_status(
        intent_ref,
        resolved,
        funding_intent,
        watch_rule,
    ))
}

fn project_status(
    intent_ref: &str,
    resolved: ResolvedIntentRef,
    funding_intent: Option<W5hFundingIntent>,
    watch_rule: Option<StoredWatchRule>,
) -> IntentStatusResponse {
    match funding_intent {
        Some(intent) => funding_response(intent_ref, resolved, intent, watch_rule.as_ref()),
        None => match watch_rule {
            Some(rule) => watch_rule_response(intent_ref, resolved, rule),
            None => not_found_response(intent_ref, resolved),
        },
    }
}

fn funding_response(
    intent_ref: &str,
    resolved: ResolvedIntentRef,
    intent: W5hFundingIntent,
    watch_rule: Option<&StoredWatchRule>,
) -> IntentStatusResponse {
    let (input_mint, output_mint) = watch_rule.map(action_mints).unwrap_or((None, None));

    IntentStatusResponse {
        intent_ref: intent_ref.to_string(),
        status: intent.status.as_str().to_string(),
        is_terminal: intent.status.is_terminal(),
        message: None,
        source: Some("w5h_funding_intent"),
        resolved_intent_id: Some(resolved.intent_id),
        canonical_rule_hash: Some(intent.canonical_rule_hash_hex),
        watch_rule_status: watch_rule.map(|rule| rule.status.as_str().to_string()),
        amount_raw: Some(intent.amount_raw),
        input_mint,
        output_mint,
        created_at_ms: Some(intent.created_at_ms),
        expires_at_ms: Some(intent.expires_at_ms),
        updated_at_ms: Some(intent.updated_at_ms),
        created_at_slot: watch_rule.map(|rule| rule.rule.created_at_slot),
        expires_at_slot: watch_rule.map(|rule| rule.rule.expires_at_slot),
    }
}

fn watch_rule_response(
    intent_ref: &str,
    resolved: ResolvedIntentRef,
    rule: StoredWatchRule,
) -> IntentStatusResponse {
    let (input_mint, output_mint) = action_mints(&rule);
    let status = rule.status.as_str();

    IntentStatusResponse {
        intent_ref: intent_ref.to_string(),
        status: status.to_string(),
        is_terminal: matches!(status, "completed" | "expired" | "revoked" | "failed"),
        message: Some(
            "watch rule exists, but no W5h funding intent is stored for this UUID".to_string(),
        ),
        source: Some("watch_rule"),
        resolved_intent_id: Some(resolved.intent_id),
        canonical_rule_hash: Some(hex_encode(&rule.canonical_rule_hash)),
        watch_rule_status: Some(status.to_string()),
        amount_raw: Some(rule.rule.max_input_amount_raw),
        input_mint,
        output_mint,
        created_at_ms: Some(rule.created_at_ms),
        expires_at_ms: None,
        updated_at_ms: Some(rule.updated_at_ms),
        created_at_slot: Some(rule.rule.created_at_slot),
        expires_at_slot: Some(rule.rule.expires_at_slot),
    }
}

fn not_found_response(intent_ref: &str, resolved: ResolvedIntentRef) -> IntentStatusResponse {
    IntentStatusResponse {
        intent_ref: intent_ref.to_string(),
        status: "not_found".to_string(),
        is_terminal: false,
        message: None,
        source: None,
        resolved_intent_id: Some(resolved.intent_id),
        canonical_rule_hash: None,
        watch_rule_status: None,
        amount_raw: None,
        input_mint: None,
        output_mint: None,
        created_at_ms: None,
        expires_at_ms: None,
        updated_at_ms: None,
        created_at_slot: None,
        expires_at_slot: None,
    }
}

fn unsupported_response(intent_ref: &str, reason: UnsupportedIntentRef) -> IntentStatusResponse {
    let message = match reason {
        UnsupportedIntentRef::CanonicalHashUnavailable => {
            "canonical hash lookup is unavailable because the derived MCP sidecar \
             is missing, unreadable, or has no mapping; use the intent UUID"
        }
        UnsupportedIntentRef::CanonicalHashInconsistent => {
            "canonical hash lookup was rejected because the derived mapping did \
             not revalidate against the authoritative state-store repositories; \
             use the intent UUID"
        }
        UnsupportedIntentRef::InvalidFormat => {
            "unsupported intent_ref format; use the intent UUID (hyphenated or \
             32-character hex) or a 64-character canonical rule hash"
        }
    };

    IntentStatusResponse {
        intent_ref: intent_ref.to_string(),
        status: "unsupported_ref".to_string(),
        is_terminal: false,
        message: Some(message.to_string()),
        source: None,
        resolved_intent_id: None,
        canonical_rule_hash: None,
        watch_rule_status: None,
        amount_raw: None,
        input_mint: None,
        output_mint: None,
        created_at_ms: None,
        expires_at_ms: None,
        updated_at_ms: None,
        created_at_slot: None,
        expires_at_slot: None,
    }
}

fn resolve_uuid_ref(intent_ref: &str) -> Result<ResolvedIntentRef, UnsupportedIntentRef> {
    let candidate = intent_ref.trim();
    let compact = match candidate.len() {
        32 if candidate.bytes().all(|byte| byte.is_ascii_hexdigit()) => candidate.to_string(),
        36 if has_uuid_hyphens(candidate) => candidate
            .bytes()
            .filter(|byte| *byte != b'-')
            .map(char::from)
            .collect(),
        _ => return Err(UnsupportedIntentRef::InvalidFormat),
    };

    if !compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(UnsupportedIntentRef::InvalidFormat);
    }

    let mut rule_id = [0_u8; 16];
    for (index, byte) in rule_id.iter_mut().enumerate() {
        let offset = index * 2;
        *byte = (hex_nibble(compact.as_bytes()[offset])? << 4)
            | hex_nibble(compact.as_bytes()[offset + 1])?;
    }

    Ok(ResolvedIntentRef {
        intent_id: compact.to_ascii_lowercase(),
        rule_id,
    })
}

fn canonical_mapping_matches_main_store(
    expected_hash: &str,
    resolved: &ResolvedIntentRef,
    funding_intent: Option<&W5hFundingIntent>,
    watch_rule: Option<&StoredWatchRule>,
) -> bool {
    let Some(watch_rule) = watch_rule else {
        // A funding-only legacy orphan has no authoritative WatchRule bytes
        // from which the canonical hash can be recomputed.
        return false;
    };

    if watch_rule.rule.rule_id != resolved.rule_id
        || hex_lower(&watch_rule.rule.rule_id) != resolved.intent_id
        || hex_lower(&watch_rule.canonical_rule_hash) != expected_hash
        || hex_lower(&canonical_rule_hash(&watch_rule.rule)) != expected_hash
    {
        return false;
    }

    funding_intent.is_none_or(|intent| {
        intent.intent_id == resolved.intent_id
            && intent.rule_id_hex == resolved.intent_id
            && intent
                .canonical_rule_hash_hex
                .eq_ignore_ascii_case(expected_hash)
    })
}

fn has_uuid_hyphens(candidate: &str) -> bool {
    candidate
        .bytes()
        .enumerate()
        .all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn hex_nibble(byte: u8) -> Result<u8, UnsupportedIntentRef> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(UnsupportedIntentRef::InvalidFormat),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn action_mints(rule: &StoredWatchRule) -> (Option<String>, Option<String>) {
    match &rule.rule.action {
        ActionSpec::JupiterBuySolWithUsdc {
            input_mint,
            output_mint,
            ..
        } => (Some(input_mint.to_base58()), Some(output_mint.to_base58())),
        ActionSpec::SolendWithdrawAllDelegated { .. } => (None, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dev_seed::{
        seed_intent_status_fixture, DEV_AMOUNT_RAW, DEV_CANONICAL_HASH, DEV_CREATED_AT_MS,
        DEV_EXPIRES_AT_MS, DEV_INTENT_ID, DEV_INTENT_UUID,
    };
    use claw_state_store::{Database, DatabaseConfig, W5hIntentStatus};
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static TEST_DB_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TempDatabaseFile {
        path: PathBuf,
    }

    impl TempDatabaseFile {
        fn unique() -> Self {
            let sequence = TEST_DB_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time must be after the Unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "solfrontier-mcp-status-{}-{timestamp}-{sequence}.db",
                std::process::id()
            ));
            Self { path }
        }

        fn config(&self) -> DatabaseConfig {
            DatabaseConfig {
                path: self.path.to_string_lossy().into_owned(),
                max_connections: 1,
            }
        }
    }

    impl Drop for TempDatabaseFile {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let candidate = PathBuf::from(format!("{}{suffix}", self.path.display()));
                let _ = fs::remove_file(candidate);
            }
        }
    }

    async fn test_store() -> (
        TempDatabaseFile,
        Database,
        Stage2W5hFundingIntentRepository,
        Stage2WatchRuleRepository,
    ) {
        let file = TempDatabaseFile::unique();
        let db = Database::open(&file.config())
            .await
            .expect("temporary SQLite database must open");
        let funding_intents = Stage2W5hFundingIntentRepository::new(db.pool().clone());
        let watch_rules = Stage2WatchRuleRepository::new(db.pool().clone());
        (file, db, funding_intents, watch_rules)
    }

    async fn close_test_store(
        db: Database,
        funding_intents: Stage2W5hFundingIntentRepository,
        watch_rules: Stage2WatchRuleRepository,
    ) {
        drop(funding_intents);
        drop(watch_rules);
        db.pool().close().await;
    }

    #[tokio::test]
    async fn stored_funding_intent_returns_repository_status_fields() {
        let (file, db, funding_intents, watch_rules) = test_store().await;
        seed_intent_status_fixture(&db)
            .await
            .expect("fixture seed must succeed");
        let sidecar = IntentSidecar::for_database_path(&file.path);

        let response =
            query_intent_status(&funding_intents, &watch_rules, &sidecar, DEV_INTENT_UUID)
                .await
                .expect("status lookup must succeed");

        assert_eq!(response.intent_ref, DEV_INTENT_UUID);
        assert_eq!(response.status, W5hIntentStatus::FundingRequired.as_str());
        assert!(!response.is_terminal);
        assert_eq!(response.source, Some("w5h_funding_intent"));
        assert_eq!(response.resolved_intent_id.as_deref(), Some(DEV_INTENT_ID));
        assert_eq!(
            response.canonical_rule_hash.as_deref(),
            Some(DEV_CANONICAL_HASH)
        );
        assert_eq!(response.amount_raw, Some(DEV_AMOUNT_RAW));
        assert_eq!(response.created_at_ms, Some(DEV_CREATED_AT_MS));
        assert_eq!(response.expires_at_ms, Some(DEV_EXPIRES_AT_MS));
        assert!(response.updated_at_ms.is_some());
        assert!(response.input_mint.is_none());
        assert!(response.output_mint.is_none());

        close_test_store(db, funding_intents, watch_rules).await;
    }

    #[tokio::test]
    async fn missing_intent_returns_not_found_without_protocol_error() {
        let (file, db, funding_intents, watch_rules) = test_store().await;
        let sidecar = IntentSidecar::for_database_path(&file.path);

        let response = query_intent_status(&funding_intents, &watch_rules, &sidecar, DEV_INTENT_ID)
            .await
            .expect("missing rows are a normal query result");

        assert_eq!(response.intent_ref, DEV_INTENT_ID);
        assert_eq!(response.status, "not_found");
        assert!(!response.is_terminal);
        assert_eq!(response.resolved_intent_id.as_deref(), Some(DEV_INTENT_ID));
        assert!(response.source.is_none());
        assert!(response.amount_raw.is_none());

        close_test_store(db, funding_intents, watch_rules).await;
    }

    #[tokio::test]
    async fn canonical_hash_without_sidecar_mapping_is_unsupported() {
        let (file, db, funding_intents, watch_rules) = test_store().await;
        let sidecar = IntentSidecar::for_database_path(&file.path);
        let response =
            query_intent_status(&funding_intents, &watch_rules, &sidecar, DEV_CANONICAL_HASH)
                .await
                .expect("missing sidecar is a normal result");

        assert_eq!(response.intent_ref, DEV_CANONICAL_HASH);
        assert_eq!(response.status, "unsupported_ref");
        assert!(!response.is_terminal);
        assert!(response
            .message
            .as_deref()
            .is_some_and(|message| message.contains("sidecar")));

        close_test_store(db, funding_intents, watch_rules).await;
    }
}
