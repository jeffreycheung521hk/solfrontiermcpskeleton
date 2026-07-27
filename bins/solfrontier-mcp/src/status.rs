use claw_state_store::{
    Stage2W5hFundingIntentRepository, Stage2WatchRuleRepository, StoreError, StoredWatchRule,
    W5hFundingIntent,
};
use claw_types::ActionSpec;
use serde::Serialize;

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
    CanonicalHash,
    InvalidFormat,
}

pub(crate) async fn query_intent_status(
    funding_intents: &Stage2W5hFundingIntentRepository,
    watch_rules: &Stage2WatchRuleRepository,
    intent_ref: &str,
) -> Result<IntentStatusResponse, StoreError> {
    let resolved = match resolve_intent_ref(intent_ref) {
        Ok(resolved) => resolved,
        Err(reason) => return Ok(unsupported_response(intent_ref, reason)),
    };

    let funding_intent = funding_intents.get(&resolved.intent_id).await?;
    let watch_rule = watch_rules.get(&resolved.rule_id).await?;

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
        UnsupportedIntentRef::CanonicalHash => {
            "canonical hash lookup is not exposed by the state-store public API; \
             use the intent UUID (hyphenated or 32-character hex)"
        }
        UnsupportedIntentRef::InvalidFormat => {
            "unsupported intent_ref format; use the intent UUID (hyphenated or \
             32-character hex). Canonical hash lookup is unavailable through the \
             current state-store public API"
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

fn resolve_intent_ref(intent_ref: &str) -> Result<ResolvedIntentRef, UnsupportedIntentRef> {
    let candidate = intent_ref.trim();
    if candidate.len() == 64 && candidate.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(UnsupportedIntentRef::CanonicalHash);
    }

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
