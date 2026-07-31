use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use base64::Engine as _;
use claw_executor::{
    AmountReport, CandidateClassification, CandidateReport, ClockReport, ValidatedSolendPlanInputs,
};
use claw_state_store::{
    Database, NewW5hFundingIntent, Stage2W5hFundingIntentRepository, Stage2WatchRuleRepository,
    W5hIntentStatus, WatchRuleStatus,
};
use claw_types::{
    canonical_intent::PubkeyBytes,
    canonical_rule_hash,
    stage2_watch_rule::{
        ActionSpec, Comparison, Condition, ConditionLogic, RateKind, WatchRule,
        STAGE2_WATCH_RULE_SCHEMA_V2,
    },
};
use solana_sdk::{pubkey::Pubkey, signature::Signature};

use super::*;
use crate::{
    watch::{CandidateInspection, WatchCycleReport, WatchRowReport},
    watch_execution_state::{
        ExecutionStateAdapter, ExecutionStatePort, LeaseOutcome, StateStoreUnavailable,
    },
    watch_submission::{NetworkOutcome, ReviewedSignedPayload},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FundingLifecycle {
    BudgetReserved,
    Executing,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StateCall {
    Lease,
    Release,
    FailFunding,
    CompleteRule,
    CompleteFunding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrderedCall {
    Lease,
    ReviewAndSign,
    SubmitAndObserve,
}

type SharedCallOrder = Arc<Mutex<Vec<OrderedCall>>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeaseBehavior {
    Normal,
    NotAcquired,
    StoreUnavailable,
}

#[derive(Debug)]
struct MockExecutionState {
    lifecycle: Mutex<FundingLifecycle>,
    calls: Mutex<Vec<StateCall>>,
    call_order: SharedCallOrder,
    lease_behavior: LeaseBehavior,
}

impl Default for MockExecutionState {
    fn default() -> Self {
        Self {
            lifecycle: Mutex::new(FundingLifecycle::BudgetReserved),
            calls: Mutex::new(Vec::new()),
            call_order: Arc::new(Mutex::new(Vec::new())),
            lease_behavior: LeaseBehavior::Normal,
        }
    }
}

impl MockExecutionState {
    fn with_lease_behavior(lease_behavior: LeaseBehavior) -> Self {
        Self {
            lease_behavior,
            ..Self::default()
        }
    }

    fn lifecycle(&self) -> FundingLifecycle {
        *self.lifecycle.lock().expect("mock lifecycle lock")
    }

    fn calls(&self) -> Vec<StateCall> {
        self.calls.lock().expect("mock calls lock").clone()
    }

    fn record(&self, call: StateCall) {
        self.calls.lock().expect("mock calls lock").push(call);
    }

    fn shared_call_order(&self) -> SharedCallOrder {
        Arc::clone(&self.call_order)
    }

    fn ordered_calls(&self) -> Vec<OrderedCall> {
        self.call_order
            .lock()
            .expect("shared call-order lock")
            .clone()
    }
}

impl ExecutionStatePort for MockExecutionState {
    async fn lease_funding(
        &self,
        _intent_id: &str,
        _now_ms: i64,
    ) -> Result<u64, StateStoreUnavailable> {
        self.record(StateCall::Lease);
        self.call_order
            .lock()
            .expect("shared call-order lock")
            .push(OrderedCall::Lease);
        match self.lease_behavior {
            LeaseBehavior::NotAcquired => return Ok(0),
            LeaseBehavior::StoreUnavailable => return Err(StateStoreUnavailable),
            LeaseBehavior::Normal => {}
        }
        let mut lifecycle = self.lifecycle.lock().expect("mock lifecycle lock");
        if *lifecycle == FundingLifecycle::BudgetReserved {
            *lifecycle = FundingLifecycle::Executing;
            Ok(1)
        } else {
            Ok(0)
        }
    }

    async fn release_funding(
        &self,
        _intent_id: &str,
        _reason: &str,
    ) -> Result<u64, StateStoreUnavailable> {
        self.record(StateCall::Release);
        let mut lifecycle = self.lifecycle.lock().expect("mock lifecycle lock");
        if *lifecycle == FundingLifecycle::Executing {
            *lifecycle = FundingLifecycle::BudgetReserved;
            Ok(1)
        } else {
            Ok(0)
        }
    }

    async fn fail_funding(
        &self,
        _intent_id: &str,
        _reason: &str,
    ) -> Result<u64, StateStoreUnavailable> {
        self.record(StateCall::FailFunding);
        let mut lifecycle = self.lifecycle.lock().expect("mock lifecycle lock");
        if *lifecycle == FundingLifecycle::Executing {
            *lifecycle = FundingLifecycle::Failed;
            Ok(1)
        } else {
            Ok(0)
        }
    }

    async fn complete_watch_rule(
        &self,
        _rule_id: &[u8; 16],
        _used_amount_raw: u64,
        _finalized_slot: u64,
    ) -> Result<u64, StateStoreUnavailable> {
        self.record(StateCall::CompleteRule);
        Ok(1)
    }

    async fn complete_funding(
        &self,
        _intent_id: &str,
        _execution_signature: &str,
    ) -> Result<u64, StateStoreUnavailable> {
        self.record(StateCall::CompleteFunding);
        let mut lifecycle = self.lifecycle.lock().expect("mock lifecycle lock");
        if *lifecycle == FundingLifecycle::Executing {
            *lifecycle = FundingLifecycle::Completed;
            Ok(1)
        } else {
            Ok(0)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BackendCall {
    ReviewAndSign,
    SubmitAndObserve,
}

#[derive(Debug, Clone)]
enum BackendPlan {
    ReviewFailure(&'static str),
    Network(NetworkOutcome),
}

#[derive(Debug)]
struct MockBackend {
    plan: BackendPlan,
    calls: Mutex<Vec<BackendCall>>,
    call_order: SharedCallOrder,
}

impl MockBackend {
    fn new(plan: BackendPlan) -> Self {
        Self::with_call_order(plan, Arc::new(Mutex::new(Vec::new())))
    }

    fn with_call_order(plan: BackendPlan, call_order: SharedCallOrder) -> Self {
        Self {
            plan,
            calls: Mutex::new(Vec::new()),
            call_order,
        }
    }

    fn calls(&self) -> Vec<BackendCall> {
        self.calls.lock().expect("backend calls lock").clone()
    }
}

#[async_trait]
impl CandidateExecutionBackend for MockBackend {
    async fn review_and_sign(
        &self,
        _candidate: &RevalidatedExecutionCandidate,
    ) -> Result<ReviewedSignedPayload, &'static str> {
        self.calls
            .lock()
            .expect("backend calls lock")
            .push(BackendCall::ReviewAndSign);
        self.call_order
            .lock()
            .expect("shared call-order lock")
            .push(OrderedCall::ReviewAndSign);
        match &self.plan {
            BackendPlan::ReviewFailure(error_class) => Err(error_class),
            BackendPlan::Network(_) => Ok(ReviewedSignedPayload {
                signed_transaction_bytes: vec![1, 2, 3],
                signature: Signature::new_unique(),
                last_valid_block_height: 999,
            }),
        }
    }

    async fn submit_and_observe(&self, _payload: &ReviewedSignedPayload) -> NetworkOutcome {
        self.calls
            .lock()
            .expect("backend calls lock")
            .push(BackendCall::SubmitAndObserve);
        self.call_order
            .lock()
            .expect("shared call-order lock")
            .push(OrderedCall::SubmitAndObserve);
        match &self.plan {
            BackendPlan::Network(outcome) => outcome.clone(),
            BackendPlan::ReviewFailure(_) => {
                panic!("submit must not follow a review failure")
            }
        }
    }
}

fn fixture_pubkey() -> PubkeyBytes {
    PubkeyBytes::new(Pubkey::new_unique().to_bytes())
}

fn ready_candidate_report() -> CandidateReport {
    let id = "07070707070707070707070707070707".to_owned();
    CandidateReport {
        classification: CandidateClassification::Ready,
        intent_id: Some(id.clone()),
        rule_id_hex: Some(id),
        findings: Vec::new(),
        clocks: ClockReport {
            now_ms: 100,
            funding_expires_at_ms: Some(200),
            wall_clock_eligible: Some(true),
            current_confirmed_slot: Some(10),
            rule_expires_at_slot: Some(20),
            slot_clock_eligible: Some(true),
            reserve_last_update_slot: Some(9),
            reserve_age_slots: Some(1),
            max_reserve_staleness_slots: Some(16),
        },
        amounts: Some(AmountReport {
            action_input_amount_raw: Some("200000".to_owned()),
            rule_max_input_amount_raw: Some("200000".to_owned()),
            funding_amount_raw: Some("200000".to_owned()),
            all_equal_and_nonzero: true,
        }),
        condition: None,
    }
}

fn validated_plan() -> ValidatedSolendPlanInputs {
    ValidatedSolendPlanInputs {
        solend_program_id: fixture_pubkey(),
        input_mint: fixture_pubkey(),
        input_amount_raw: 200_000,
        source_liquidity: fixture_pubkey(),
        user_collateral: fixture_pubkey(),
        reserve: fixture_pubkey(),
        reserve_liquidity_supply: fixture_pubkey(),
        reserve_collateral_mint: fixture_pubkey(),
        lending_market: fixture_pubkey(),
        destination_deposit_collateral: fixture_pubkey(),
        obligation: fixture_pubkey(),
        obligation_owner: fixture_pubkey(),
        pyth_oracle: fixture_pubkey(),
        switchboard_oracle: fixture_pubkey(),
        user_transfer_authority: fixture_pubkey(),
        token_program: fixture_pubkey(),
        report: ready_candidate_report(),
    }
}

fn execution_candidate() -> RevalidatedExecutionCandidate {
    RevalidatedExecutionCandidate {
        intent_id: "07070707070707070707070707070707".to_owned(),
        rule_id: [7_u8; 16],
        amount_raw: 200_000,
        plan: validated_plan(),
    }
}

#[test]
fn complete_transaction_report_is_emitted_before_transaction_can_reach_signing() {
    let candidate = execution_candidate();
    let mut order = Vec::new();
    let (transaction, placeholder_report) =
        transaction_after_visible_review(&candidate, |report| {
            order.push("full_transaction_report");
            let json = serde_json::to_value(report).expect("serialize review report");
            let instructions = json["instructions"].as_array().expect("instruction array");
            assert_eq!(instructions.len(), 4);
            assert_eq!(instructions[0]["label"], "set_compute_unit_limit");
            assert_eq!(instructions[1]["label"], "set_compute_unit_price");
            assert_eq!(instructions[2]["label"], "refresh_reserve");
            assert_eq!(
                instructions[3]["label"],
                "deposit_reserve_liquidity_and_obligation_collateral"
            );
            assert_eq!(
                instructions[3]["accounts"]
                    .as_array()
                    .expect("deposit account metas")
                    .len(),
                14
            );
            for instruction in instructions {
                assert!(instruction["program_id"].is_string());
                assert!(instruction["data_hex"].is_string());
                for account in instruction["accounts"]
                    .as_array()
                    .expect("instruction account metas")
                {
                    assert!(account["pubkey"].is_string());
                    assert!(account["is_signer"].is_boolean());
                    assert!(account["is_writable"].is_boolean());
                }
            }
            Ok(())
        })
        .expect("report permits transaction handoff");
    order.push("signing_can_begin");

    assert_eq!(order, vec!["full_transaction_report", "signing_can_begin"]);
    assert_eq!(transaction.message.recent_blockhash, Default::default());

    let mut fresh = transaction.clone();
    fresh.message.recent_blockhash = solana_sdk::hash::Hash::new_unique();
    let exact = crate::watch::report_for_fresh_unsigned_transaction(
        placeholder_report.clone(),
        &transaction.message.serialize(),
        &fresh,
    )
    .expect("fresh blockhash is the only permitted message change");
    assert_eq!(
        exact.kind,
        "fresh_unsigned_transaction_immediately_before_signing"
    );
    let fresh_blockhash = fresh.message.recent_blockhash.to_string();
    assert_eq!(
        exact.recent_blockhash.as_deref(),
        Some(fresh_blockhash.as_str())
    );
    let exact_bytes = base64::engine::general_purpose::STANDARD
        .decode(exact.transaction_base64)
        .expect("exact review base64");
    let exact_transaction: solana_sdk::transaction::Transaction =
        bincode::deserialize(&exact_bytes).expect("exact review transaction");
    assert_eq!(exact_transaction, fresh);

    let mut drifted = fresh;
    drifted.message.instructions[3].data[1] ^= 1;
    assert_eq!(
        crate::watch::report_for_fresh_unsigned_transaction(
            placeholder_report,
            &transaction.message.serialize(),
            &drifted,
        )
        .expect_err("instruction drift must block signing"),
        "fresh_unsigned_transaction_drifted"
    );

    assert_eq!(
        transaction_after_visible_review(&candidate, |_| {
            Err("review_report_serialization_failed")
        })
        .expect_err("missing report must withhold transaction"),
        "review_report_serialization_failed"
    );
}

#[tokio::test]
async fn prebroadcast_failure_releases_and_can_be_leased_again() {
    let mock_state = MockExecutionState::default();
    let shared_call_order = mock_state.shared_call_order();
    let state = ExecutionStateAdapter::new(mock_state);
    let backend = MockBackend::with_call_order(
        BackendPlan::ReviewFailure("simulation_failed"),
        shared_call_order,
    );

    let first =
        execute_revalidated_candidate(&state, &backend, execution_candidate(), || 100).await;
    assert_eq!(first.status, "prebroadcast_failed");
    assert_eq!(first.funding_state, "budget_reserved");
    assert_eq!(
        state.state_for_test().lifecycle(),
        FundingLifecycle::BudgetReserved
    );

    let second =
        execute_revalidated_candidate(&state, &backend, execution_candidate(), || 101).await;
    assert_eq!(second.status, "prebroadcast_failed");
    assert_eq!(
        state.state_for_test().lifecycle(),
        FundingLifecycle::BudgetReserved
    );
    assert_eq!(
        state.state_for_test().calls(),
        vec![
            StateCall::Lease,
            StateCall::Release,
            StateCall::Lease,
            StateCall::Release,
        ]
    );
    assert_eq!(
        state.state_for_test().ordered_calls(),
        vec![
            OrderedCall::Lease,
            OrderedCall::ReviewAndSign,
            OrderedCall::Lease,
            OrderedCall::ReviewAndSign,
        ],
        "each lease CAS must precede review/sign"
    );
}

#[tokio::test]
async fn missing_transaction_audit_report_releases_lease_and_aborts_remaining_cycle() {
    let state = ExecutionStateAdapter::new(MockExecutionState::default());
    let backend = MockBackend::new(BackendPlan::ReviewFailure(
        TRANSACTION_REVIEW_SERIALIZATION_FAILED,
    ));

    let report =
        execute_revalidated_candidate(&state, &backend, execution_candidate(), || 100).await;

    assert_eq!(report.status, "prebroadcast_failed");
    assert_eq!(
        report.error_class,
        Some(TRANSACTION_REVIEW_SERIALIZATION_FAILED)
    );
    assert!(report.abort_remaining_cycle);
    assert_eq!(report.funding_state, "budget_reserved");
    assert_eq!(
        state.state_for_test().calls(),
        vec![StateCall::Lease, StateCall::Release]
    );
}

#[tokio::test]
async fn fresh_transaction_integrity_failure_releases_and_aborts_the_cycle() {
    let state = ExecutionStateAdapter::new(MockExecutionState::default());
    let backend = MockBackend::new(BackendPlan::ReviewFailure(
        TRANSACTION_REVIEW_INTEGRITY_FAILED,
    ));

    let report =
        execute_revalidated_candidate(&state, &backend, execution_candidate(), || 100).await;

    assert_eq!(report.status, "prebroadcast_failed");
    assert_eq!(
        report.error_class,
        Some(TRANSACTION_REVIEW_INTEGRITY_FAILED)
    );
    assert!(report.abort_remaining_cycle);
    assert_eq!(report.funding_state, "budget_reserved");
    assert_eq!(
        state.state_for_test().calls(),
        vec![StateCall::Lease, StateCall::Release]
    );
}

#[tokio::test]
async fn audit_failure_prevents_every_later_candidate_in_the_cycle() {
    let state = ExecutionStateAdapter::new(MockExecutionState::default());
    let backend = MockBackend::new(BackendPlan::ReviewFailure(
        TRANSACTION_REVIEW_SERIALIZATION_FAILED,
    ));
    let mut attempted = Vec::new();

    for candidate_number in 1..=2 {
        attempted.push(candidate_number);
        let report =
            execute_revalidated_candidate(&state, &backend, execution_candidate(), || 100).await;
        if execution_attempt_aborts_cycle(&report) {
            break;
        }
    }

    assert_eq!(attempted, vec![1]);
    assert_eq!(
        state.state_for_test().calls(),
        vec![StateCall::Lease, StateCall::Release]
    );
    assert_eq!(backend.calls(), vec![BackendCall::ReviewAndSign]);
}

#[tokio::test]
async fn lease_zero_or_unavailable_never_reaches_backend() {
    for lease_behavior in [LeaseBehavior::NotAcquired, LeaseBehavior::StoreUnavailable] {
        let mock_state = MockExecutionState::with_lease_behavior(lease_behavior);
        let shared_call_order = mock_state.shared_call_order();
        let state = ExecutionStateAdapter::new(mock_state);
        let backend = MockBackend::with_call_order(
            BackendPlan::ReviewFailure("backend_must_not_be_reached"),
            shared_call_order,
        );

        let report =
            execute_revalidated_candidate(&state, &backend, execution_candidate(), || 100).await;

        assert_eq!(report.status, "lease_not_acquired");
        assert_eq!(
            report.funding_state, "unknown_or_not_budget_reserved",
            "lease result was {lease_behavior:?}"
        );
        assert_eq!(state.state_for_test().calls(), vec![StateCall::Lease]);
        assert_eq!(
            state.state_for_test().ordered_calls(),
            vec![OrderedCall::Lease]
        );
        assert!(
            backend.calls().is_empty(),
            "backend ran after lease result {lease_behavior:?}"
        );
    }
}

#[tokio::test]
async fn onchain_failure_marks_funding_failed_only() {
    let state = ExecutionStateAdapter::new(MockExecutionState::default());
    let backend = MockBackend::new(BackendPlan::Network(NetworkOutcome::OnChainFailed {
        signature: "signature-a".to_owned(),
        error_class: "transaction_failed_on_chain",
    }));

    let report =
        execute_revalidated_candidate(&state, &backend, execution_candidate(), || 100).await;

    assert_eq!(report.status, "failed");
    assert_eq!(report.funding_state, "failed");
    assert_eq!(report.watch_rule_state, "unchanged");
    assert_eq!(state.state_for_test().lifecycle(), FundingLifecycle::Failed);
    assert_eq!(
        state.state_for_test().calls(),
        vec![StateCall::Lease, StateCall::FailFunding]
    );
}

#[tokio::test]
async fn unknown_result_leaves_lease_and_never_rebroadcasts() {
    let state = ExecutionStateAdapter::new(MockExecutionState::default());
    let backend = MockBackend::new(BackendPlan::Network(NetworkOutcome::Unknown {
        signature: "signature-a".to_owned(),
        error_class: "finality_timeout",
    }));

    let first =
        execute_revalidated_candidate(&state, &backend, execution_candidate(), || 100).await;
    assert_eq!(
        first.status,
        "result_unknown_manual_reconciliation_required"
    );
    assert_eq!(first.funding_state, "executing");
    assert_eq!(
        state.state_for_test().lifecycle(),
        FundingLifecycle::Executing
    );

    let second =
        execute_revalidated_candidate(&state, &backend, execution_candidate(), || 101).await;
    assert_eq!(second.status, "lease_not_acquired");
    assert_eq!(second.funding_state, "unknown_or_not_budget_reserved");
    assert_eq!(
        state.state_for_test().calls(),
        vec![StateCall::Lease, StateCall::Lease]
    );
    assert_eq!(
        backend.calls(),
        vec![BackendCall::ReviewAndSign, BackendCall::SubmitAndObserve]
    );
}

#[tokio::test]
async fn confirmed_but_not_finalized_stays_executing_without_terminal_write() {
    let state = ExecutionStateAdapter::new(MockExecutionState::default());
    // `watch_submission` keeps confirmed observations pending. If its bounded
    // observation window ends before finalized, the durable layer receives an
    // Unknown result and must retain the execution lease.
    let backend = MockBackend::new(BackendPlan::Network(NetworkOutcome::Unknown {
        signature: "signature-confirmed".to_owned(),
        error_class: "confirmed_not_finalized",
    }));

    let report =
        execute_revalidated_candidate(&state, &backend, execution_candidate(), || 100).await;

    assert_eq!(
        report.status,
        "result_unknown_manual_reconciliation_required"
    );
    assert_eq!(report.funding_state, "executing");
    assert_eq!(report.watch_rule_state, "unchanged");
    assert_eq!(
        state.state_for_test().lifecycle(),
        FundingLifecycle::Executing
    );
    assert_eq!(state.state_for_test().calls(), vec![StateCall::Lease]);
}

#[tokio::test]
async fn finalized_records_watch_rule_then_funding() {
    let state = ExecutionStateAdapter::new(MockExecutionState::default());
    let backend = MockBackend::new(BackendPlan::Network(NetworkOutcome::Finalized {
        signature: "signature-a".to_owned(),
        slot: 555,
    }));

    let report =
        execute_revalidated_candidate(&state, &backend, execution_candidate(), || 100).await;

    assert_eq!(report.status, "completed");
    assert_eq!(report.finalized_slot, Some(555));
    assert_eq!(report.watch_rule_state, "completed");
    assert_eq!(report.funding_state, "completed");
    assert_eq!(
        state.state_for_test().lifecycle(),
        FundingLifecycle::Completed
    );
    assert_eq!(
        state.state_for_test().calls(),
        vec![
            StateCall::Lease,
            StateCall::CompleteRule,
            StateCall::CompleteFunding,
        ]
    );
}

fn minimal_ready_row() -> WatchRowReport {
    let id = "07070707070707070707070707070707".to_owned();
    WatchRowReport {
        status: "ready".to_owned(),
        intent_id: Some(id.clone()),
        rule_id_hex: Some(id),
        rule_lifecycle: None,
        candidate: Some(ready_candidate_report()),
        error_class: None,
        unsigned_transaction: None,
    }
}

fn inspection_with(row: WatchRowReport) -> CandidateInspection {
    let mut plan = validated_plan();
    if let Some(report) = row.candidate.clone() {
        plan.report = report;
    }
    CandidateInspection {
        row,
        plan: Some(plan),
    }
}

fn cycle_with(row: WatchRowReport) -> WatchCycleReport {
    WatchCycleReport {
        mode: "execute_revalidation",
        database_access: "fixture",
        current_confirmed_slot: Some(10),
        slot_error_class: None,
        rows: vec![row],
        summary: BTreeMap::new(),
        scan_errors: Vec::new(),
    }
}

#[tokio::test]
async fn every_scan_integrity_flag_blocks_before_state_or_backend_calls() {
    for scan_error in [
        "funding_scan_truncated",
        "funding_scan_failed",
        "watch_rule_scan_truncated",
        "watch_rule_scan_failed",
    ] {
        let mut report = cycle_with(minimal_ready_row());
        report.scan_errors.push(scan_error.to_owned());
        let state = ExecutionStateAdapter::new(MockExecutionState::default());
        let backend = MockBackend::new(BackendPlan::ReviewFailure(
            "backend_must_not_be_reached_after_scan_error",
        ));

        let gate = execute_ready_intent_ids(&report);
        if let Ok(intent_ids) = &gate {
            for _intent_id in intent_ids {
                let _ =
                    execute_revalidated_candidate(&state, &backend, execution_candidate(), || 100)
                        .await;
            }
        }

        assert_eq!(
            gate,
            Err(CandidateArtifactError::ScanUnsafe),
            "scan flag {scan_error} must abort the complete execute cycle"
        );
        assert!(
            state.state_for_test().calls().is_empty(),
            "scan flag {scan_error} reached the lease/state layer"
        );
        assert!(
            backend.calls().is_empty(),
            "scan flag {scan_error} reached review/sign/submit"
        );
    }
}

#[tokio::test]
async fn malformed_hash_amount_or_expiry_never_reaches_lease() {
    let state = ExecutionStateAdapter::new(MockExecutionState::default());
    let backend = MockBackend::new(BackendPlan::ReviewFailure("backend_must_not_be_reached"));

    let mut malformed_hash = minimal_ready_row();
    let uppercase = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned();
    malformed_hash.intent_id = Some(uppercase.clone());
    malformed_hash.rule_id_hex = Some(uppercase.clone());
    let candidate = malformed_hash
        .candidate
        .as_mut()
        .expect("candidate fixture");
    candidate.intent_id = Some(uppercase.clone());
    candidate.rule_id_hex = Some(uppercase);
    let malformed_hash_inspection = inspection_with(malformed_hash);
    assert!(matches!(
        execute_from_final_revalidation(
            &state,
            &backend,
            &malformed_hash_inspection,
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            || 100,
        )
        .await,
        Err(CandidateArtifactError::ReadyRowMalformed)
    ));

    let mut malformed_amount = minimal_ready_row();
    malformed_amount
        .candidate
        .as_mut()
        .expect("candidate fixture")
        .amounts
        .as_mut()
        .expect("amount fixture")
        .all_equal_and_nonzero = false;
    let malformed_amount_inspection = inspection_with(malformed_amount);
    assert!(matches!(
        execute_from_final_revalidation(
            &state,
            &backend,
            &malformed_amount_inspection,
            "07070707070707070707070707070707",
            || 100,
        )
        .await,
        Err(CandidateArtifactError::ReadyRowMalformed)
    ));

    let mut expired = minimal_ready_row();
    expired
        .candidate
        .as_mut()
        .expect("candidate fixture")
        .clocks
        .wall_clock_eligible = Some(false);
    let expired_inspection = inspection_with(expired);
    assert!(matches!(
        execute_from_final_revalidation(
            &state,
            &backend,
            &expired_inspection,
            "07070707070707070707070707070707",
            || 100,
        )
        .await,
        Err(CandidateArtifactError::ReadyRowMalformed)
    ));

    assert!(
        state.state_for_test().calls().is_empty(),
        "artifact rejection must happen before any lease call"
    );
    assert!(
        backend.calls().is_empty(),
        "artifact rejection must happen before review/sign/submit"
    );
}

fn funding_fixture(intent_id: &str) -> NewW5hFundingIntent {
    NewW5hFundingIntent {
        intent_id: intent_id.to_owned(),
        rule_id_hex: intent_id.to_owned(),
        canonical_rule_hash_hex: "0000000000000000000000000000000000000000000000000000000000000000"
            .to_owned(),
        user_wallet: Pubkey::new_unique().to_string(),
        user_usdc_ata: Pubkey::new_unique().to_string(),
        controlled_wallet: Pubkey::new_unique().to_string(),
        controlled_usdc_ata: Pubkey::new_unique().to_string(),
        amount_raw: 200_000,
        threshold_bps: 50,
        save_display_apy_bps_at_creation: 300,
        native_onchain_apr_bps_at_creation: 100,
        created_at_ms: 1_000,
        expires_at_ms: 10_000,
    }
}

fn repository_watch_rule_fixture() -> WatchRule {
    let controlled_wallet = fixture_pubkey();
    WatchRule {
        schema_version: STAGE2_WATCH_RULE_SCHEMA_V2,
        rule_id: [7_u8; 16],
        user: fixture_pubkey(),
        executor: controlled_wallet,
        delegated_wallet: controlled_wallet,
        created_at_slot: 1,
        expires_at_slot: 10_000,
        one_shot: true,
        condition_logic: ConditionLogic::All,
        conditions: vec![Condition::SolendReserveSupplyRate {
            reserve_pubkey: fixture_pubkey(),
            lending_market: fixture_pubkey(),
            solend_program_id: fixture_pubkey(),
            comparison: Comparison::Gt,
            threshold_bps: 50,
            rate_kind: RateKind::Apr,
            formula_version: 1,
            max_reserve_staleness_slots: 16,
            required_refresh_same_tx: true,
        }],
        action: ActionSpec::SolendDeposit {
            target_obligation: fixture_pubkey(),
            reserve_pubkey: fixture_pubkey(),
            lending_market: fixture_pubkey(),
            solend_program_id: fixture_pubkey(),
            input_mint: fixture_pubkey(),
            input_amount_raw: 200_000,
        },
        max_input_amount_raw: 200_000,
        used_amount_raw: 0,
        destination: controlled_wallet,
        slippage_bps: 0,
    }
}

async fn repository_execution_fixture() -> (
    Database,
    Stage2W5hFundingIntentRepository,
    Stage2WatchRuleRepository,
) {
    let db = Database::open_in_memory().await.expect("in-memory DB");
    let funding = Stage2W5hFundingIntentRepository::new(db.pool().clone());
    let rules = Stage2WatchRuleRepository::new(db.pool().clone());
    let rule = repository_watch_rule_fixture();
    rules.insert(&rule).await.expect("insert watch rule");

    let mut fixture = funding_fixture(&hex::encode(rule.rule_id));
    fixture.canonical_rule_hash_hex = hex::encode(canonical_rule_hash(&rule));
    fixture.controlled_wallet = rule.executor.to_base58();
    funding.insert(&fixture).await.expect("insert funding");
    assert_eq!(
        funding
            .mark_funding_submitted_if_required(&fixture.intent_id, "funding-signature")
            .await
            .expect("mark funding submitted"),
        1
    );
    assert_eq!(
        funding
            .mark_budget_reserved_if_submitted(&fixture.intent_id, "funding-signature", 42)
            .await
            .expect("mark budget reserved"),
        1
    );

    (db, funding, rules)
}

#[tokio::test]
async fn sqlite_prebroadcast_review_failure_releases_with_fixed_reason() {
    let (_db, funding, rules) = repository_execution_fixture().await;
    let state = repository_state_adapter(funding.clone(), rules.clone());
    let backend = MockBackend::new(BackendPlan::ReviewFailure("simulation_failed"));

    let report =
        execute_revalidated_candidate(&state, &backend, execution_candidate(), || 2_000).await;

    assert_eq!(report.status, "prebroadcast_failed");
    assert_eq!(report.funding_state, "budget_reserved");
    let stored = funding
        .get("07070707070707070707070707070707")
        .await
        .expect("read funding")
        .expect("funding row");
    assert_eq!(stored.status, W5hIntentStatus::BudgetReserved);
    assert_eq!(
        stored.last_error.as_deref(),
        Some("execution_prebroadcast_failed")
    );
    assert!(stored.execution_signature.is_none());

    let stored_rule = rules
        .get(&[7_u8; 16])
        .await
        .expect("read watch rule")
        .expect("watch rule row");
    assert_eq!(stored_rule.status, WatchRuleStatus::Active);
    assert!(!stored_rule.completed);

    assert_eq!(
        state
            .lease_after_final_revalidation("07070707070707070707070707070707", 2_001)
            .await,
        LeaseOutcome::Acquired,
        "the repository must make a definitely pre-broadcast failure retryable"
    );
    let releasable = funding
        .get("07070707070707070707070707070707")
        .await
        .expect("read re-leased funding")
        .expect("re-leased funding row");
    assert_eq!(releasable.status, W5hIntentStatus::Executing);
}

#[tokio::test]
async fn sqlite_confirmed_not_finalized_remains_executing_without_rule_completion() {
    let (_db, funding, rules) = repository_execution_fixture().await;
    let state = repository_state_adapter(funding.clone(), rules.clone());
    let backend = MockBackend::new(BackendPlan::Network(NetworkOutcome::Unknown {
        signature: "signature-confirmed".to_owned(),
        error_class: "finality_timeout",
    }));

    let report =
        execute_revalidated_candidate(&state, &backend, execution_candidate(), || 2_000).await;

    assert_eq!(
        report.status,
        "result_unknown_manual_reconciliation_required"
    );
    assert_eq!(report.funding_state, "executing");
    let stored = funding
        .get("07070707070707070707070707070707")
        .await
        .expect("read funding")
        .expect("funding row");
    assert_eq!(stored.status, W5hIntentStatus::Executing);
    assert!(stored.execution_signature.is_none());
    assert!(stored.last_error.is_none());

    let stored_rule = rules
        .get(&[7_u8; 16])
        .await
        .expect("read watch rule")
        .expect("watch rule row");
    assert_eq!(stored_rule.status, WatchRuleStatus::Active);
    assert!(!stored_rule.completed);
    assert!(stored_rule.last_checked_slot.is_none());

    let retry_backend = MockBackend::new(BackendPlan::ReviewFailure(
        "backend_must_not_be_reached_after_unknown",
    ));
    let retry =
        execute_revalidated_candidate(&state, &retry_backend, execution_candidate(), || 2_001)
            .await;
    assert_eq!(retry.status, "lease_not_acquired");
    assert_eq!(retry.funding_state, "unknown_or_not_budget_reserved");
    assert!(
        retry_backend.calls().is_empty(),
        "an executing row must not be reviewed, signed, or broadcast again"
    );
}

#[tokio::test]
async fn sqlite_finalized_error_is_terminal_without_completing_rule_or_releasing_lease() {
    let (_db, funding, rules) = repository_execution_fixture().await;
    let state = repository_state_adapter(funding.clone(), rules.clone());
    let backend = MockBackend::new(BackendPlan::Network(NetworkOutcome::OnChainFailed {
        signature: "signature-finalized-error".to_owned(),
        error_class: "transaction_failed_on_chain",
    }));

    let report =
        execute_revalidated_candidate(&state, &backend, execution_candidate(), || 2_000).await;

    assert_eq!(report.status, "failed");
    assert_eq!(report.funding_state, "failed");
    assert_eq!(report.watch_rule_state, "unchanged");
    let stored = funding
        .get("07070707070707070707070707070707")
        .await
        .expect("read failed funding")
        .expect("failed funding row");
    assert_eq!(stored.status, W5hIntentStatus::Failed);
    assert_eq!(
        stored.last_error.as_deref(),
        Some("execution_onchain_failed")
    );
    assert!(stored.execution_signature.is_none());

    let stored_rule = rules
        .get(&[7_u8; 16])
        .await
        .expect("read watch rule")
        .expect("watch rule row");
    assert_eq!(stored_rule.status, WatchRuleStatus::Active);
    assert!(!stored_rule.completed);
    assert!(stored_rule.last_checked_slot.is_none());

    let retry_backend = MockBackend::new(BackendPlan::ReviewFailure(
        "backend_must_not_be_reached_after_terminal_failure",
    ));
    let retry =
        execute_revalidated_candidate(&state, &retry_backend, execution_candidate(), || 2_001)
            .await;
    assert_eq!(retry.status, "lease_not_acquired");
    assert_eq!(retry.funding_state, "unknown_or_not_budget_reserved");
    assert!(
        retry_backend.calls().is_empty(),
        "a terminal failed row must never be reviewed, signed, or broadcast again"
    );
}

#[tokio::test]
async fn sqlite_finalized_persists_rule_bookkeeping_and_funding_signature() {
    let (_db, funding, rules) = repository_execution_fixture().await;
    let state = repository_state_adapter(funding.clone(), rules.clone());
    let backend = MockBackend::new(BackendPlan::Network(NetworkOutcome::Finalized {
        signature: "signature-finalized".to_owned(),
        slot: 555,
    }));

    let report =
        execute_revalidated_candidate(&state, &backend, execution_candidate(), || 2_000).await;

    assert_eq!(report.status, "completed");
    assert_eq!(report.finalized_slot, Some(555));
    let stored_rule = rules
        .get(&[7_u8; 16])
        .await
        .expect("read watch rule")
        .expect("watch rule row");
    assert_eq!(stored_rule.status, WatchRuleStatus::Completed);
    assert!(stored_rule.completed);
    assert_eq!(stored_rule.last_checked_slot, Some(555));

    // `used_amount_raw` is lifecycle bookkeeping stored separately from the
    // immutable canonical rule JSON, so assert the repository's backing row.
    let (used_amount_raw, last_checked_slot): (i64, Option<i64>) = sqlx::query_as(
        "SELECT used_amount_raw, last_checked_slot FROM stage2_watch_rules WHERE rule_id = ?",
    )
    .bind("07070707070707070707070707070707")
    .fetch_one(rules.pool())
    .await
    .expect("read watch-rule bookkeeping");
    assert_eq!(used_amount_raw, 200_000);
    assert_eq!(last_checked_slot, Some(555));

    let stored = funding
        .get("07070707070707070707070707070707")
        .await
        .expect("read funding")
        .expect("funding row");
    assert_eq!(stored.status, W5hIntentStatus::Completed);
    assert_eq!(
        stored.execution_signature.as_deref(),
        Some("signature-finalized")
    );
}

#[tokio::test]
async fn sqlite_repository_adapters_compete_for_exactly_one_lease() {
    let db = Database::open_in_memory().await.expect("in-memory DB");
    let funding = Stage2W5hFundingIntentRepository::new(db.pool().clone());
    let rules = Stage2WatchRuleRepository::new(db.pool().clone());
    let fixture = funding_fixture("07070707070707070707070707070707");
    funding.insert(&fixture).await.expect("insert funding");
    assert_eq!(
        funding
            .mark_funding_submitted_if_required(&fixture.intent_id, "funding-signature",)
            .await
            .expect("mark funding submitted"),
        1
    );
    assert_eq!(
        funding
            .mark_budget_reserved_if_submitted(&fixture.intent_id, "funding-signature", 42,)
            .await
            .expect("mark budget reserved"),
        1
    );

    let first = repository_state_adapter(funding.clone(), rules.clone());
    let second = repository_state_adapter(funding, rules);
    let (left, right) = tokio::join!(
        first.lease_after_final_revalidation(&fixture.intent_id, 2_000),
        second.lease_after_final_revalidation(&fixture.intent_id, 2_000),
    );

    let acquired = [left, right]
        .into_iter()
        .filter(|outcome| *outcome == LeaseOutcome::Acquired)
        .count();
    let rejected = [left, right]
        .into_iter()
        .filter(|outcome| matches!(outcome, LeaseOutcome::NotAcquired { .. }))
        .count();
    assert_eq!(acquired, 1, "exactly one repository adapter wins");
    assert_eq!(rejected, 1, "the competing CAS must observe zero rows");
}
