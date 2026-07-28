//! solfrontier-mcp — MCP stdio server for the SolFrontier bounded-intent control plane.
//!
//! Phase 1 is complete: get_quote / get_position / get_intent_status are
//! wired to real read backends over stdio. Phase 2 starts with
//! propose_intent, a pure draft calculator with no persistence, network,
//! signing, transaction construction, or watcher path.
//!
//! System invariants INV-1..INV-8 (原 ARCHITECTURE.md) apply verbatim.
//! In particular: this binary must NEVER hold main-wallet key material,
//! and no tool in the current proposal-only slice may persist, sign, or
//! submit a transaction.
//!
//! API shape follows the official rmcp 1.7 counter/calculator examples
//! (Parameters wrapper + CallToolResult).

#[cfg(test)]
mod dev_seed;
mod position;
mod propose;
mod quote;
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
use crate::propose::{propose_intent_json, ProposeIntentParams};
use crate::quote::{configured_client_from_env, query_quote, HttpJupiterClient};
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
    /// Intent UUID. A canonical SHA-256 hash currently returns `unsupported_ref`.
    intent_ref: String,
}

// ── Server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
struct SolFrontierServer {
    funding_intents: Stage2W5hFundingIntentRepository,
    watch_rules: Stage2WatchRuleRepository,
    position_reader: Option<RpcSolendPositionReader>,
    quote_source: HttpJupiterClient,
}

#[tool_router]
impl SolFrontierServer {
    fn new(
        db: &Database,
        position_reader: Option<RpcSolendPositionReader>,
        quote_source: HttpJupiterClient,
    ) -> Self {
        Self {
            funding_intents: Stage2W5hFundingIntentRepository::new(db.pool().clone()),
            watch_rules: Stage2WatchRuleRepository::new(db.pool().clone()),
            position_reader,
            quote_source,
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
Solana DeFi intents. The AI proposes; only humans approve and sign (INV-1). Current tools are \
read-only or pure draft calculations: nothing here persists a proposal, builds, signs, or submits \
a transaction. Funding/signing always happens in the user's own wallet (Phantom) via a separate \
signing page, never through this server.";

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

    tracing::info!("solfrontier-mcp starting (stdio, Phase 2 proposal-only slice)");
    let position_reader = configured_reader_from_env();
    let quote_source = configured_client_from_env();
    let db = Database::open(&DatabaseConfig {
        path: cli.db.to_string_lossy().into_owned(),
        ..DatabaseConfig::default()
    })
    .await?;
    let service = SolFrontierServer::new(&db, position_reader, quote_source)
        .serve(stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}
