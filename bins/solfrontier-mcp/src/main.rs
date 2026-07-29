//! solfrontier-mcp — MCP stdio server for the SolFrontier bounded-intent control plane.
//!
//! Phase 1 is complete: get_quote / get_position / get_intent_status are
//! wired to real read backends over stdio. Phase 2 adds a pure
//! `propose_intent` calculator and an explicitly write-capable
//! `finalize_intent` bridge. Finalize verifies the proposal hash, consumes a
//! random non-canonical draft id, and creates the legacy-compatible WatchRule
//! plus funding-intent rows. A separate `confirm_funding` tool verifies an
//! already user-signed transaction before advancing the two funding CAS
//! transitions. The binary still performs no signing, transaction
//! construction, broadcast, or watcher/executor action.
//!
//! System invariants INV-1..INV-8 (原 ARCHITECTURE.md) apply verbatim.
//! In particular: this binary must NEVER hold main-wallet key material,
//! and no tool may sign or submit a transaction.
//!
//! API shape follows the official rmcp 1.7 counter/calculator examples
//! (Parameters wrapper + CallToolResult).

#[cfg(test)]
mod dev_seed;
mod finalize;
mod finalize_market;
mod funding;
mod position;
mod propose;
mod quote;
mod sidecar;
mod status;

use clap::Parser;
use claw_state_store::{
    Database, DatabaseConfig, Stage2W5hFundingIntentRepository, Stage2WatchRuleRepository,
};
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};
use std::path::PathBuf;

use crate::finalize::{finalize_intent_json, FinalizeIntentParams, SystemFinalizeClock};
use crate::finalize_market::{
    configured_finalize_market_source_from_env, RpcSaveFinalizeMarketSource,
};
use crate::funding::{
    configured_funding_reader_from_env, confirm_funding_json, ConfirmFundingParams,
    RpcFundingTransactionReader, SystemFundingClock,
};
use crate::position::{configured_reader_from_env, query_position, RpcSolendPositionReader};
use crate::propose::{propose_intent_json, ProposeIntentParams};
use crate::quote::{configured_client_from_env, query_quote, HttpJupiterClient};
use crate::sidecar::IntentSidecar;
use crate::status::query_intent_status;

// ── Tool parameter types (schemars 1.x → JSON Schema, host 端自動看到) ──

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct GetQuoteParams {
    /// Input mint address (e.g. USDC mint)
    input_mint: String,
    /// Output mint address
    output_mint: String,
    /// Amount in base units (u64 as string to avoid JS precision loss)
    amount: String,
    /// Maximum slippage in basis points. Values above 100 are policy-blocked.
    slippage_bps: u64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct GetPositionParams {
    /// Wallet public key (base58)
    wallet: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct GetIntentStatusParams {
    /// Intent UUID or canonical rule SHA-256 hash.
    intent_ref: String,
}

// ── Server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
struct SolFrontierServer {
    funding_intents: Stage2W5hFundingIntentRepository,
    watch_rules: Stage2WatchRuleRepository,
    position_reader: Option<RpcSolendPositionReader>,
    quote_source: HttpJupiterClient,
    finalize_market_source: Option<RpcSaveFinalizeMarketSource>,
    funding_reader: Option<RpcFundingTransactionReader>,
    intent_sidecar: IntentSidecar,
}

#[tool_router]
impl SolFrontierServer {
    fn new(
        db: &Database,
        position_reader: Option<RpcSolendPositionReader>,
        quote_source: HttpJupiterClient,
        finalize_market_source: Option<RpcSaveFinalizeMarketSource>,
        funding_reader: Option<RpcFundingTransactionReader>,
        intent_sidecar: IntentSidecar,
    ) -> Self {
        Self {
            funding_intents: Stage2W5hFundingIntentRepository::new(db.pool().clone()),
            watch_rules: Stage2WatchRuleRepository::new(db.pool().clone()),
            position_reader,
            quote_source,
            finalize_market_source,
            funding_reader,
            intent_sidecar,
        }
    }

    /// PURE. Validate a typed conditional-deposit draft and compute its hash.
    #[tool(
        description = "Propose a typed Solend USDC conditional-deposit draft and compute its canonical draft hash (pure calculation; no DB row, network, signature, or transaction)"
    )]
    async fn propose_intent(
        &self,
        Parameters(p): Parameters<ProposeIntentParams>,
    ) -> Result<CallToolResult, McpError> {
        let body = propose_intent_json(&p);
        Ok(CallToolResult::success(vec![Content::text(
            body.to_string(),
        )]))
    }

    /// WRITE. Verify an already-signed funding transfer before advancing state.
    #[tool(
        description = "WRITE DATABASE: verify a confirmed Solana funding transaction against the finalized intent, then advance funding_required to budget_reserved; this server never signs or broadcasts"
    )]
    async fn confirm_funding(
        &self,
        Parameters(p): Parameters<ConfirmFundingParams>,
    ) -> Result<CallToolResult, McpError> {
        let body = confirm_funding_json(
            &p,
            self.funding_reader.as_ref(),
            &self.funding_intents,
            &self.watch_rules,
            &SystemFundingClock,
        )
        .await
        .map_err(|_| {
            // Never interpolate provider/state-store errors here. RPC failures
            // are normal sanitized JSON results; a DB failure may happen after
            // the first CAS, so retrying the exact same identifiers is safe.
            tracing::error!("confirm_funding state-store operation failed");
            McpError::internal_error(
                "confirm_funding state-store operation failed; retry the same intent_id and tx_signature",
                None,
            )
        })?;
        Ok(CallToolResult::success(vec![Content::text(
            body.to_string(),
        )]))
    }

    /// WRITE. Consume an approved draft and create its funding-required rows.
    #[tool(
        description = "WRITE DATABASE: finalize an approved typed draft after rechecking its draft_hash; creates the WatchRule and funding-intent rows, but never signs or submits a transaction"
    )]
    async fn finalize_intent(
        &self,
        Parameters(p): Parameters<FinalizeIntentParams>,
    ) -> Result<CallToolResult, McpError> {
        let body = finalize_intent_json(
            &p,
            self.finalize_market_source.as_ref(),
            &self.intent_sidecar,
            &self.watch_rules,
            &self.funding_intents,
            &SystemFinalizeClock,
        )
        .await
        .map_err(|error| {
            tracing::error!(error = %error, "finalize_intent state-store operation failed");
            McpError::internal_error("finalize state-store operation failed", None)
        })?;
        Ok(CallToolResult::success(vec![Content::text(
            body.to_string(),
        )]))
    }

    /// READ-ONLY. Jupiter quote preview — no transaction is built or signed.
    #[tool(
        description = "Get a SOL/USDC Jupiter quote (read-only preview, max 100 bps slippage, no transaction)"
    )]
    async fn get_quote(
        &self,
        Parameters(p): Parameters<GetQuoteParams>,
    ) -> Result<CallToolResult, McpError> {
        let body = query_quote(
            &self.quote_source,
            &p.input_mint,
            &p.output_mint,
            &p.amount,
            p.slippage_bps,
        )
        .await;
        Ok(CallToolResult::success(vec![Content::text(
            body.to_string(),
        )]))
    }

    /// READ-ONLY. Solend position for a wallet.
    #[tool(description = "Get current Solend position for a wallet (read-only)")]
    async fn get_position(
        &self,
        Parameters(p): Parameters<GetPositionParams>,
    ) -> Result<CallToolResult, McpError> {
        let body = query_position(self.position_reader.as_ref(), &p.wallet).await;
        Ok(CallToolResult::success(vec![Content::text(
            body.to_string(),
        )]))
    }

    /// READ-ONLY. Bounded-intent lifecycle state from the state store.
    #[tool(
        description = "Get bounded-intent lifecycle status by UUID or indexed canonical rule hash (read-only; missing/corrupt derived index degrades to unsupported_ref)"
    )]
    async fn get_intent_status(
        &self,
        Parameters(p): Parameters<GetIntentStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let response = query_intent_status(
            &self.funding_intents,
            &self.watch_rules,
            &self.intent_sidecar,
            &p.intent_ref,
        )
        .await
        .map_err(|error| {
            tracing::error!(error = %error, "get_intent_status state-store query failed");
            McpError::internal_error("state-store query failed", None)
        })?;
        let body = serde_json::to_string(&response).map_err(|error| {
            tracing::error!(error = %error, "get_intent_status response serialization failed");
            McpError::internal_error("intent status serialization failed", None)
        })?;
        Ok(CallToolResult::success(vec![Content::text(body)]))
    }
}

#[derive(Debug, Parser)]
#[command(name = "solfrontier-mcp")]
struct Cli {
    /// SQLite state-store path.
    #[arg(
        long,
        env = "SOLFRONTIER_DB",
        default_value = "./data/solfrontier.db",
        value_name = "PATH"
    )]
    db: PathBuf,
}

const INSTRUCTIONS: &str = "SolFrontier is a fail-closed, policy-gated control plane for bounded \
Solana DeFi intents. The AI proposes; only humans approve and sign (INV-1). finalize_intent is an \
explicitly labeled database-writing tool: it persists an approved rule and funding requirement, \
but it never builds, signs, or submits a transaction. confirm_funding is also explicitly labeled: \
it only advances database state after a confirmed transaction passes every funding check. \
Funding/signing happens in the user's own wallet (Phantom) via a separate static signing page, \
never through this server.";

#[tool_handler]
impl ServerHandler for SolFrontierServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(INSTRUCTIONS)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // stdio transport ⇒ 日誌必須走 stderr,stdout 專屬 JSON-RPC。
    tracing_subscriber::fmt()
        // Solana's HTTP client has debug events that format a response URL.
        // Keep a hard INFO ceiling so an API-key-bearing endpoint cannot leak.
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("solfrontier-mcp starting (stdio, Phase 2 funding slice)");
    let position_reader = configured_reader_from_env();
    let quote_source = configured_client_from_env();
    let finalize_market_source = configured_finalize_market_source_from_env();
    let funding_reader = configured_funding_reader_from_env();
    let intent_sidecar = IntentSidecar::for_database_path(&cli.db);
    let db = Database::open(&DatabaseConfig {
        path: cli.db.to_string_lossy().into_owned(),
        ..DatabaseConfig::default()
    })
    .await?;
    let service = SolFrontierServer::new(
        &db,
        position_reader,
        quote_source,
        finalize_market_source,
        funding_reader,
        intent_sidecar,
    )
    .serve(stdio())
    .await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::propose::tests::valid_params;

    #[derive(Debug, PartialEq, Eq)]
    struct FileFingerprint {
        exists: bool,
        bytes: Vec<u8>,
        modified: Option<SystemTime>,
    }

    struct TemporaryDatabase {
        path: PathBuf,
    }

    impl TemporaryDatabase {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "solfrontier-propose-no-write-{}-{nonce}.db",
                std::process::id()
            ));
            assert!(path.starts_with(std::env::temp_dir()));
            Self { path }
        }

        fn family_paths(&self) -> [PathBuf; 3] {
            [
                self.path.clone(),
                with_suffix(&self.path, "-wal"),
                with_suffix(&self.path, "-shm"),
            ]
        }

        fn snapshot(&self) -> Vec<FileFingerprint> {
            self.family_paths()
                .iter()
                .map(|path| {
                    if !path.exists() {
                        return FileFingerprint {
                            exists: false,
                            bytes: Vec::new(),
                            modified: None,
                        };
                    }
                    FileFingerprint {
                        exists: true,
                        bytes: fs::read(path).expect("read SQLite fingerprint"),
                        modified: Some(
                            fs::metadata(path)
                                .expect("SQLite metadata")
                                .modified()
                                .expect("SQLite modified time"),
                        ),
                    }
                })
                .collect()
        }

        fn cleanup_best_effort(&self) -> bool {
            let temporary_root = std::env::temp_dir();
            for _ in 0..20 {
                let mut retry = false;
                for path in self.family_paths() {
                    if !path.starts_with(&temporary_root) {
                        return false;
                    }
                    match fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(_) => retry = true,
                    }
                }
                if !retry {
                    return true;
                }
                // Windows can retain SQLite handles briefly after every pool
                // clone is dropped. Cleanup must never turn that lag into a
                // test failure.
                std::thread::sleep(Duration::from_millis(10));
            }
            false
        }
    }

    impl Drop for TemporaryDatabase {
        fn drop(&mut self) {
            let _ = self.cleanup_best_effort();
        }
    }

    fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
        let mut value = OsString::from(path.as_os_str());
        value.push(suffix);
        PathBuf::from(value)
    }

    async fn wait_for_stable_fingerprint(database: &TemporaryDatabase) -> Vec<FileFingerprint> {
        let mut previous = database.snapshot();
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let current = database.snapshot();
            if current == previous {
                return current;
            }
            previous = current;
        }
        panic!("SQLite files did not settle after startup migrations");
    }

    #[test]
    fn propose_tool_leaves_migrated_database_bytes_and_mtime_unchanged() {
        let temporary_database = TemporaryDatabase::new();
        let runtime = tokio::runtime::Runtime::new().expect("create isolated test runtime");
        let (before, after) = runtime.block_on(async {
            let sidecar_path = crate::sidecar::derive_sidecar_path(&temporary_database.path);
            assert!(
                !sidecar_path.exists(),
                "test starts without a finalize sidecar"
            );
            let db = Database::open(&DatabaseConfig {
                path: temporary_database.path.to_string_lossy().into_owned(),
                ..DatabaseConfig::default()
            })
            .await
            .expect("open migrated test database");
            let server = SolFrontierServer::new(
                &db,
                None,
                configured_client_from_env(),
                None,
                None,
                IntentSidecar::for_database_path(&temporary_database.path),
            );

            // Startup migrations are allowed. Close every shared pool connection,
            // then establish the baseline so only the tool call is measured.
            db.pool().close().await;
            let before = wait_for_stable_fingerprint(&temporary_database).await;

            let result = server
                .propose_intent(Parameters(valid_params()))
                .await
                .expect("propose tool call");

            let result_json = serde_json::to_value(result).expect("serialize tool result");
            let body: serde_json::Value = serde_json::from_str(
                result_json["content"][0]["text"]
                    .as_str()
                    .expect("text tool content"),
            )
            .expect("parse proposal JSON");
            assert_eq!(body["status"], "ok");
            assert_eq!(body["draft_hash"].as_str().expect("draft hash").len(), 64);

            let after = temporary_database.snapshot();
            assert!(
                !sidecar_path.exists(),
                "propose_intent must not create its finalize sidecar"
            );
            drop(server);
            drop(db);
            (before, after)
        });
        drop(runtime);
        assert!(
            temporary_database.cleanup_best_effort(),
            "temporary SQLite files must be removable after the isolated runtime stops"
        );
        assert_eq!(
            after, before,
            "propose_intent must not change SQLite bytes, sidecars, or mtimes"
        );
    }
}
