//! solfrontier-mcp — MCP stdio server for the SolFrontier bounded-intent control plane.
//!
//! Phase 1 scope (docs/重構建議書.md §4):
//!   three READ-ONLY tools — get_quote / get_position / get_intent_status —
//!   wired to Claude Desktop over stdio. No write path, no signing, no watcher.
//!
//! System invariants INV-1..INV-8 (原 ARCHITECTURE.md) apply verbatim.
//! In particular: this binary must NEVER hold main-wallet key material,
//! and no tool may sign or submit a transaction in Phase 1.
//!
//! API shape follows the official rmcp 1.7 counter/calculator examples
//! (Parameters wrapper + CallToolResult).

#[cfg(test)]
mod dev_seed;
mod position;
mod solend_raw;
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

use crate::position::{configured_reader_from_env, query_position, RpcSolendPositionReader};
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
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct GetPositionParams {
    /// Wallet public key (base58)
    wallet: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct GetIntentStatusParams {
    /// Intent UUID. A canonical SHA-256 hash currently returns `unsupported_ref`.
    intent_ref: String,
}

// ── Server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
struct SolFrontierServer {
    funding_intents: Stage2W5hFundingIntentRepository,
    watch_rules: Stage2WatchRuleRepository,
    position_reader: Option<RpcSolendPositionReader>,
}

#[tool_router]
impl SolFrontierServer {
    fn new(db: &Database, position_reader: Option<RpcSolendPositionReader>) -> Self {
        Self {
            funding_intents: Stage2W5hFundingIntentRepository::new(db.pool().clone()),
            watch_rules: Stage2WatchRuleRepository::new(db.pool().clone()),
            position_reader,
        }
    }

    /// READ-ONLY. Jupiter quote preview — no transaction is built or signed.
    #[tool(description = "Get a Jupiter swap quote (read-only preview, no transaction)")]
    async fn get_quote(
        &self,
        Parameters(p): Parameters<GetQuoteParams>,
    ) -> Result<CallToolResult, McpError> {
        // TODO(Phase1): port logic from 舊 repo crates/gateway/src/tools/get_jupiter_quote.rs
        // 純 builder:給定 mints+amount → 呼叫 Jupiter quote API → 回傳 JSON 摘要。
        let body = serde_json::json!({
            "status": "stub",
            "input_mint": p.input_mint,
            "output_mint": p.output_mint,
            "amount": p.amount,
        });
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
        description = "Get bounded-intent lifecycle status by UUID (read-only); canonical hash lookup currently returns unsupported_ref"
    )]
    async fn get_intent_status(
        &self,
        Parameters(p): Parameters<GetIntentStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let response = query_intent_status(&self.funding_intents, &self.watch_rules, &p.intent_ref)
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
Solana DeFi intents. The AI proposes; only humans approve and sign (INV-1). All tools in Phase 1 \
are read-only: nothing here builds, signs, or submits a transaction. Funding/signing always happens \
in the user's own wallet (Phantom) via a separate signing page, never through this server.";

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

    tracing::info!("solfrontier-mcp starting (stdio, Phase 1 read-only)");
    let position_reader = configured_reader_from_env();
    let db = Database::open(&DatabaseConfig {
        path: cli.db.to_string_lossy().into_owned(),
        ..DatabaseConfig::default()
    })
    .await?;
    let service = SolFrontierServer::new(&db, position_reader)
        .serve(stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}
