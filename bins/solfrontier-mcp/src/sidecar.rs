//! MCP-owned finalize receipts and canonical-rule-hash index.
//!
//! This SQLite file is deliberately separate from the SolFrontier state-store
//! database. The index is derived data only: callers must re-read the main
//! repositories and recompute/compare their canonical rule hash before trusting
//! a lookup result. A missing, incompatible, or corrupt sidecar therefore makes
//! hash lookup unavailable; it never changes the main database's source-of-truth
//! status.
//!
//! `draft_id` is a random, noncanonical request correlation value. It is never
//! included in either the frozen draft-hash preimage or the canonical rule hash.
//! Its permanent tombstone only preserves consume-once finalize semantics.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    Sqlite, SqlitePool, Transaction,
};
use thiserror::Error;
use tokio::sync::Mutex;

const SIDECAR_SUFFIX: &str = ".mcp-intent-index.sqlite3";
const SIDECAR_SCHEMA_VERSION: i64 = 1;

const CREATE_META_SQL: &str = r#"
CREATE TABLE mcp_meta (
    singleton      INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL
)
"#;

const CREATE_CONSUMED_DRAFTS_SQL: &str = r#"
CREATE TABLE consumed_drafts (
    draft_id       TEXT    PRIMARY KEY,
    draft_hash     TEXT    NOT NULL,
    consumed_at_ms INTEGER NOT NULL,
    CHECK (
        length(draft_id) = 32
        AND draft_id = lower(draft_id)
        AND draft_id NOT GLOB '*[^0-9a-f]*'
    ),
    CHECK (
        length(draft_hash) = 64
        AND draft_hash = lower(draft_hash)
        AND draft_hash NOT GLOB '*[^0-9a-f]*'
    ),
    UNIQUE (draft_id, draft_hash)
)
"#;

const CREATE_HASH_INDEX_SQL: &str = r#"
CREATE TABLE canonical_hash_index (
    canonical_rule_hash TEXT    PRIMARY KEY,
    intent_id           TEXT    NOT NULL UNIQUE,
    draft_id            TEXT    NOT NULL,
    draft_hash          TEXT    NOT NULL,
    indexed_at_ms       INTEGER NOT NULL,
    CHECK (
        length(canonical_rule_hash) = 64
        AND canonical_rule_hash = lower(canonical_rule_hash)
        AND canonical_rule_hash NOT GLOB '*[^0-9a-f]*'
    ),
    CHECK (
        length(intent_id) = 32
        AND intent_id = lower(intent_id)
        AND intent_id NOT GLOB '*[^0-9a-f]*'
    ),
    FOREIGN KEY (draft_id, draft_hash)
        REFERENCES consumed_drafts (draft_id, draft_hash)
)
"#;

const SELECT_HASH_ENTRY_SQL: &str = r#"
SELECT canonical_rule_hash, intent_id, draft_id, draft_hash, indexed_at_ms
FROM canonical_hash_index
WHERE canonical_rule_hash = ?
"#;

const SELECT_INTENT_ENTRY_SQL: &str = r#"
SELECT canonical_rule_hash, intent_id, draft_id, draft_hash, indexed_at_ms
FROM canonical_hash_index
WHERE intent_id = ?
"#;

/// Derive the bin-owned sidecar path without changing or opening the main DB.
///
/// Appending instead of replacing the extension keeps paths without an
/// extension unambiguous and makes the separation visible to operators.
pub(crate) fn derive_sidecar_path(main_db_path: &Path) -> PathBuf {
    let mut path = main_db_path.as_os_str().to_os_string();
    path.push(SIDECAR_SUFFIX);
    PathBuf::from(path)
}

/// One immutable, derived canonical-hash mapping.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub(crate) struct HashIndexEntry {
    pub(crate) canonical_rule_hash: String,
    pub(crate) intent_id: String,
    pub(crate) draft_id: String,
    pub(crate) draft_hash: String,
    pub(crate) indexed_at_ms: i64,
}

/// Result of atomically consuming a noncanonical draft id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimOutcome {
    Claimed,
    AlreadyConsumed,
    Conflict,
    Unavailable,
}

/// Result of appending a derived canonical-hash mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndexOutcome {
    Indexed,
    AlreadyPresent,
    Unavailable,
    Conflict,
}

/// Hash lookup deliberately distinguishes a valid index miss from an index
/// that cannot safely be consulted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HashLookup {
    Found { intent_id: String },
    Missing,
    Unavailable,
}

#[derive(Debug, Error)]
pub(crate) enum SidecarError {
    #[error("{field} must be exactly {expected_len} lowercase hexadecimal characters")]
    InvalidHex {
        field: &'static str,
        expected_len: usize,
    },
    #[error("{field} must be non-negative")]
    InvalidTimestamp { field: &'static str },
    #[error("intent sidecar is unavailable")]
    Unavailable,
    #[error("intent sidecar has not been initialized by a draft claim")]
    NotInitialized,
    #[error("intent sidecar integrity check failed")]
    Integrity,
    #[error("unsupported intent sidecar schema version {found}")]
    UnsupportedSchema { found: i64 },
    #[error("intent sidecar contains a conflicting canonical-hash mapping")]
    MappingConflict,
    #[error("intent sidecar storage operation failed")]
    Storage(#[source] sqlx::Error),
    #[error("intent sidecar filesystem check failed")]
    Filesystem(#[source] std::io::Error),
}

#[derive(Debug)]
enum SidecarState {
    /// No I/O has happened yet. Lookup may inspect but must not create the file.
    Unopened,
    Ready(SqlitePool),
    /// Fail closed for the lifetime of this server instance. Operators may
    /// repair the sidecar and restart; the main state-store remains untouched.
    Unavailable,
}

/// Lazily opened, bin-owned sidecar.
///
/// Clones share both the pool and initialization/availability state. The mutex
/// serializes only lazy initialization and state transitions; SQLite
/// transactions enforce the append-only claim/index invariants.
#[derive(Debug, Clone)]
pub(crate) struct IntentSidecar {
    path: Arc<PathBuf>,
    state: Arc<Mutex<SidecarState>>,
}

impl IntentSidecar {
    pub(crate) fn for_database_path(main_db_path: &Path) -> Self {
        Self {
            path: Arc::new(derive_sidecar_path(main_db_path)),
            state: Arc::new(Mutex::new(SidecarState::Unopened)),
        }
    }

    /// Atomically install a permanent consume-once tombstone.
    ///
    /// This is the only operation allowed to create an absent sidecar. Invalid
    /// identifiers are rejected before any filesystem access. A duplicate
    /// `draft_id` with the same hash is `AlreadyConsumed`; binding the same id
    /// to a different hash is a fail-closed `Conflict`. Neither result reveals
    /// the prior hash.
    pub(crate) async fn claim_draft(
        &self,
        draft_id: &str,
        draft_hash: &str,
        consumed_at_ms: i64,
    ) -> ClaimOutcome {
        let draft_id = match normalize_draft_id(draft_id) {
            Ok(draft_id) => draft_id,
            Err(error) => {
                tracing::warn!(error = %error, "intent sidecar rejected an invalid draft claim");
                return ClaimOutcome::Unavailable;
            }
        };
        if let Err(error) = validate_lower_hex("draft_hash", draft_hash, 64)
            .and_then(|()| validate_timestamp("consumed_at_ms", consumed_at_ms))
        {
            tracing::warn!(error = %error, "intent sidecar rejected an invalid draft claim");
            return ClaimOutcome::Unavailable;
        }

        let pool = match self.pool_for_claim().await {
            Ok(pool) => pool,
            Err(error) => {
                tracing::warn!(error = %error, "intent sidecar draft claim is unavailable");
                return ClaimOutcome::Unavailable;
            }
        };
        let result = claim_draft_in_pool(&pool, &draft_id, draft_hash, consumed_at_ms).await;
        match result {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::warn!(error = %error, "intent sidecar draft claim failed");
                self.mark_unavailable().await;
                ClaimOutcome::Unavailable
            }
        }
    }

    /// Append an immutable canonical-hash mapping.
    ///
    /// Ordering relative to the two main repository writes is intentionally the
    /// caller's responsibility. This method neither reads nor writes the main
    /// DB. The referenced draft must already have been claimed. Re-indexing the
    /// same `(canonical_rule_hash, intent_id)` pair is idempotent; any one-to-one
    /// identity conflict fails closed and is never overwritten.
    pub(crate) async fn index_hash(
        &self,
        canonical_rule_hash: &str,
        intent_id: &str,
        draft_id: &str,
        draft_hash: &str,
        indexed_at_ms: i64,
    ) -> IndexOutcome {
        let draft_id = match normalize_draft_id(draft_id) {
            Ok(draft_id) => draft_id,
            Err(error) => {
                tracing::warn!(error = %error, "intent sidecar rejected an invalid hash mapping");
                return IndexOutcome::Unavailable;
            }
        };
        if let Err(error) = validate_lower_hex("canonical_rule_hash", canonical_rule_hash, 64)
            .and_then(|()| validate_lower_hex("intent_id", intent_id, 32))
            .and_then(|()| validate_lower_hex("draft_hash", draft_hash, 64))
            .and_then(|()| validate_timestamp("indexed_at_ms", indexed_at_ms))
        {
            tracing::warn!(error = %error, "intent sidecar rejected an invalid hash mapping");
            return IndexOutcome::Unavailable;
        }

        let pool = match self.ready_pool().await {
            Ok(pool) => pool,
            Err(error) => {
                tracing::warn!(error = %error, "intent sidecar hash index is unavailable");
                return IndexOutcome::Unavailable;
            }
        };
        let result = index_hash_in_pool(
            &pool,
            canonical_rule_hash,
            intent_id,
            &draft_id,
            draft_hash,
            indexed_at_ms,
        )
        .await;
        match result {
            Ok(outcome) => outcome,
            Err(SidecarError::MappingConflict) => {
                tracing::warn!("intent sidecar rejected a conflicting hash mapping");
                self.mark_unavailable().await;
                IndexOutcome::Conflict
            }
            Err(error) => {
                tracing::warn!(error = %error, "intent sidecar hash indexing failed");
                self.mark_unavailable().await;
                IndexOutcome::Unavailable
            }
        }
    }

    /// Look up a derived mapping without creating or repairing the sidecar.
    ///
    /// Missing file, incompatible schema, corruption, and storage failure all
    /// return `Unavailable`. A valid file with no such hash returns `Missing`.
    /// Callers must revalidate every `Found` entry against both main
    /// repositories before resolving the user's hash.
    pub(crate) async fn lookup_hash(&self, canonical_rule_hash: &str) -> HashLookup {
        if validate_lower_hex("canonical_rule_hash", canonical_rule_hash, 64).is_err() {
            return HashLookup::Missing;
        }

        let Some(pool) = self.pool_for_lookup().await else {
            return HashLookup::Unavailable;
        };

        match sqlx::query_as::<_, HashIndexEntry>(SELECT_HASH_ENTRY_SQL)
            .bind(canonical_rule_hash)
            .fetch_optional(&pool)
            .await
        {
            Ok(Some(entry)) => HashLookup::Found {
                intent_id: entry.intent_id,
            },
            Ok(None) => HashLookup::Missing,
            Err(_) => {
                self.mark_unavailable().await;
                HashLookup::Unavailable
            }
        }
    }

    async fn pool_for_lookup(&self) -> Option<SqlitePool> {
        let mut state = self.state.lock().await;
        match &*state {
            SidecarState::Ready(pool) => return Some(pool.clone()),
            SidecarState::Unavailable => return None,
            SidecarState::Unopened => {}
        }

        let exists = match tokio::fs::try_exists(self.path.as_path()).await {
            Ok(exists) => exists,
            Err(_) => {
                *state = SidecarState::Unavailable;
                return None;
            }
        };
        if !exists {
            // Keep `Unopened`: a later finalize claim is allowed to create it.
            return None;
        }

        match open_existing(self.path.as_path()).await {
            Ok(pool) => {
                *state = SidecarState::Ready(pool.clone());
                Some(pool)
            }
            Err(_) => {
                *state = SidecarState::Unavailable;
                None
            }
        }
    }

    async fn pool_for_claim(&self) -> Result<SqlitePool, SidecarError> {
        let mut state = self.state.lock().await;
        match &*state {
            SidecarState::Ready(pool) => return Ok(pool.clone()),
            SidecarState::Unavailable => return Err(SidecarError::Unavailable),
            SidecarState::Unopened => {}
        }

        let exists = tokio::fs::try_exists(self.path.as_path())
            .await
            .map_err(SidecarError::Filesystem)?;
        let opened = if exists {
            open_existing(self.path.as_path()).await
        } else {
            create_new(self.path.as_path()).await
        };

        match opened {
            Ok(pool) => {
                *state = SidecarState::Ready(pool.clone());
                Ok(pool)
            }
            Err(error) => {
                *state = SidecarState::Unavailable;
                Err(error)
            }
        }
    }

    async fn ready_pool(&self) -> Result<SqlitePool, SidecarError> {
        let state = self.state.lock().await;
        match &*state {
            SidecarState::Ready(pool) => Ok(pool.clone()),
            SidecarState::Unavailable => Err(SidecarError::Unavailable),
            SidecarState::Unopened => Err(SidecarError::NotInitialized),
        }
    }

    async fn mark_unavailable(&self) {
        let mut state = self.state.lock().await;
        *state = SidecarState::Unavailable;
    }
}

fn connect_options(path: &Path, create_if_missing: bool) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(create_if_missing)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Delete)
        .synchronous(SqliteSynchronous::Full)
        .busy_timeout(Duration::from_secs(5))
}

async fn connect(path: &Path, create_if_missing: bool) -> Result<SqlitePool, SidecarError> {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(connect_options(path, create_if_missing))
        .await
        .map_err(SidecarError::Storage)
}

async fn create_new(path: &Path) -> Result<SqlitePool, SidecarError> {
    let pool = connect(path, true).await?;
    if let Err(error) = initialize_schema(&pool).await {
        pool.close().await;
        return Err(error);
    }
    Ok(pool)
}

async fn open_existing(path: &Path) -> Result<SqlitePool, SidecarError> {
    let pool = connect(path, false).await?;
    if let Err(error) = validate_existing(&pool).await {
        pool.close().await;
        return Err(error);
    }
    Ok(pool)
}

async fn initialize_schema(pool: &SqlitePool) -> Result<(), SidecarError> {
    let mut transaction = pool.begin().await.map_err(SidecarError::Storage)?;
    sqlx::query(CREATE_META_SQL)
        .execute(&mut *transaction)
        .await
        .map_err(SidecarError::Storage)?;
    sqlx::query(CREATE_CONSUMED_DRAFTS_SQL)
        .execute(&mut *transaction)
        .await
        .map_err(SidecarError::Storage)?;
    sqlx::query(CREATE_HASH_INDEX_SQL)
        .execute(&mut *transaction)
        .await
        .map_err(SidecarError::Storage)?;
    sqlx::query("INSERT INTO mcp_meta (singleton, schema_version) VALUES (1, ?)")
        .bind(SIDECAR_SCHEMA_VERSION)
        .execute(&mut *transaction)
        .await
        .map_err(SidecarError::Storage)?;
    transaction.commit().await.map_err(SidecarError::Storage)
}

async fn validate_existing(pool: &SqlitePool) -> Result<(), SidecarError> {
    let quick_check: String = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(pool)
        .await
        .map_err(SidecarError::Storage)?;
    if quick_check != "ok" {
        return Err(SidecarError::Integrity);
    }

    let schema_version: i64 =
        sqlx::query_scalar("SELECT schema_version FROM mcp_meta WHERE singleton = 1")
            .fetch_one(pool)
            .await
            .map_err(SidecarError::Storage)?;
    if schema_version != SIDECAR_SCHEMA_VERSION {
        return Err(SidecarError::UnsupportedSchema {
            found: schema_version,
        });
    }

    // Force SQLite to validate the two expected table shapes at open time.
    sqlx::query("SELECT draft_id, draft_hash, consumed_at_ms FROM consumed_drafts LIMIT 0")
        .execute(pool)
        .await
        .map_err(SidecarError::Storage)?;
    sqlx::query(
        "SELECT canonical_rule_hash, intent_id, draft_id, draft_hash, indexed_at_ms \
         FROM canonical_hash_index LIMIT 0",
    )
    .execute(pool)
    .await
    .map_err(SidecarError::Storage)?;

    Ok(())
}

async fn claim_draft_in_pool(
    pool: &SqlitePool,
    draft_id: &str,
    draft_hash: &str,
    consumed_at_ms: i64,
) -> Result<ClaimOutcome, SidecarError> {
    let mut transaction = pool.begin().await.map_err(SidecarError::Storage)?;
    let result = sqlx::query(
        "INSERT OR IGNORE INTO consumed_drafts \
         (draft_id, draft_hash, consumed_at_ms) VALUES (?, ?, ?)",
    )
    .bind(draft_id)
    .bind(draft_hash)
    .bind(consumed_at_ms)
    .execute(&mut *transaction)
    .await
    .map_err(SidecarError::Storage)?;

    let outcome = if result.rows_affected() == 1 {
        ClaimOutcome::Claimed
    } else {
        // Readback distinguishes a true duplicate from a constraint failure
        // hidden by INSERT OR IGNORE.
        let existing_hash: Option<String> =
            sqlx::query_scalar("SELECT draft_hash FROM consumed_drafts WHERE draft_id = ?")
                .bind(draft_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(SidecarError::Storage)?;
        match existing_hash {
            Some(existing_hash) if existing_hash == draft_hash => ClaimOutcome::AlreadyConsumed,
            Some(_) => ClaimOutcome::Conflict,
            None => return rollback_with(transaction, SidecarError::Integrity).await,
        }
    };

    transaction.commit().await.map_err(SidecarError::Storage)?;
    Ok(outcome)
}

async fn index_hash_in_pool(
    pool: &SqlitePool,
    canonical_rule_hash: &str,
    intent_id: &str,
    draft_id: &str,
    draft_hash: &str,
    indexed_at_ms: i64,
) -> Result<IndexOutcome, SidecarError> {
    let mut transaction = pool.begin().await.map_err(SidecarError::Storage)?;
    let result = sqlx::query(
        "INSERT OR IGNORE INTO canonical_hash_index \
         (canonical_rule_hash, intent_id, draft_id, draft_hash, indexed_at_ms) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(canonical_rule_hash)
    .bind(intent_id)
    .bind(draft_id)
    .bind(draft_hash)
    .bind(indexed_at_ms)
    .execute(&mut *transaction)
    .await
    .map_err(SidecarError::Storage)?;

    if result.rows_affected() == 1 {
        transaction.commit().await.map_err(SidecarError::Storage)?;
        return Ok(IndexOutcome::Indexed);
    }

    let by_hash = select_hash_entry(&mut transaction, canonical_rule_hash).await?;
    let by_intent = select_intent_entry(&mut transaction, intent_id).await?;
    let identical_pair = by_hash
        .as_ref()
        .is_some_and(|entry| entry.intent_id == intent_id)
        && by_intent
            .as_ref()
            .is_some_and(|entry| entry.canonical_rule_hash == canonical_rule_hash);

    if !identical_pair {
        return rollback_with(transaction, SidecarError::MappingConflict).await;
    }

    transaction.commit().await.map_err(SidecarError::Storage)?;
    Ok(IndexOutcome::AlreadyPresent)
}

async fn select_hash_entry(
    transaction: &mut Transaction<'_, Sqlite>,
    canonical_rule_hash: &str,
) -> Result<Option<HashIndexEntry>, SidecarError> {
    sqlx::query_as::<_, HashIndexEntry>(SELECT_HASH_ENTRY_SQL)
        .bind(canonical_rule_hash)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(SidecarError::Storage)
}

async fn select_intent_entry(
    transaction: &mut Transaction<'_, Sqlite>,
    intent_id: &str,
) -> Result<Option<HashIndexEntry>, SidecarError> {
    sqlx::query_as::<_, HashIndexEntry>(SELECT_INTENT_ENTRY_SQL)
        .bind(intent_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(SidecarError::Storage)
}

async fn rollback_with<T>(
    transaction: Transaction<'_, Sqlite>,
    error: SidecarError,
) -> Result<T, SidecarError> {
    transaction
        .rollback()
        .await
        .map_err(SidecarError::Storage)?;
    Err(error)
}

fn validate_lower_hex(
    field: &'static str,
    value: &str,
    expected_len: usize,
) -> Result<(), SidecarError> {
    if value.len() == expected_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(SidecarError::InvalidHex {
            field,
            expected_len,
        })
    }
}

fn normalize_draft_id(value: &str) -> Result<String, SidecarError> {
    let compact = match value.len() {
        32 => value.to_string(),
        36 => value
            .bytes()
            .enumerate()
            .filter_map(|(index, byte)| match index {
                8 | 13 | 18 | 23 if byte == b'-' => None,
                8 | 13 | 18 | 23 => Some(Err(())),
                _ => Some(Ok(char::from(byte))),
            })
            .collect::<Result<String, ()>>()
            .map_err(|()| SidecarError::InvalidHex {
                field: "draft_id",
                expected_len: 32,
            })?,
        _ => {
            return Err(SidecarError::InvalidHex {
                field: "draft_id",
                expected_len: 32,
            });
        }
    };
    validate_lower_hex("draft_id", &compact, 32)?;
    Ok(compact)
}

fn validate_timestamp(field: &'static str, value: i64) -> Result<(), SidecarError> {
    if value >= 0 {
        Ok(())
    } else {
        Err(SidecarError::InvalidTimestamp { field })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    const DRAFT_ID: &str = "11111111111111111111111111111111";
    const DRAFT_HASH: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const OTHER_DRAFT_HASH: &str =
        "3333333333333333333333333333333333333333333333333333333333333333";
    const CANONICAL_HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER_CANONICAL_HASH: &str =
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const INTENT_ID: &str = "cccccccccccccccccccccccccccccccc";
    const OTHER_INTENT_ID: &str = "dddddddddddddddddddddddddddddddd";

    static TEST_PATH_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TemporarySidecar {
        main_db_path: PathBuf,
        sidecar_path: PathBuf,
    }

    impl TemporarySidecar {
        fn unique(label: &str) -> Self {
            let sequence = TEST_PATH_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time must be after the Unix epoch")
                .as_nanos();
            let main_db_path = std::env::temp_dir().join(format!(
                "solfrontier-sidecar-{label}-{}-{timestamp}-{sequence}.db",
                std::process::id()
            ));
            let sidecar_path = derive_sidecar_path(&main_db_path);
            Self {
                main_db_path,
                sidecar_path,
            }
        }

        fn sidecar(&self) -> IntentSidecar {
            IntentSidecar::for_database_path(&self.main_db_path)
        }

        fn cleanup_paths(&self) -> [PathBuf; 5] {
            [
                self.main_db_path.clone(),
                self.sidecar_path.clone(),
                with_suffix(&self.sidecar_path, "-journal"),
                with_suffix(&self.sidecar_path, "-wal"),
                with_suffix(&self.sidecar_path, "-shm"),
            ]
        }

        fn cleanup_best_effort(&self) {
            let temporary_root = std::env::temp_dir();
            for _ in 0..20 {
                let mut retry = false;
                for path in self.cleanup_paths() {
                    if !path.starts_with(&temporary_root) {
                        return;
                    }
                    match fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => retry = true,
                    }
                }
                if !retry {
                    return;
                }
                // SQLite handles can remain visible briefly on Windows even
                // after every pool clone has been closed or dropped.
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for TemporarySidecar {
        fn drop(&mut self) {
            self.cleanup_best_effort();
        }
    }

    fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
        let mut value = OsString::from(path.as_os_str());
        value.push(suffix);
        PathBuf::from(value)
    }

    async fn close_sidecar(sidecar: &IntentSidecar) {
        let pool = {
            let state = sidecar.state.lock().await;
            match &*state {
                SidecarState::Ready(pool) => Some(pool.clone()),
                SidecarState::Unopened | SidecarState::Unavailable => None,
            }
        };
        if let Some(pool) = pool {
            pool.close().await;
        }
    }

    async fn claim_and_index(sidecar: &IntentSidecar) {
        assert_eq!(
            sidecar.claim_draft(DRAFT_ID, DRAFT_HASH, 100).await,
            ClaimOutcome::Claimed
        );
        assert_eq!(
            sidecar
                .index_hash(CANONICAL_HASH, INTENT_ID, DRAFT_ID, DRAFT_HASH, 101,)
                .await,
            IndexOutcome::Indexed
        );
    }

    #[test]
    fn derived_path_is_separate_from_the_main_database() {
        let files = TemporarySidecar::unique("path");
        let expected = with_suffix(&files.main_db_path, SIDECAR_SUFFIX);

        assert_ne!(files.sidecar_path, files.main_db_path);
        assert_eq!(files.sidecar_path, expected);
        assert_eq!(files.sidecar_path.parent(), files.main_db_path.parent());
        assert!(!files.main_db_path.exists());
        assert!(!files.sidecar_path.exists());
    }

    #[tokio::test]
    async fn lookup_with_no_sidecar_does_not_create_any_file() {
        let files = TemporarySidecar::unique("absent-lookup");
        let sidecar = files.sidecar();

        assert_eq!(
            sidecar.lookup_hash(CANONICAL_HASH).await,
            HashLookup::Unavailable
        );
        assert!(!files.main_db_path.exists());
        assert!(!files.sidecar_path.exists());

        let state = sidecar.state.lock().await;
        assert!(matches!(&*state, SidecarState::Unopened));
    }

    #[tokio::test]
    async fn first_claim_creates_only_the_sidecar() {
        let files = TemporarySidecar::unique("claim-create");
        let sidecar = files.sidecar();

        assert_eq!(
            sidecar.claim_draft(DRAFT_ID, DRAFT_HASH, 100).await,
            ClaimOutcome::Claimed
        );
        assert!(files.sidecar_path.exists());
        assert!(!files.main_db_path.exists());

        close_sidecar(&sidecar).await;
    }

    #[tokio::test]
    async fn repeated_identical_claim_is_already_consumed() {
        let files = TemporarySidecar::unique("claim-repeat");
        let sidecar = files.sidecar();

        assert_eq!(
            sidecar.claim_draft(DRAFT_ID, DRAFT_HASH, 100).await,
            ClaimOutcome::Claimed
        );
        assert_eq!(
            sidecar.claim_draft(DRAFT_ID, DRAFT_HASH, 200).await,
            ClaimOutcome::AlreadyConsumed
        );

        close_sidecar(&sidecar).await;
    }

    #[tokio::test]
    async fn repeated_draft_id_with_different_hash_is_a_conflict() {
        let files = TemporarySidecar::unique("claim-conflict");
        let sidecar = files.sidecar();

        assert_eq!(
            sidecar.claim_draft(DRAFT_ID, DRAFT_HASH, 100).await,
            ClaimOutcome::Claimed
        );
        assert_eq!(
            sidecar.claim_draft(DRAFT_ID, OTHER_DRAFT_HASH, 200).await,
            ClaimOutcome::Conflict
        );

        close_sidecar(&sidecar).await;
    }

    #[tokio::test]
    async fn indexed_hash_can_be_looked_up_and_reindexed_idempotently() {
        let files = TemporarySidecar::unique("index-lookup");
        let sidecar = files.sidecar();
        claim_and_index(&sidecar).await;

        assert_eq!(
            sidecar.lookup_hash(CANONICAL_HASH).await,
            HashLookup::Found {
                intent_id: INTENT_ID.to_string()
            }
        );
        assert_eq!(
            sidecar.lookup_hash(OTHER_CANONICAL_HASH).await,
            HashLookup::Missing
        );
        assert_eq!(
            sidecar
                .index_hash(CANONICAL_HASH, INTENT_ID, DRAFT_ID, DRAFT_HASH, 999,)
                .await,
            IndexOutcome::AlreadyPresent
        );

        close_sidecar(&sidecar).await;
    }

    #[tokio::test]
    async fn canonical_hash_conflict_fails_closed() {
        let files = TemporarySidecar::unique("hash-conflict");
        let sidecar = files.sidecar();
        claim_and_index(&sidecar).await;

        assert_eq!(
            sidecar
                .index_hash(CANONICAL_HASH, OTHER_INTENT_ID, DRAFT_ID, DRAFT_HASH, 102,)
                .await,
            IndexOutcome::Conflict
        );
        assert_eq!(
            sidecar.lookup_hash(CANONICAL_HASH).await,
            HashLookup::Unavailable
        );

        close_sidecar(&sidecar).await;
    }

    #[tokio::test]
    async fn intent_id_conflict_fails_closed() {
        let files = TemporarySidecar::unique("intent-conflict");
        let sidecar = files.sidecar();
        claim_and_index(&sidecar).await;

        assert_eq!(
            sidecar
                .index_hash(OTHER_CANONICAL_HASH, INTENT_ID, DRAFT_ID, DRAFT_HASH, 102,)
                .await,
            IndexOutcome::Conflict
        );
        assert_eq!(
            sidecar.lookup_hash(CANONICAL_HASH).await,
            HashLookup::Unavailable
        );

        close_sidecar(&sidecar).await;
    }

    #[tokio::test]
    async fn corrupt_sidecar_lookup_is_unavailable_without_panicking() {
        let files = TemporarySidecar::unique("corrupt");
        fs::write(&files.sidecar_path, b"not a sqlite database")
            .expect("write corrupt sidecar fixture");
        let original_bytes = fs::read(&files.sidecar_path).expect("read corrupt sidecar fixture");
        let sidecar = files.sidecar();

        assert_eq!(
            sidecar.lookup_hash(CANONICAL_HASH).await,
            HashLookup::Unavailable
        );
        assert_eq!(
            sidecar.lookup_hash(CANONICAL_HASH).await,
            HashLookup::Unavailable
        );
        assert_eq!(
            fs::read(&files.sidecar_path).expect("re-read corrupt sidecar fixture"),
            original_bytes
        );

        close_sidecar(&sidecar).await;
    }
}
