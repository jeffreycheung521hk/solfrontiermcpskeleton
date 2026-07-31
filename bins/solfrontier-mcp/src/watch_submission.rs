//! Sanitized, single-shot Solana submission and finalized-only observation.
//!
//! The caller reaches this module only after wallet-engine simulation, risk
//! policy approval, and controlled-wallet signing. Submission is attempted
//! exactly once. Once `sendTransaction` has been invoked, every transport
//! error or observation timeout is deliberately classified as
//! [`NetworkOutcome::Unknown`]: the transaction may have landed, so the
//! durable funding lease must remain `executing` for manual reconciliation.
//! No RPC URL, provider body, or raw transport error crosses this boundary.

use std::time::{Duration, Instant};

use claw_solana_core::{
    submission::{submit_transaction, SubmissionError},
    RpcPool,
};
use solana_sdk::signature::Signature;
use solana_transaction_status::{TransactionConfirmationStatus, TransactionStatus};

const INITIAL_POLL_BACKOFF: Duration = Duration::from_secs(2);
const MAX_POLL_BACKOFF: Duration = Duration::from_secs(8);
const TOTAL_POLL_TIMEOUT: Duration = Duration::from_secs(120);
const TRANSIENT_ERROR_TOLERANCE: u32 = 3;

/// Exact signed bytes emitted by the wallet-engine review pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewedSignedPayload {
    pub(crate) signed_transaction_bytes: Vec<u8>,
    pub(crate) signature: Signature,
    pub(crate) last_valid_block_height: u64,
}

/// Sanitized network disposition used by the durable execution state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NetworkOutcome {
    /// No network submission was attempted, so releasing the lease is safe.
    PreBroadcastFailure { error_class: &'static str },
    /// The transaction reached finalized commitment.
    Finalized { signature: String, slot: u64 },
    /// The transaction landed with a non-null runtime error.
    OnChainFailed {
        signature: String,
        error_class: &'static str,
    },
    /// Submission was invoked but the final result cannot be proven.
    Unknown {
        signature: String,
        error_class: &'static str,
    },
}

/// Submit once and wait for finalized commitment without rebroadcasting.
pub(crate) async fn submit_once_and_observe(
    rpc_pool: &RpcPool,
    payload: &ReviewedSignedPayload,
) -> NetworkOutcome {
    let client = match rpc_pool.write_client() {
        Ok(client) => client,
        Err(_) => {
            return NetworkOutcome::PreBroadcastFailure {
                error_class: "rpc_write_client_unavailable",
            };
        }
    };

    let submitted = match submit_transaction(
        &client,
        &payload.signed_transaction_bytes,
        payload.signature,
        payload.last_valid_block_height,
    )
    .await
    {
        Ok(submitted) => submitted,
        Err(SubmissionError::DeserializationError(_)) => {
            return NetworkOutcome::PreBroadcastFailure {
                error_class: "signed_transaction_deserialize_failed",
            };
        }
        Err(SubmissionError::RpcError(_)) => {
            // The RPC response can be lost after the node accepted the bytes.
            return NetworkOutcome::Unknown {
                signature: payload.signature.to_string(),
                error_class: "send_result_unknown",
            };
        }
        Err(SubmissionError::HeightFetchError(_)) => {
            // `sendTransaction` already returned successfully. The predecessor
            // immediately polled the deterministic payload signature and did
            // not let an auxiliary post-send block-height read strand a landed
            // transaction in `executing`. Continue observation without any
            // rebroadcast; timeout still degrades to Unknown.
            return poll_finalized(rpc_pool, payload.signature).await;
        }
    };

    if submitted.signature != payload.signature {
        return NetworkOutcome::Unknown {
            signature: payload.signature.to_string(),
            error_class: "rpc_signature_mismatch",
        };
    }

    poll_finalized(rpc_pool, payload.signature).await
}

async fn poll_finalized(rpc_pool: &RpcPool, signature: Signature) -> NetworkOutcome {
    let finality = FinalityTracker::new(signature);
    let deadline = Instant::now() + TOTAL_POLL_TIMEOUT;
    let mut backoff = INITIAL_POLL_BACKOFF;
    let mut consecutive_errors = 0_u32;

    loop {
        if Instant::now() >= deadline {
            return finality.unknown("finality_timeout");
        }
        tokio::time::sleep(backoff).await;
        if Instant::now() >= deadline {
            return finality.unknown("finality_timeout");
        }

        let client = match rpc_pool.read_client() {
            Ok(client) => client,
            Err(_) => {
                consecutive_errors = consecutive_errors.saturating_add(1);
                if consecutive_errors > TRANSIENT_ERROR_TOLERANCE {
                    return finality.unknown("confirmation_rpc_unavailable");
                }
                backoff = next_backoff(backoff);
                continue;
            }
        };

        match client
            .get_signature_statuses_with_history(&[signature])
            .await
        {
            Ok(response) => {
                rpc_pool.record_success(&client.url());
                consecutive_errors = 0;
                if let Some(outcome) =
                    finality.observe(response.value.first().and_then(Option::as_ref))
                {
                    return outcome;
                }
            }
            Err(_) => {
                rpc_pool.record_failure(&client.url());
                consecutive_errors = consecutive_errors.saturating_add(1);
                if consecutive_errors > TRANSIENT_ERROR_TOLERANCE {
                    return finality.unknown("confirmation_result_unknown");
                }
            }
        }
        backoff = next_backoff(backoff);
    }
}

fn next_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(MAX_POLL_BACKOFF)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollDecision {
    Pending,
    Finalized { slot: u64 },
    OnChainFailed,
}

/// Stateless finality mapper for one deterministic transaction signature.
///
/// A non-final observation deliberately leaves no sticky failure bit: an error
/// seen on a processed/confirmed fork cannot contaminate a later finalized
/// success, and exhausting the polling budget remains an ambiguous outcome.
#[derive(Debug, Clone, Copy)]
struct FinalityTracker {
    signature: Signature,
}

impl FinalityTracker {
    const fn new(signature: Signature) -> Self {
        Self { signature }
    }

    fn observe(&self, status: Option<&TransactionStatus>) -> Option<NetworkOutcome> {
        match classify_status(status) {
            PollDecision::Pending => None,
            PollDecision::Finalized { slot } => Some(self.finalized(slot)),
            PollDecision::OnChainFailed => Some(self.on_chain_failed()),
        }
    }

    fn finalized(&self, slot: u64) -> NetworkOutcome {
        NetworkOutcome::Finalized {
            signature: self.signature.to_string(),
            slot,
        }
    }

    fn on_chain_failed(&self) -> NetworkOutcome {
        NetworkOutcome::OnChainFailed {
            signature: self.signature.to_string(),
            error_class: "transaction_failed_on_chain",
        }
    }

    fn unknown(&self, error_class: &'static str) -> NetworkOutcome {
        NetworkOutcome::Unknown {
            signature: self.signature.to_string(),
            error_class,
        }
    }
}

fn classify_status(status: Option<&TransactionStatus>) -> PollDecision {
    let Some(status) = status else {
        return PollDecision::Pending;
    };
    match status.confirmation_status {
        Some(TransactionConfirmationStatus::Finalized) if status.err.is_some() => {
            PollDecision::OnChainFailed
        }
        Some(TransactionConfirmationStatus::Finalized) => {
            PollDecision::Finalized { slot: status.slot }
        }
        // Neither success nor failure is terminal before the transaction is
        // finalized. A non-final fork can disappear, so even an observed `err`
        // remains pending and cannot release or terminal-fail the lease.
        Some(TransactionConfirmationStatus::Confirmed)
        | Some(TransactionConfirmationStatus::Processed)
        | None => PollDecision::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_sdk::transaction::TransactionError;

    fn status(
        confirmation_status: Option<TransactionConfirmationStatus>,
        err: Option<TransactionError>,
    ) -> TransactionStatus {
        TransactionStatus {
            slot: 42,
            confirmations: None,
            status: match &err {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            },
            err,
            confirmation_status,
        }
    }

    #[test]
    fn confirmed_is_pending_and_only_finalized_succeeds() {
        let confirmed = status(Some(TransactionConfirmationStatus::Confirmed), None);
        assert_eq!(classify_status(Some(&confirmed)), PollDecision::Pending);

        let finalized = status(Some(TransactionConfirmationStatus::Finalized), None);
        assert_eq!(
            classify_status(Some(&finalized)),
            PollDecision::Finalized { slot: 42 }
        );
    }

    #[test]
    fn processed_and_confirmed_errors_remain_pending() {
        for commitment in [
            TransactionConfirmationStatus::Processed,
            TransactionConfirmationStatus::Confirmed,
        ] {
            let failed = status(Some(commitment), Some(TransactionError::AccountNotFound));
            assert_eq!(classify_status(Some(&failed)), PollDecision::Pending);
        }
    }

    #[test]
    fn the_same_signature_becomes_failed_only_after_finalized_error() {
        let signature = Signature::new_unique();
        let tracker = FinalityTracker::new(signature);
        let early = status(
            Some(TransactionConfirmationStatus::Processed),
            Some(TransactionError::AccountNotFound),
        );
        assert_eq!(tracker.observe(Some(&early)), None);

        let failed = status(
            Some(TransactionConfirmationStatus::Finalized),
            Some(TransactionError::AccountNotFound),
        );
        assert_eq!(
            tracker.observe(Some(&failed)),
            Some(NetworkOutcome::OnChainFailed {
                signature: signature.to_string(),
                error_class: "transaction_failed_on_chain",
            })
        );
    }

    #[test]
    fn a_nonfinal_error_does_not_contaminate_later_finalized_success() {
        let signature = Signature::new_unique();
        let tracker = FinalityTracker::new(signature);
        let early = status(
            Some(TransactionConfirmationStatus::Confirmed),
            Some(TransactionError::AccountNotFound),
        );
        assert_eq!(tracker.observe(Some(&early)), None);

        let finalized = status(Some(TransactionConfirmationStatus::Finalized), None);
        assert_eq!(
            tracker.observe(Some(&finalized)),
            Some(NetworkOutcome::Finalized {
                signature: signature.to_string(),
                slot: 42,
            })
        );
    }

    #[test]
    fn polling_budget_after_nonfinal_error_remains_unknown_not_failed() {
        let signature = Signature::new_unique();
        let tracker = FinalityTracker::new(signature);
        let early = status(
            Some(TransactionConfirmationStatus::Processed),
            Some(TransactionError::AccountNotFound),
        );
        assert_eq!(tracker.observe(Some(&early)), None);
        assert_eq!(
            tracker.unknown("finality_timeout"),
            NetworkOutcome::Unknown {
                signature: signature.to_string(),
                error_class: "finality_timeout",
            }
        );
    }

    #[test]
    fn null_and_processed_observations_remain_pending() {
        assert_eq!(classify_status(None), PollDecision::Pending);
        let processed = status(Some(TransactionConfirmationStatus::Processed), None);
        assert_eq!(classify_status(Some(&processed)), PollDecision::Pending);
    }
}
