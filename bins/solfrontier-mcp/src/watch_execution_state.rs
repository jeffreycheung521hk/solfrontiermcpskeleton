//! Narrow persistence adapter for the Phase 3b execution state machine.
//!
//! The caller owns final revalidation and all transaction work. This module
//! only maps an already validated execution onto the public state-store CAS
//! surface. In particular, the funding row is the sole execution lease:
//! the WatchRule remains `active` / `condition_met` until a finalized
//! transaction is recorded.
//!
//! Store errors are deliberately collapsed to fixed classes. Neither SQLite
//! error text nor values from a database row cross this boundary.

use claw_state_store::{Stage2W5hFundingIntentRepository, Stage2WatchRuleRepository};

const PREBROADCAST_FAILURE_REASON: &str = "execution_prebroadcast_failed";
const ONCHAIN_FAILURE_REASON: &str = "execution_onchain_failed";

const LEASE_NOT_ACQUIRED: &str = "execution_lease_not_acquired";
const LEASE_STORE_UNAVAILABLE: &str = "execution_lease_store_unavailable";
const RELEASE_NOT_APPLIED: &str = "execution_release_not_applied";
const RELEASE_STORE_UNAVAILABLE: &str = "execution_release_store_unavailable";
const FAILURE_NOT_RECORDED: &str = "execution_failure_not_recorded";
const FAILURE_STORE_UNAVAILABLE: &str = "execution_failure_store_unavailable";
const RULE_COMPLETION_NOT_APPLIED: &str = "watch_rule_completion_not_applied";
const RULE_COMPLETION_STORE_UNAVAILABLE: &str = "watch_rule_completion_store_unavailable";
const FUNDING_COMPLETION_NOT_APPLIED_AFTER_RULE: &str =
    "funding_completion_not_applied_after_rule_completed";
const FUNDING_COMPLETION_STORE_UNAVAILABLE_AFTER_RULE: &str =
    "funding_completion_store_unavailable_after_rule_completed";

/// Zero-information error used by the repository seam.
///
/// Production implementations erase the underlying `StoreError` here so a
/// caller cannot accidentally log database text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StateStoreUnavailable;

#[allow(async_fn_in_trait)]
pub(crate) trait ExecutionStatePort: Send + Sync {
    async fn lease_funding(
        &self,
        intent_id: &str,
        now_ms: i64,
    ) -> Result<u64, StateStoreUnavailable>;

    async fn release_funding(
        &self,
        intent_id: &str,
        reason: &str,
    ) -> Result<u64, StateStoreUnavailable>;

    async fn fail_funding(
        &self,
        intent_id: &str,
        reason: &str,
    ) -> Result<u64, StateStoreUnavailable>;

    async fn complete_watch_rule(
        &self,
        rule_id: &[u8; 16],
        used_amount_raw: u64,
        finalized_slot: u64,
    ) -> Result<u64, StateStoreUnavailable>;

    async fn complete_funding(
        &self,
        intent_id: &str,
        execution_signature: &str,
    ) -> Result<u64, StateStoreUnavailable>;
}

/// Production seam backed exclusively by public `claw-state-store` APIs.
#[derive(Clone, Debug)]
pub(crate) struct RepositoryExecutionState {
    funding: Stage2W5hFundingIntentRepository,
    watch_rules: Stage2WatchRuleRepository,
}

impl RepositoryExecutionState {
    pub(crate) fn new(
        funding: Stage2W5hFundingIntentRepository,
        watch_rules: Stage2WatchRuleRepository,
    ) -> Self {
        Self {
            funding,
            watch_rules,
        }
    }
}

impl ExecutionStatePort for RepositoryExecutionState {
    async fn lease_funding(
        &self,
        intent_id: &str,
        now_ms: i64,
    ) -> Result<u64, StateStoreUnavailable> {
        self.funding
            .lease_execution_if_budget_reserved(intent_id, now_ms)
            .await
            .map_err(|_| StateStoreUnavailable)
    }

    async fn release_funding(
        &self,
        intent_id: &str,
        reason: &str,
    ) -> Result<u64, StateStoreUnavailable> {
        self.funding
            .release_execution_lease_to_budget_reserved(intent_id, reason)
            .await
            .map_err(|_| StateStoreUnavailable)
    }

    async fn fail_funding(
        &self,
        intent_id: &str,
        reason: &str,
    ) -> Result<u64, StateStoreUnavailable> {
        self.funding
            .mark_failed(intent_id, reason)
            .await
            .map_err(|_| StateStoreUnavailable)
    }

    async fn complete_watch_rule(
        &self,
        rule_id: &[u8; 16],
        used_amount_raw: u64,
        finalized_slot: u64,
    ) -> Result<u64, StateStoreUnavailable> {
        self.watch_rules
            .mark_completed(rule_id, used_amount_raw, finalized_slot)
            .await
            .map_err(|_| StateStoreUnavailable)
    }

    async fn complete_funding(
        &self,
        intent_id: &str,
        execution_signature: &str,
    ) -> Result<u64, StateStoreUnavailable> {
        self.funding
            .mark_completed_if_executing(intent_id, execution_signature)
            .await
            .map_err(|_| StateStoreUnavailable)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeaseOutcome {
    Acquired,
    NotAcquired { error_class: &'static str },
    StoreUnavailable { error_class: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StateWriteOutcome {
    Applied,
    NotApplied { error_class: &'static str },
    StoreUnavailable { error_class: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FinalizedWriteOutcome {
    Completed,
    WatchRuleNotCompleted {
        error_class: &'static str,
    },
    WatchRuleStoreUnavailable {
        error_class: &'static str,
    },
    FundingNotCompletedAfterRule {
        error_class: &'static str,
        watch_rule_completed: bool,
    },
    FundingStoreUnavailableAfterRule {
        error_class: &'static str,
        watch_rule_completed: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnknownBroadcastOutcome {
    LeftExecutingWithoutWrite,
}

#[derive(Debug)]
pub(crate) struct ExecutionStateAdapter<S> {
    state: S,
}

impl<S> ExecutionStateAdapter<S>
where
    S: ExecutionStatePort,
{
    pub(crate) fn new(state: S) -> Self {
        Self { state }
    }

    #[cfg(test)]
    pub(crate) fn state_for_test(&self) -> &S {
        &self.state
    }

    /// Acquire the funding CAS only after the caller has completed its final
    /// canonical-hash, amount, and dual-clock revalidation.
    pub(crate) async fn lease_after_final_revalidation(
        &self,
        intent_id: &str,
        now_ms: i64,
    ) -> LeaseOutcome {
        match self.state.lease_funding(intent_id, now_ms).await {
            Ok(1) => LeaseOutcome::Acquired,
            Ok(_) => LeaseOutcome::NotAcquired {
                error_class: LEASE_NOT_ACQUIRED,
            },
            Err(StateStoreUnavailable) => LeaseOutcome::StoreUnavailable {
                error_class: LEASE_STORE_UNAVAILABLE,
            },
        }
    }

    /// A failure known to have happened before broadcast can safely release
    /// the funding lease for a later attempt.
    pub(crate) async fn release_prebroadcast_failure(&self, intent_id: &str) -> StateWriteOutcome {
        match self
            .state
            .release_funding(intent_id, PREBROADCAST_FAILURE_REASON)
            .await
        {
            Ok(1) => StateWriteOutcome::Applied,
            Ok(_) => StateWriteOutcome::NotApplied {
                error_class: RELEASE_NOT_APPLIED,
            },
            Err(StateStoreUnavailable) => StateWriteOutcome::StoreUnavailable {
                error_class: RELEASE_STORE_UNAVAILABLE,
            },
        }
    }

    /// A finalized on-chain failure terminates the funding row only. It does
    /// not mutate the WatchRule; non-final errors never reach this method.
    pub(crate) async fn mark_onchain_failed(&self, intent_id: &str) -> StateWriteOutcome {
        match self
            .state
            .fail_funding(intent_id, ONCHAIN_FAILURE_REASON)
            .await
        {
            Ok(1) => StateWriteOutcome::Applied,
            Ok(_) => StateWriteOutcome::NotApplied {
                error_class: FAILURE_NOT_RECORDED,
            },
            Err(StateStoreUnavailable) => StateWriteOutcome::StoreUnavailable {
                error_class: FAILURE_STORE_UNAVAILABLE,
            },
        }
    }

    /// Preserve the predecessor's non-transactional success order:
    /// WatchRule first, funding row second.
    ///
    /// If the second write returns zero or errors, the result explicitly says
    /// that the WatchRule is already completed while funding may remain
    /// `executing`; callers must surface this for manual reconciliation.
    pub(crate) async fn mark_finalized(
        &self,
        intent_id: &str,
        rule_id: &[u8; 16],
        used_amount_raw: u64,
        finalized_slot: u64,
        execution_signature: &str,
    ) -> FinalizedWriteOutcome {
        match self
            .state
            .complete_watch_rule(rule_id, used_amount_raw, finalized_slot)
            .await
        {
            Ok(1) => {}
            Ok(_) => {
                return FinalizedWriteOutcome::WatchRuleNotCompleted {
                    error_class: RULE_COMPLETION_NOT_APPLIED,
                };
            }
            Err(StateStoreUnavailable) => {
                return FinalizedWriteOutcome::WatchRuleStoreUnavailable {
                    error_class: RULE_COMPLETION_STORE_UNAVAILABLE,
                };
            }
        }

        match self
            .state
            .complete_funding(intent_id, execution_signature)
            .await
        {
            Ok(1) => FinalizedWriteOutcome::Completed,
            Ok(_) => FinalizedWriteOutcome::FundingNotCompletedAfterRule {
                error_class: FUNDING_COMPLETION_NOT_APPLIED_AFTER_RULE,
                watch_rule_completed: true,
            },
            Err(StateStoreUnavailable) => FinalizedWriteOutcome::FundingStoreUnavailableAfterRule {
                error_class: FUNDING_COMPLETION_STORE_UNAVAILABLE_AFTER_RULE,
                watch_rule_completed: true,
            },
        }
    }

    /// An already-broadcast transaction with an unknown result must remain
    /// `executing`. Deliberately perform no repository call.
    pub(crate) fn leave_broadcast_unknown(&self, _intent_id: &str) -> UnknownBroadcastOutcome {
        UnknownBroadcastOutcome::LeftExecutingWithoutWrite
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Call {
        Lease {
            intent_id: String,
            now_ms: i64,
        },
        Release {
            intent_id: String,
            reason: String,
        },
        FailFunding {
            intent_id: String,
            reason: String,
        },
        CompleteRule {
            rule_id: [u8; 16],
            used_amount_raw: u64,
            finalized_slot: u64,
        },
        CompleteFunding {
            intent_id: String,
            execution_signature: String,
        },
    }

    #[derive(Debug)]
    struct MockState {
        calls: Mutex<Vec<Call>>,
        lease: Result<u64, StateStoreUnavailable>,
        release: Result<u64, StateStoreUnavailable>,
        fail_funding: Result<u64, StateStoreUnavailable>,
        complete_rule: Result<u64, StateStoreUnavailable>,
        complete_funding: Result<u64, StateStoreUnavailable>,
    }

    impl Default for MockState {
        fn default() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                lease: Ok(1),
                release: Ok(1),
                fail_funding: Ok(1),
                complete_rule: Ok(1),
                complete_funding: Ok(1),
            }
        }
    }

    impl MockState {
        fn calls(&self) -> Vec<Call> {
            self.calls.lock().expect("mock calls lock").clone()
        }
    }

    impl ExecutionStatePort for MockState {
        async fn lease_funding(
            &self,
            intent_id: &str,
            now_ms: i64,
        ) -> Result<u64, StateStoreUnavailable> {
            self.calls
                .lock()
                .expect("mock calls lock")
                .push(Call::Lease {
                    intent_id: intent_id.to_owned(),
                    now_ms,
                });
            self.lease
        }

        async fn release_funding(
            &self,
            intent_id: &str,
            reason: &str,
        ) -> Result<u64, StateStoreUnavailable> {
            self.calls
                .lock()
                .expect("mock calls lock")
                .push(Call::Release {
                    intent_id: intent_id.to_owned(),
                    reason: reason.to_owned(),
                });
            self.release
        }

        async fn fail_funding(
            &self,
            intent_id: &str,
            reason: &str,
        ) -> Result<u64, StateStoreUnavailable> {
            self.calls
                .lock()
                .expect("mock calls lock")
                .push(Call::FailFunding {
                    intent_id: intent_id.to_owned(),
                    reason: reason.to_owned(),
                });
            self.fail_funding
        }

        async fn complete_watch_rule(
            &self,
            rule_id: &[u8; 16],
            used_amount_raw: u64,
            finalized_slot: u64,
        ) -> Result<u64, StateStoreUnavailable> {
            self.calls
                .lock()
                .expect("mock calls lock")
                .push(Call::CompleteRule {
                    rule_id: *rule_id,
                    used_amount_raw,
                    finalized_slot,
                });
            self.complete_rule
        }

        async fn complete_funding(
            &self,
            intent_id: &str,
            execution_signature: &str,
        ) -> Result<u64, StateStoreUnavailable> {
            self.calls
                .lock()
                .expect("mock calls lock")
                .push(Call::CompleteFunding {
                    intent_id: intent_id.to_owned(),
                    execution_signature: execution_signature.to_owned(),
                });
            self.complete_funding
        }
    }

    #[tokio::test]
    async fn lease_forwards_final_revalidation_identity_and_clock() {
        let adapter = ExecutionStateAdapter::new(MockState::default());

        assert_eq!(
            adapter
                .lease_after_final_revalidation("intent-a", 123_456)
                .await,
            LeaseOutcome::Acquired
        );
        assert_eq!(
            adapter.state.calls(),
            vec![Call::Lease {
                intent_id: "intent-a".to_owned(),
                now_ms: 123_456,
            }]
        );
    }

    #[tokio::test]
    async fn lease_zero_and_store_error_are_fixed_classes() {
        let zero = ExecutionStateAdapter::new(MockState {
            lease: Ok(0),
            ..MockState::default()
        });
        assert_eq!(
            zero.lease_after_final_revalidation("intent-a", 1).await,
            LeaseOutcome::NotAcquired {
                error_class: LEASE_NOT_ACQUIRED,
            }
        );

        let unavailable = ExecutionStateAdapter::new(MockState {
            lease: Err(StateStoreUnavailable),
            ..MockState::default()
        });
        assert_eq!(
            unavailable
                .lease_after_final_revalidation("intent-a", 1)
                .await,
            LeaseOutcome::StoreUnavailable {
                error_class: LEASE_STORE_UNAVAILABLE,
            }
        );
    }

    #[tokio::test]
    async fn prebroadcast_failure_releases_with_fixed_reason() {
        let adapter = ExecutionStateAdapter::new(MockState::default());

        assert_eq!(
            adapter.release_prebroadcast_failure("intent-a").await,
            StateWriteOutcome::Applied
        );
        assert_eq!(
            adapter.state.calls(),
            vec![Call::Release {
                intent_id: "intent-a".to_owned(),
                reason: PREBROADCAST_FAILURE_REASON.to_owned(),
            }]
        );
    }

    #[tokio::test]
    async fn prebroadcast_release_zero_and_store_error_are_fixed_classes() {
        let zero = ExecutionStateAdapter::new(MockState {
            release: Ok(0),
            ..MockState::default()
        });
        assert_eq!(
            zero.release_prebroadcast_failure("intent-a").await,
            StateWriteOutcome::NotApplied {
                error_class: RELEASE_NOT_APPLIED,
            }
        );

        let unavailable = ExecutionStateAdapter::new(MockState {
            release: Err(StateStoreUnavailable),
            ..MockState::default()
        });
        assert_eq!(
            unavailable.release_prebroadcast_failure("intent-a").await,
            StateWriteOutcome::StoreUnavailable {
                error_class: RELEASE_STORE_UNAVAILABLE,
            }
        );
    }

    #[tokio::test]
    async fn onchain_failure_mutates_funding_only() {
        let adapter = ExecutionStateAdapter::new(MockState::default());

        assert_eq!(
            adapter.mark_onchain_failed("intent-a").await,
            StateWriteOutcome::Applied
        );
        assert_eq!(
            adapter.state.calls(),
            vec![Call::FailFunding {
                intent_id: "intent-a".to_owned(),
                reason: ONCHAIN_FAILURE_REASON.to_owned(),
            }]
        );
    }

    #[tokio::test]
    async fn onchain_failure_zero_and_store_error_are_fixed_classes() {
        let zero = ExecutionStateAdapter::new(MockState {
            fail_funding: Ok(0),
            ..MockState::default()
        });
        assert_eq!(
            zero.mark_onchain_failed("intent-a").await,
            StateWriteOutcome::NotApplied {
                error_class: FAILURE_NOT_RECORDED,
            }
        );

        let unavailable = ExecutionStateAdapter::new(MockState {
            fail_funding: Err(StateStoreUnavailable),
            ..MockState::default()
        });
        assert_eq!(
            unavailable.mark_onchain_failed("intent-a").await,
            StateWriteOutcome::StoreUnavailable {
                error_class: FAILURE_STORE_UNAVAILABLE,
            }
        );
    }

    #[tokio::test]
    async fn finalized_success_writes_rule_before_funding() {
        let adapter = ExecutionStateAdapter::new(MockState::default());
        let rule_id = [7_u8; 16];

        assert_eq!(
            adapter
                .mark_finalized("intent-a", &rule_id, 200_000, 987, "signature-a")
                .await,
            FinalizedWriteOutcome::Completed
        );
        assert_eq!(
            adapter.state.calls(),
            vec![
                Call::CompleteRule {
                    rule_id,
                    used_amount_raw: 200_000,
                    finalized_slot: 987,
                },
                Call::CompleteFunding {
                    intent_id: "intent-a".to_owned(),
                    execution_signature: "signature-a".to_owned(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn finalized_funding_zero_reports_preserved_split_state() {
        let adapter = ExecutionStateAdapter::new(MockState {
            complete_funding: Ok(0),
            ..MockState::default()
        });

        assert_eq!(
            adapter
                .mark_finalized("intent-a", &[7_u8; 16], 200_000, 987, "signature-a")
                .await,
            FinalizedWriteOutcome::FundingNotCompletedAfterRule {
                error_class: FUNDING_COMPLETION_NOT_APPLIED_AFTER_RULE,
                watch_rule_completed: true,
            }
        );
        assert_eq!(adapter.state.calls().len(), 2);
    }

    #[tokio::test]
    async fn finalized_funding_error_reports_preserved_split_state_without_db_text() {
        let adapter = ExecutionStateAdapter::new(MockState {
            complete_funding: Err(StateStoreUnavailable),
            ..MockState::default()
        });

        assert_eq!(
            adapter
                .mark_finalized("intent-a", &[7_u8; 16], 200_000, 987, "signature-a")
                .await,
            FinalizedWriteOutcome::FundingStoreUnavailableAfterRule {
                error_class: FUNDING_COMPLETION_STORE_UNAVAILABLE_AFTER_RULE,
                watch_rule_completed: true,
            }
        );
        assert_eq!(adapter.state.calls().len(), 2);
    }

    #[tokio::test]
    async fn failed_rule_completion_never_attempts_funding_completion() {
        let zero = ExecutionStateAdapter::new(MockState {
            complete_rule: Ok(0),
            ..MockState::default()
        });
        assert_eq!(
            zero.mark_finalized("intent-a", &[7_u8; 16], 200_000, 987, "signature-a")
                .await,
            FinalizedWriteOutcome::WatchRuleNotCompleted {
                error_class: RULE_COMPLETION_NOT_APPLIED,
            }
        );
        assert!(matches!(
            zero.state.calls().as_slice(),
            [Call::CompleteRule { .. }]
        ));

        let unavailable = ExecutionStateAdapter::new(MockState {
            complete_rule: Err(StateStoreUnavailable),
            ..MockState::default()
        });
        assert_eq!(
            unavailable
                .mark_finalized("intent-a", &[7_u8; 16], 200_000, 987, "signature-a")
                .await,
            FinalizedWriteOutcome::WatchRuleStoreUnavailable {
                error_class: RULE_COMPLETION_STORE_UNAVAILABLE,
            }
        );
        assert!(matches!(
            unavailable.state.calls().as_slice(),
            [Call::CompleteRule { .. }]
        ));
    }

    #[test]
    fn unknown_broadcast_performs_no_write() {
        let adapter = ExecutionStateAdapter::new(MockState::default());

        assert_eq!(
            adapter.leave_broadcast_unknown("intent-a"),
            UnknownBroadcastOutcome::LeftExecutingWithoutWrite
        );
        assert!(adapter.state.calls().is_empty());
    }
}
