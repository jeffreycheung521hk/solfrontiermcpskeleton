//! Execute-mode orchestration after a complete dry-run-equivalent revalidation.
//!
//! This module is intentionally narrow: it converts an already validated
//! report into a canonical execution artifact, acquires the funding CAS lease,
//! invokes the wallet-engine review/sign adapter, submits once, and maps the
//! sanitized network result onto durable state. Scanning and chain
//! revalidation stay in `watch`; key handling stays in `watch_wallet`.

use std::collections::BTreeSet;

use async_trait::async_trait;
use claw_executor::ValidatedSolendPlanInputs;
use claw_state_store::{Stage2W5hFundingIntentRepository, Stage2WatchRuleRepository};
use claw_types::session::SessionId;
use serde::Serialize;
use solana_sdk::{pubkey::Pubkey, transaction::Transaction};

use crate::{
    watch::{
        build_unsigned_transaction, emit_json_report, report_for_fresh_unsigned_transaction,
        CandidateInspection, UnsignedTransactionReport, WatchCycleReport,
    },
    watch_execution_state::{
        ExecutionStateAdapter, ExecutionStatePort, FinalizedWriteOutcome, LeaseOutcome,
        StateWriteOutcome,
    },
    watch_submission::{submit_once_and_observe, NetworkOutcome, ReviewedSignedPayload},
    watch_wallet::{CanonicalExecutionPolicy, WatchWalletPipeline},
};

const TRANSACTION_REVIEW_SERIALIZATION_FAILED: &str =
    "execute_transaction_review_serialization_failed_before_signing";
const TRANSACTION_REVIEW_INTEGRITY_FAILED: &str =
    "execute_transaction_review_integrity_failed_before_signing";

/// A fresh, dry-run-equivalent validation result ready for the CAS boundary.
#[derive(Debug)]
pub(crate) struct RevalidatedExecutionCandidate {
    pub(crate) intent_id: String,
    pub(crate) rule_id: [u8; 16],
    pub(crate) amount_raw: u64,
    pub(crate) plan: ValidatedSolendPlanInputs,
}

/// A fixed-class failure while interpreting an internal validation report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CandidateArtifactError {
    ScanUnsafe,
    DuplicateReadyIntent,
    ReadyRowMissing,
    ReadyRowMalformed,
}

impl CandidateArtifactError {
    pub(crate) const fn class(self) -> &'static str {
        match self {
            Self::ScanUnsafe => "execute_scan_unsafe",
            Self::DuplicateReadyIntent => "duplicate_ready_intent",
            Self::ReadyRowMissing => "revalidated_candidate_not_ready",
            Self::ReadyRowMalformed => "revalidated_candidate_malformed",
        }
    }
}

/// Return ready intent ids only when the entire scan is complete.
///
/// Any `*_scan_failed` or `*_scan_truncated` flag aborts execute mode before
/// lease/sign. Dry-run callers do not use this gate and retain their existing
/// partial-report behavior.
pub(crate) fn execute_ready_intent_ids(
    report: &WatchCycleReport,
) -> Result<Vec<String>, CandidateArtifactError> {
    if !report.scan_errors.is_empty() {
        return Err(CandidateArtifactError::ScanUnsafe);
    }
    let mut seen = BTreeSet::new();
    let mut ids = Vec::new();
    for row in report.rows.iter().filter(|row| row.status == "ready") {
        let intent_id = row
            .intent_id
            .as_ref()
            .ok_or(CandidateArtifactError::ReadyRowMalformed)?;
        if !seen.insert(intent_id.clone()) {
            return Err(CandidateArtifactError::DuplicateReadyIntent);
        }
        ids.push(intent_id.clone());
    }
    Ok(ids)
}

/// Extract one exact candidate from a fresh target-specific inspection.
pub(crate) fn revalidated_candidate(
    inspection: &CandidateInspection,
    expected_intent_id: &str,
) -> Result<RevalidatedExecutionCandidate, CandidateArtifactError> {
    use claw_executor::CandidateClassification;

    let row = &inspection.row;
    if row.status != "ready" || row.error_class.is_some() {
        return Err(CandidateArtifactError::ReadyRowMissing);
    }
    let intent_id = row
        .intent_id
        .as_ref()
        .ok_or(CandidateArtifactError::ReadyRowMalformed)?;
    let rule_id_hex = row
        .rule_id_hex
        .as_ref()
        .ok_or(CandidateArtifactError::ReadyRowMalformed)?;
    if intent_id != rule_id_hex {
        return Err(CandidateArtifactError::ReadyRowMalformed);
    }
    if intent_id != expected_intent_id {
        return Err(CandidateArtifactError::ReadyRowMissing);
    }
    let rule_id =
        exact_lower_hex_rule_id(rule_id_hex).ok_or(CandidateArtifactError::ReadyRowMalformed)?;

    let candidate = row
        .candidate
        .as_ref()
        .ok_or(CandidateArtifactError::ReadyRowMalformed)?;
    if candidate.classification != CandidateClassification::Ready
        || candidate.intent_id.as_deref() != Some(intent_id)
        || candidate.rule_id_hex.as_deref() != Some(rule_id_hex)
        || candidate.clocks.wall_clock_eligible != Some(true)
        || candidate.clocks.slot_clock_eligible != Some(true)
    {
        return Err(CandidateArtifactError::ReadyRowMalformed);
    }
    let plan = inspection
        .plan
        .as_ref()
        .ok_or(CandidateArtifactError::ReadyRowMalformed)?;
    if &plan.report != candidate {
        return Err(CandidateArtifactError::ReadyRowMalformed);
    }
    let amounts = candidate
        .amounts
        .as_ref()
        .ok_or(CandidateArtifactError::ReadyRowMalformed)?;
    if !amounts.all_equal_and_nonzero {
        return Err(CandidateArtifactError::ReadyRowMalformed);
    }
    let amount_raw = plan.input_amount_raw;
    let amount_text = amount_raw.to_string();
    if amount_raw == 0
        || amounts.action_input_amount_raw.as_deref() != Some(amount_text.as_str())
        || amounts.rule_max_input_amount_raw.as_deref() != Some(amount_text.as_str())
        || amounts.funding_amount_raw.as_deref() != Some(amount_text.as_str())
    {
        return Err(CandidateArtifactError::ReadyRowMalformed);
    }

    Ok(RevalidatedExecutionCandidate {
        intent_id: intent_id.clone(),
        rule_id,
        amount_raw,
        plan: plan.clone(),
    })
}

fn exact_lower_hex_rule_id(value: &str) -> Option<[u8; 16]> {
    if value.len() != 32
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    hex::decode(value).ok()?.try_into().ok()
}

#[async_trait]
pub(crate) trait CandidateExecutionBackend: Send + Sync {
    async fn review_and_sign(
        &self,
        candidate: &RevalidatedExecutionCandidate,
    ) -> Result<ReviewedSignedPayload, &'static str>;

    async fn submit_and_observe(&self, payload: &ReviewedSignedPayload) -> NetworkOutcome;
}

pub(crate) struct ProductionExecutionBackend {
    wallet: WatchWalletPipeline,
    rpc_pool: claw_solana_core::RpcPool,
}

impl ProductionExecutionBackend {
    pub(crate) fn new(wallet: WatchWalletPipeline, rpc_pool: claw_solana_core::RpcPool) -> Self {
        Self { wallet, rpc_pool }
    }
}

/// Build the leased candidate's unsigned transaction and make its complete
/// dry-run-format report visible before returning any transaction that can be
/// passed to the signing pipeline. A reporting failure returns no transaction,
/// so the caller cannot sign after a missing audit record.
fn transaction_after_visible_review<F>(
    candidate: &RevalidatedExecutionCandidate,
    emit: F,
) -> Result<(Transaction, UnsignedTransactionReport), &'static str>
where
    F: FnOnce(&UnsignedTransactionReport) -> Result<(), &'static str>,
{
    let (unsigned_transaction, unsigned_report) = build_unsigned_transaction(&candidate.plan)?;
    emit(&unsigned_report)?;
    Ok((unsigned_transaction, unsigned_report))
}

#[async_trait]
impl CandidateExecutionBackend for ProductionExecutionBackend {
    async fn review_and_sign(
        &self,
        candidate: &RevalidatedExecutionCandidate,
    ) -> Result<ReviewedSignedPayload, &'static str> {
        // The state-store contract requires the funding lease to be acquired
        // before transaction construction. This is the only production call
        // site, and the orchestrator invokes it strictly after a successful
        // CAS.
        let (unsigned_transaction, placeholder_report) = transaction_after_visible_review(
            candidate,
            |report| {
                eprintln!(
                "!!! EXECUTE TRANSACTION REVIEW: leased intent_id={} amount_raw={}; canonical placeholder-blockhash shape follows, then an exact fresh-blockhash report is required before policy/signing !!!",
                candidate.intent_id, candidate.amount_raw
            );
                emit_json_report(report, TRANSACTION_REVIEW_SERIALIZATION_FAILED)
            },
        )?;
        let expected_message_bytes = unsigned_transaction.message.serialize();
        let canonical_policy = CanonicalExecutionPolicy {
            solend_program_id: to_pubkey(candidate.plan.solend_program_id),
            input_mint: to_pubkey(candidate.plan.input_mint),
            input_amount_raw: candidate.plan.input_amount_raw,
            source_liquidity: to_pubkey(candidate.plan.source_liquidity),
            reserve_liquidity_supply: to_pubkey(candidate.plan.reserve_liquidity_supply),
            expected_message_bytes: expected_message_bytes.clone(),
        };
        let reviewed = self
            .wallet
            .review_and_sign(
                &candidate.intent_id,
                SessionId::new(),
                unsigned_transaction,
                canonical_policy,
                move |fresh_unsigned| {
                    let fresh_report = report_for_fresh_unsigned_transaction(
                        placeholder_report,
                        &expected_message_bytes,
                        fresh_unsigned,
                    )
                    .map_err(|error| match error {
                        "fresh_unsigned_transaction_drifted" => {
                            crate::watch_wallet::WatchWalletError::AuditIntegrityFailed
                        }
                        _ => crate::watch_wallet::WatchWalletError::AuditOutputFailed,
                    })?;
                    eprintln!(
                        "!!! EXECUTE EXACT SIGNING REVIEW: fresh blockhash attached; the following unsigned transaction is the exact message entering policy/signing !!!"
                    );
                    emit_json_report(&fresh_report, TRANSACTION_REVIEW_SERIALIZATION_FAILED)
                        .map_err(|_| crate::watch_wallet::WatchWalletError::AuditOutputFailed)
                },
            )
            .await
            .map_err(|error| error.class())?;
        let bytes = reviewed
            .submission_bytes()
            .map_err(|error| error.class())?
            .to_vec();
        Ok(ReviewedSignedPayload {
            signed_transaction_bytes: bytes,
            signature: reviewed.signature(),
            last_valid_block_height: reviewed.last_valid_block_height(),
        })
    }

    async fn submit_and_observe(&self, payload: &ReviewedSignedPayload) -> NetworkOutcome {
        submit_once_and_observe(&self.rpc_pool, payload).await
    }
}

fn to_pubkey(value: claw_types::canonical_intent::PubkeyBytes) -> Pubkey {
    Pubkey::new_from_array(*value.as_bytes())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ExecutionAttemptReport {
    pub(crate) intent_id: String,
    pub(crate) amount_raw: String,
    pub(crate) status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) finalized_slot: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error_class: Option<&'static str>,
    pub(crate) funding_state: &'static str,
    pub(crate) watch_rule_state: &'static str,
    /// A missing human-audit record is cycle-fatal even though its lease was
    /// safely released before broadcast.
    pub(crate) abort_remaining_cycle: bool,
}

/// A cycle-fatal audit failure must prevent every later ready candidate from
/// reaching lease or signing in the same scan result.
pub(crate) const fn execution_attempt_aborts_cycle(report: &ExecutionAttemptReport) -> bool {
    report.abort_remaining_cycle
}

/// Run one candidate after its fresh report has been parsed.
///
/// The CAS lease is always the first mutation. The caller must invoke this
/// immediately after final revalidation; no transaction work may happen
/// between that revalidation and this function.
pub(crate) async fn execute_revalidated_candidate<S, B, F>(
    state: &ExecutionStateAdapter<S>,
    backend: &B,
    candidate: RevalidatedExecutionCandidate,
    lease_clock: F,
) -> ExecutionAttemptReport
where
    S: ExecutionStatePort,
    B: CandidateExecutionBackend,
    F: FnOnce() -> i64,
{
    let base = || ExecutionAttemptReport {
        intent_id: candidate.intent_id.clone(),
        amount_raw: candidate.amount_raw.to_string(),
        status: "internal",
        signature: None,
        finalized_slot: None,
        error_class: None,
        funding_state: "budget_reserved",
        watch_rule_state: "unchanged",
        abort_remaining_cycle: false,
    };

    // Sample after the operator-visible candidate warning and immediately
    // before CAS so the persisted inclusive expiry predicate is authoritative.
    let lease_now_ms = lease_clock();
    match state
        .lease_after_final_revalidation(&candidate.intent_id, lease_now_ms)
        .await
    {
        LeaseOutcome::Acquired => {}
        LeaseOutcome::NotAcquired { error_class }
        | LeaseOutcome::StoreUnavailable { error_class } => {
            return ExecutionAttemptReport {
                status: "lease_not_acquired",
                error_class: Some(error_class),
                funding_state: "unknown_or_not_budget_reserved",
                ..base()
            };
        }
    }

    let payload = match backend.review_and_sign(&candidate).await {
        Ok(payload) => payload,
        Err(error_class) => {
            let abort_remaining_cycle = matches!(
                error_class,
                TRANSACTION_REVIEW_SERIALIZATION_FAILED | TRANSACTION_REVIEW_INTEGRITY_FAILED
            );
            return report_release(
                state,
                &candidate,
                "prebroadcast_failed",
                error_class,
                abort_remaining_cycle,
                base(),
            )
            .await;
        }
    };

    match backend.submit_and_observe(&payload).await {
        NetworkOutcome::PreBroadcastFailure { error_class } => {
            report_release(
                state,
                &candidate,
                "prebroadcast_failed",
                error_class,
                false,
                base(),
            )
            .await
        }
        NetworkOutcome::Unknown {
            signature,
            error_class,
        } => {
            let _ = state.leave_broadcast_unknown(&candidate.intent_id);
            ExecutionAttemptReport {
                status: "result_unknown_manual_reconciliation_required",
                signature: Some(signature),
                error_class: Some(error_class),
                funding_state: "executing",
                ..base()
            }
        }
        NetworkOutcome::OnChainFailed {
            signature,
            error_class,
        } => {
            let write = state.mark_onchain_failed(&candidate.intent_id).await;
            let (status, write_error) =
                state_write_report(write, "failed", "onchain_failure_state_write_failed");
            ExecutionAttemptReport {
                status,
                signature: Some(signature),
                error_class: write_error.or(Some(error_class)),
                funding_state: if write_error.is_none() {
                    "failed"
                } else {
                    "executing_or_failed_unknown"
                },
                ..base()
            }
        }
        NetworkOutcome::Finalized { signature, slot } => {
            let write = state
                .mark_finalized(
                    &candidate.intent_id,
                    &candidate.rule_id,
                    candidate.amount_raw,
                    slot,
                    &signature,
                )
                .await;
            finalized_report(base(), signature, slot, write)
        }
    }
}

/// Parse a fresh validation report and cross the lease boundary only if the
/// exact target row remains fully eligible. This is the production entrypoint
/// used by `watch --execute`; keeping parse and lease in one function makes it
/// impossible for a malformed/expired final report to reach a lease through a
/// caller ordering mistake.
pub(crate) async fn execute_from_final_revalidation<S, B, F>(
    state: &ExecutionStateAdapter<S>,
    backend: &B,
    inspection: &CandidateInspection,
    expected_intent_id: &str,
    lease_clock: F,
) -> Result<ExecutionAttemptReport, CandidateArtifactError>
where
    S: ExecutionStatePort,
    B: CandidateExecutionBackend,
    F: FnOnce() -> i64,
{
    let candidate = revalidated_candidate(inspection, expected_intent_id)?;
    let controlled_wallet = to_pubkey(candidate.plan.obligation_owner);
    eprintln!(
        "!!! EXECUTE CANDIDATE: intent_id={} amount_raw={} controlled_wallet={} !!!",
        candidate.intent_id, candidate.amount_raw, controlled_wallet
    );
    Ok(execute_revalidated_candidate(state, backend, candidate, lease_clock).await)
}

async fn report_release<S: ExecutionStatePort>(
    state: &ExecutionStateAdapter<S>,
    candidate: &RevalidatedExecutionCandidate,
    success_status: &'static str,
    original_error: &'static str,
    abort_remaining_cycle: bool,
    base: ExecutionAttemptReport,
) -> ExecutionAttemptReport {
    let release = state
        .release_prebroadcast_failure(&candidate.intent_id)
        .await;
    let (status, write_error) =
        state_write_report(release, success_status, "prebroadcast_release_failed");
    ExecutionAttemptReport {
        status,
        error_class: write_error.or(Some(original_error)),
        funding_state: if write_error.is_none() {
            "budget_reserved"
        } else {
            "executing_or_budget_reserved_unknown"
        },
        abort_remaining_cycle,
        ..base
    }
}

fn state_write_report(
    outcome: StateWriteOutcome,
    applied_status: &'static str,
    failure_status: &'static str,
) -> (&'static str, Option<&'static str>) {
    match outcome {
        StateWriteOutcome::Applied => (applied_status, None),
        StateWriteOutcome::NotApplied { error_class }
        | StateWriteOutcome::StoreUnavailable { error_class } => {
            (failure_status, Some(error_class))
        }
    }
}

fn finalized_report(
    base: ExecutionAttemptReport,
    signature: String,
    slot: u64,
    outcome: FinalizedWriteOutcome,
) -> ExecutionAttemptReport {
    match outcome {
        FinalizedWriteOutcome::Completed => ExecutionAttemptReport {
            status: "completed",
            signature: Some(signature),
            finalized_slot: Some(slot),
            funding_state: "completed",
            watch_rule_state: "completed",
            ..base
        },
        FinalizedWriteOutcome::WatchRuleNotCompleted { error_class }
        | FinalizedWriteOutcome::WatchRuleStoreUnavailable { error_class } => {
            ExecutionAttemptReport {
                status: "finalized_state_write_failed",
                signature: Some(signature),
                finalized_slot: Some(slot),
                error_class: Some(error_class),
                funding_state: "executing",
                watch_rule_state: "unchanged_or_unknown",
                ..base
            }
        }
        FinalizedWriteOutcome::FundingNotCompletedAfterRule {
            error_class,
            watch_rule_completed: true,
        }
        | FinalizedWriteOutcome::FundingStoreUnavailableAfterRule {
            error_class,
            watch_rule_completed: true,
        } => ExecutionAttemptReport {
            status: "completed_split_state_manual_reconciliation_required",
            signature: Some(signature),
            finalized_slot: Some(slot),
            error_class: Some(error_class),
            funding_state: "executing_or_completed_unknown",
            watch_rule_state: "completed",
            ..base
        },
        FinalizedWriteOutcome::FundingNotCompletedAfterRule {
            watch_rule_completed: false,
            ..
        }
        | FinalizedWriteOutcome::FundingStoreUnavailableAfterRule {
            watch_rule_completed: false,
            ..
        } => ExecutionAttemptReport {
            status: "finalized_state_invariant_broken",
            signature: Some(signature),
            finalized_slot: Some(slot),
            error_class: Some("watch_rule_completion_evidence_missing"),
            funding_state: "executing_or_completed_unknown",
            watch_rule_state: "unknown",
            ..base
        },
    }
}

/// Convenience constructor used by the binary after opening the writable DB.
pub(crate) fn repository_state_adapter(
    funding: Stage2W5hFundingIntentRepository,
    rules: Stage2WatchRuleRepository,
) -> ExecutionStateAdapter<crate::watch_execution_state::RepositoryExecutionState> {
    ExecutionStateAdapter::new(crate::watch_execution_state::RepositoryExecutionState::new(
        funding, rules,
    ))
}

#[cfg(test)]
#[path = "watch_execute_tests.rs"]
mod tests;
