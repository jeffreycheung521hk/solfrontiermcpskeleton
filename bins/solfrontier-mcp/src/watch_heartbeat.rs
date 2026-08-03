//! Proof that an executor is alive, for whoever is about to commit money.
//!
//! The 2026-08-01 acceptance failed on ordering, not on a bug: every manual
//! step ran before `watch --execute` was started, and by the time it was, both
//! deadlines had passed. 0.2 USDC is still stranded because of it.
//!
//! The structural fix is not a faster operator. It is that nothing should be
//! able to hand a signing URL to a person without first proving the process
//! that must act on the payment is already running. This file is the evidence
//! half of that: the execute loop stamps a small JSON file at the top of every
//! cycle, and a client refuses the handoff unless it is fresh.
//!
//! Deliberately a plain file rather than a database row. It is liveness
//! evidence, not state: it must stay readable by a separate process that holds
//! no write lock, and it must never be able to influence a lifecycle decision.

use std::path::{Path, PathBuf};

use serde_json::json;
use solana_sdk::pubkey::Pubkey;

const HEARTBEAT_SUFFIX: &str = ".executor-heartbeat.json";

pub(crate) fn derive_heartbeat_path(main_db_path: &Path) -> PathBuf {
    let mut name = main_db_path.as_os_str().to_os_string();
    name.push(HEARTBEAT_SUFFIX);
    PathBuf::from(name)
}

pub(crate) struct ExecutorHeartbeat {
    path: PathBuf,
}

impl ExecutorHeartbeat {
    pub(crate) fn for_database_path(main_db_path: &Path) -> Self {
        Self {
            path: derive_heartbeat_path(main_db_path),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Stamp the current cycle. Failure is logged and ignored: a heartbeat that
    /// cannot be written must not stop an executor that is otherwise working,
    /// and its absence already makes the client refuse, which is the safe
    /// direction.
    pub(crate) async fn beat(&self, controlled_wallet: Pubkey) {
        let payload = json!({
            "role": "watch_execute",
            "controlled_wallet": controlled_wallet.to_string(),
            "updated_at_ms": chrono::Utc::now().timestamp_millis(),
            "db_path": self.path.to_string_lossy(),
            "meaning": "this process was alive and entering a scan cycle at updated_at_ms",
        });
        // Write to a temporary file and rename, so a reader never observes a
        // half-written record and mistakes a truncated file for a stale one.
        let temporary = self.path.with_extension("json.tmp");
        if let Err(error) = tokio::fs::write(&temporary, payload.to_string()).await {
            tracing::warn!(error = %error, "executor heartbeat write failed");
            return;
        }
        if let Err(error) = tokio::fs::rename(&temporary, &self.path).await {
            tracing::warn!(error = %error, "executor heartbeat rename failed");
        }
    }
}

/// How a reader should judge a heartbeat it has just loaded.
///
/// Pure, so the staleness boundary is testable without a clock or a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeartbeatVerdict {
    /// A live executor, pinned to the expected wallet.
    Live,
    /// Readable, but older than the tolerated age.
    Stale { age_ms: i64 },
    /// Readable, but belongs to a different controlled wallet.
    WrongWallet,
    /// Timestamped in the future by more than clock skew allows.
    FromTheFuture { skew_ms: i64 },
}

/// Maximum age a client should accept. Two execute cycles plus margin: a
/// heartbeat older than this means the loop is wedged, not merely between
/// scans.
pub(crate) const MAX_HEARTBEAT_AGE_MS: i64 = 75_000;
/// Clocks on one machine can disagree slightly; more than this is not skew.
pub(crate) const MAX_HEARTBEAT_SKEW_MS: i64 = 5_000;

pub(crate) fn judge_heartbeat(
    updated_at_ms: i64,
    heartbeat_wallet: &str,
    expected_wallet: &str,
    now_ms: i64,
) -> HeartbeatVerdict {
    if heartbeat_wallet != expected_wallet {
        // A heartbeat from an executor pinned to a different wallet proves
        // nothing about the one that has to act on this payment.
        return HeartbeatVerdict::WrongWallet;
    }
    let age_ms = now_ms - updated_at_ms;
    if age_ms < -MAX_HEARTBEAT_SKEW_MS {
        return HeartbeatVerdict::FromTheFuture { skew_ms: -age_ms };
    }
    if age_ms > MAX_HEARTBEAT_AGE_MS {
        return HeartbeatVerdict::Stale { age_ms };
    }
    HeartbeatVerdict::Live
}

#[cfg(test)]
mod tests {
    use super::*;

    const WALLET: &str = "BPfDMmeMBmCbMC1rWh7hwigMBoKGBrKwXxSeUu9hhs5L";
    const OTHER: &str = "C4QQjzWxnJ5QFAbkzhQJ3wTzyX6nw1vyFvJwbPXJGPNW";

    #[test]
    fn a_recent_heartbeat_from_the_pinned_wallet_is_live() {
        assert_eq!(
            judge_heartbeat(1_000, WALLET, WALLET, 1_000 + 30_000),
            HeartbeatVerdict::Live
        );
    }

    #[test]
    fn the_staleness_boundary_is_exclusive() {
        assert_eq!(
            judge_heartbeat(0, WALLET, WALLET, MAX_HEARTBEAT_AGE_MS),
            HeartbeatVerdict::Live
        );
        assert_eq!(
            judge_heartbeat(0, WALLET, WALLET, MAX_HEARTBEAT_AGE_MS + 1),
            HeartbeatVerdict::Stale {
                age_ms: MAX_HEARTBEAT_AGE_MS + 1
            }
        );
    }

    #[test]
    fn a_heartbeat_for_another_wallet_proves_nothing() {
        // The wallet check comes first on purpose: a fresh heartbeat from the
        // wrong executor is more dangerous than a stale one from the right
        // executor, because it looks entirely healthy.
        assert_eq!(
            judge_heartbeat(0, OTHER, WALLET, 0),
            HeartbeatVerdict::WrongWallet
        );
    }

    #[test]
    fn a_future_timestamp_beyond_skew_is_refused() {
        assert_eq!(
            judge_heartbeat(10_000, WALLET, WALLET, 10_000 - MAX_HEARTBEAT_SKEW_MS),
            HeartbeatVerdict::Live,
            "small skew is tolerated"
        );
        assert_eq!(
            judge_heartbeat(10_000, WALLET, WALLET, 10_000 - MAX_HEARTBEAT_SKEW_MS - 1),
            HeartbeatVerdict::FromTheFuture {
                skew_ms: MAX_HEARTBEAT_SKEW_MS + 1
            }
        );
    }
}
