//! Pre-broadcast durable record of a signed refund.
//!
//! Why this file exists at all: the frozen funding repository writes
//! `refund_signature` inside `mark_refunded_if_refunding`, which runs *after*
//! confirmation. A daemon that signs, broadcasts and then dies therefore leaves
//! a row in `refunding` with no record of what it sent. Re-signing at that
//! point produces a second transaction with a fresh blockhash, and if the first
//! one landed that is a double refund out of a shared controlled wallet.
//!
//! The journal closes that window without touching the frozen crate, using the
//! same pattern that closed DEBT-MCP-1: a bin-owned SQLite file beside the main
//! database, holding derived data only. The main database remains the sole
//! source of truth for lifecycle state — a journal entry never authorises a
//! transition, it only tells recovery which bytes were already committed to.
//!
//! Durability is the entire point, so this pool runs `synchronous = FULL`. A
//! journal that has not reached the platter before `sendTransaction` is a
//! journal that does not exist.

use std::path::{Path, PathBuf};

use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    Row, SqlitePool,
};
use thiserror::Error;

const JOURNAL_SUFFIX: &str = ".mcp-refund-journal.sqlite3";
const JOURNAL_SCHEMA_VERSION: i64 = 1;

const CREATE_META_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS refund_journal_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
)
"#;

const CREATE_ATTEMPTS_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS refund_attempts (
    intent_id               TEXT PRIMARY KEY,
    signature               TEXT NOT NULL,
    last_valid_block_height INTEGER NOT NULL,
    signed_transaction      BLOB NOT NULL,
    recorded_at_ms          INTEGER NOT NULL
)
"#;

pub(crate) fn derive_journal_path(main_db_path: &Path) -> PathBuf {
    let mut name = main_db_path.as_os_str().to_os_string();
    name.push(JOURNAL_SUFFIX);
    PathBuf::from(name)
}

/// A stable, non-sensitive failure class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub(crate) enum RefundJournalError {
    #[error("refund_journal_unavailable")]
    Unavailable,
    #[error("refund_journal_schema_incompatible")]
    SchemaIncompatible,
    #[error("refund_journal_write_failed")]
    WriteFailed,
    #[error("refund_journal_row_corrupt")]
    RowCorrupt,
    /// A different signature is already recorded for this intent. Overwriting
    /// it would erase the only evidence of what may already be on chain.
    #[error("refund_journal_conflicting_attempt")]
    ConflictingAttempt,
}

impl RefundJournalError {
    pub(crate) const fn class(self) -> &'static str {
        match self {
            Self::Unavailable => "refund_journal_unavailable",
            Self::SchemaIncompatible => "refund_journal_schema_incompatible",
            Self::WriteFailed => "refund_journal_write_failed",
            Self::RowCorrupt => "refund_journal_row_corrupt",
            Self::ConflictingAttempt => "refund_journal_conflicting_attempt",
        }
    }
}

/// One committed refund attempt, exactly as it was signed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecordedAttempt {
    pub(crate) signature: String,
    pub(crate) last_valid_block_height: u64,
    pub(crate) signed_transaction: Vec<u8>,
    pub(crate) recorded_at_ms: i64,
}

/// What the journal can say about an intent.
///
/// `Unavailable` is deliberately distinct from `Absent`: the first means the
/// journal could not be read and recovery must halt, the second means it was
/// read and nothing was ever signed. Collapsing them would let a corrupt file
/// authorise a fresh signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JournalLookup {
    Found(Box<RecordedAttempt>),
    Absent,
    Unavailable { error_class: &'static str },
}

pub(crate) struct RefundJournal {
    path: PathBuf,
}

impl RefundJournal {
    pub(crate) fn for_database_path(main_db_path: &Path) -> Self {
        Self {
            path: derive_journal_path(main_db_path),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    async fn pool(&self, create_if_missing: bool) -> Result<SqlitePool, RefundJournalError> {
        let options = SqliteConnectOptions::new()
            .filename(&self.path)
            .create_if_missing(create_if_missing)
            .journal_mode(SqliteJournalMode::Wal)
            // Not negotiable: an entry that has not been flushed before
            // sendTransaction cannot be relied on after a crash.
            .synchronous(SqliteSynchronous::Full);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .map_err(|_| RefundJournalError::Unavailable)?;
        Ok(pool)
    }

    async fn ready_pool(&self, create_if_missing: bool) -> Result<SqlitePool, RefundJournalError> {
        let pool = self.pool(create_if_missing).await?;
        sqlx::query(CREATE_META_SQL)
            .execute(&pool)
            .await
            .map_err(|_| RefundJournalError::Unavailable)?;
        sqlx::query(CREATE_ATTEMPTS_SQL)
            .execute(&pool)
            .await
            .map_err(|_| RefundJournalError::Unavailable)?;
        sqlx::query(
            "INSERT OR IGNORE INTO refund_journal_meta (key, value) VALUES ('schema_version', ?)",
        )
        .bind(JOURNAL_SCHEMA_VERSION.to_string())
        .execute(&pool)
        .await
        .map_err(|_| RefundJournalError::Unavailable)?;

        let version: String =
            sqlx::query("SELECT value FROM refund_journal_meta WHERE key = 'schema_version'")
                .fetch_one(&pool)
                .await
                .map_err(|_| RefundJournalError::Unavailable)?
                .try_get(0)
                .map_err(|_| RefundJournalError::RowCorrupt)?;
        if version != JOURNAL_SCHEMA_VERSION.to_string() {
            return Err(RefundJournalError::SchemaIncompatible);
        }
        Ok(pool)
    }

    /// Commit to these exact bytes before they are broadcast.
    ///
    /// Must be called, and must succeed, before `sendTransaction`. If it fails
    /// the caller has to abort without broadcasting: signing is reversible only
    /// while nothing has been sent.
    ///
    /// Recording the same signature twice is idempotent, so a retry of the same
    /// attempt is safe. Recording a *different* signature for an intent that
    /// already has one is refused — that would erase the only evidence of what
    /// may already be in flight.
    pub(crate) async fn record_before_broadcast(
        &self,
        intent_id: &str,
        attempt: &RecordedAttempt,
    ) -> Result<(), RefundJournalError> {
        let pool = self.ready_pool(true).await?;
        if let Some(existing) = self.read_from(&pool, intent_id).await? {
            if existing.signature == attempt.signature {
                return Ok(());
            }
            return Err(RefundJournalError::ConflictingAttempt);
        }
        sqlx::query(
            "INSERT INTO refund_attempts
                (intent_id, signature, last_valid_block_height, signed_transaction, recorded_at_ms)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(intent_id)
        .bind(&attempt.signature)
        .bind(attempt.last_valid_block_height as i64)
        .bind(&attempt.signed_transaction)
        .bind(attempt.recorded_at_ms)
        .execute(&pool)
        .await
        .map_err(|_| RefundJournalError::WriteFailed)?;
        Ok(())
    }

    /// What, if anything, was already committed to for this intent.
    pub(crate) async fn lookup(&self, intent_id: &str) -> JournalLookup {
        // Never create the file here. A recovery read that silently creates an
        // empty journal would report Absent -- "nothing was ever signed" -- for
        // an intent whose journal had simply been deleted.
        let pool = match self.ready_pool(false).await {
            Ok(pool) => pool,
            Err(error) => {
                return JournalLookup::Unavailable {
                    error_class: error.class(),
                }
            }
        };
        match self.read_from(&pool, intent_id).await {
            Ok(Some(attempt)) => JournalLookup::Found(Box::new(attempt)),
            Ok(None) => JournalLookup::Absent,
            Err(error) => JournalLookup::Unavailable {
                error_class: error.class(),
            },
        }
    }

    async fn read_from(
        &self,
        pool: &SqlitePool,
        intent_id: &str,
    ) -> Result<Option<RecordedAttempt>, RefundJournalError> {
        let row = sqlx::query(
            "SELECT signature, last_valid_block_height, signed_transaction, recorded_at_ms
               FROM refund_attempts WHERE intent_id = ?",
        )
        .bind(intent_id)
        .fetch_optional(pool)
        .await
        .map_err(|_| RefundJournalError::Unavailable)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let signature: String = row.try_get(0).map_err(|_| RefundJournalError::RowCorrupt)?;
        let height: i64 = row.try_get(1).map_err(|_| RefundJournalError::RowCorrupt)?;
        let bytes: Vec<u8> = row.try_get(2).map_err(|_| RefundJournalError::RowCorrupt)?;
        let recorded_at_ms: i64 = row.try_get(3).map_err(|_| RefundJournalError::RowCorrupt)?;
        if signature.is_empty() || bytes.is_empty() || height < 0 {
            return Err(RefundJournalError::RowCorrupt);
        }
        Ok(Some(RecordedAttempt {
            signature,
            last_valid_block_height: height as u64,
            signed_transaction: bytes,
            recorded_at_ms,
        }))
    }
}

/// Whether an intent is on chain, as far as the RPC can say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChainPresence {
    Found,
    Absent,
    Unknown,
}

/// What recovery may do about a refund whose outcome is not known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecoveryAction {
    /// Halt and reconcile by hand. Neither resending nor signing is provably
    /// safe, so neither is permitted.
    Halt { reason: &'static str },
    /// Reconcile the recorded signature against the chain result.
    Reconcile { signature: String },
    /// Resend exactly these bytes. Never re-sign: a fresh blockhash would make
    /// a second transaction that can land alongside the first.
    ResendSame { signature: String },
    /// The recorded bytes can no longer be accepted by any validator, so a
    /// fresh signature is provably safe.
    SignNew { reason: &'static str },
}

/// Decide what to do about a refund whose outcome is unknown.
///
/// The resolver is blockhash expiry, which is a fact about the chain rather
/// than a promise about our own bookkeeping: past `last_valid_block_height`, a
/// transaction that has not landed can never land.
///
/// This is a pure function so that every crash window can be tested without a
/// network, a wallet or a database.
pub(crate) fn recovery_action(
    lookup: &JournalLookup,
    presence: ChainPresence,
    current_block_height: Option<u64>,
) -> RecoveryAction {
    let attempt = match lookup {
        JournalLookup::Unavailable { error_class } => {
            // The daemon cannot tell "never sent" from "sent and lost". Both
            // available moves can be wrong, so neither is allowed.
            return RecoveryAction::Halt {
                reason: error_class,
            };
        }
        JournalLookup::Absent => {
            // The journal was readable and says nothing was ever signed. This
            // is the only state in which signing fresh is provably safe.
            return RecoveryAction::SignNew {
                reason: "nothing_was_signed",
            };
        }
        JournalLookup::Found(attempt) => attempt,
    };

    match presence {
        ChainPresence::Found => RecoveryAction::Reconcile {
            signature: attempt.signature.clone(),
        },
        ChainPresence::Unknown => RecoveryAction::Halt {
            reason: "chain_state_unknown",
        },
        ChainPresence::Absent => match current_block_height {
            None => RecoveryAction::Halt {
                reason: "block_height_unknown",
            },
            Some(height) if height > attempt.last_valid_block_height => RecoveryAction::SignNew {
                reason: "blockhash_expired",
            },
            Some(_) => RecoveryAction::ResendSame {
                signature: attempt.signature.clone(),
            },
        },
    }
}
