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
//! API shape verified against rmcp 1.7 / rust-sdk main-branch counter example
//! (tool_router field + Parameters wrapper + CallToolResult). Authored in a
//! no-build sandbox: run `cargo check` first and fix any residual drift.

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData as McpError, ServerHandler, ServiceExt,
};

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
    /// Canonical intent hash (SHA-256 hex) or intent UUID
    intent_ref: String,
}

// ── Server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
struct SolFrontierServer {
    tool_router: ToolRouter<Self>,
    // Phase 1 wiring targets (add as you connect real backends):
    // rpc: std::sync::Arc<claw_solana_core::ClawRpcClient>,
    // store: std::sync::Arc<claw_state_store::StateStore>,
}

#[tool_router]
impl SolFrontierServer {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
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
        Ok(CallToolResult::success(vec![Content::text(body.to_string())]))
    }

    /// READ-ONLY. Solend position for a wallet.
    #[tool(description = "Get current Solend position for a wallet (read-only)")]
    async fn get_position(
        &self,
        Parameters(p): Parameters<GetPositionParams>,
    ) -> Result<CallToolResult, McpError> {
        // TODO(Phase1): port from 舊 repo crates/gateway/src/tools/get_solend_position.rs
        let body = serde_json::json!({ "status": "stub", "wallet": p.wallet });
        Ok(CallToolResult::success(vec![Content::text(body.to_string())]))
    }

    /// READ-ONLY. Bounded-intent lifecycle state from the state store.
    #[tool(
        description = "Get lifecycle status of a bounded intent by canonical hash or UUID (read-only)"
    )]
    async fn get_intent_status(
        &self,
        Parameters(p): Parameters<GetIntentStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        // TODO(Phase1): query claw-state-store durable_pending / lifecycle tables.
        let body = serde_json::json!({ "status": "stub", "intent_ref": p.intent_ref });
        Ok(CallToolResult::success(vec![Content::text(body.to_string())]))
    }
}

const INSTRUCTIONS: &str = "SolFrontier is a fail-closed, policy-gated control plane for bounded \
Solana DeFi intents. The AI proposes; only humans approve and sign (INV-1). All tools in Phase 1 \
are read-only: nothing here builds, signs, or submits a transaction. Funding/signing always happens \
in the user's own wallet (Phantom) via a separate signing page, never through this server.";

#[tool_handler]
impl ServerHandler for SolFrontierServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            instructions: Some(INSTRUCTIONS.to_string()),
            ..Default::default()
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // stdio transport ⇒ 日誌必須走 stderr,stdout 專屬 JSON-RPC。
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();

    tracing::info!("solfrontier-mcp starting (stdio, Phase 1 read-only)");
    let service = SolFrontierServer::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
