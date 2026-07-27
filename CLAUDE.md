# CLAUDE.md — solfrontier-mcp

## 這個 repo 是什麼

SolFrontier(前身 ClawSolana / Solfrontier2026)的 MCP 重構版:一個 policy-gated、fail-closed 的 Solana bounded-intent 控制面,以 **MCP stdio server** 形式暴露工具,由外部 MCP host(Claude Desktop / Claude Code)取代舊有的自製 agent 層。

**開工前必讀:`docs/重構建議書.md`** — 完整的現況診斷、目標架構、四階段遷移路線。本檔只是摘要。

舊程式碼參考(不在本 repo,需要時叫用戶提供路徑或 clone):
- `github.com/jeffreycheung521hk/Solfrontier2026`(deadline 後修正版,以此為準)
- `github.com/jeffreycheung521hk/testingcrypto2`(Hackathon 提交版,含 ARCHITECTURE.md / DEBT.md 全文)

## 系統不變量(絕對,任何 PR 不得違反)

繼承原 ARCHITECTURE.md 的 INV-1 ~ INV-8,重點:

- **INV-1** Human signs, AI proposes — 本 repo 任何程式碼不得自動簽名或持有主錢包私鑰。approve 類 tool 永遠經過 host 的人手確認 + risk-engine policy gate 雙層閘。
- **INV-2** Typestate 管線(`Proposal → Simulated → Approved → Signed`)不可繞過、不可改造 — `crates/wallet-engine` 是 "do not touch" 區。
- **INV-3** Audit trail 只加不改(state-store AuditRepository 無 update/delete)。
- **INV-5** 簽名前必先 simulation,無跳過旗標。
- **INV-6** Fail-closed:policy 不匹配 → 需人手;檢查失敗 → block。
- **INV-7** local-only:stdio transport,不開 HTTP port。加遠端傳輸需先補 auth 並另行評審。
- **INV-8** 核心 crates `#![forbid(unsafe_code)]`。

## Crate 地圖

| 位置 | 狀態 | 說明 |
|---|---|---|
| `crates/types` `observability` `state-store` `solana-core` `wallet-engine` `risk-engine` | **原封搬入,不動邊界** | 來自 Solfrontier2026,是 mainnet proof 的信任錨 |
| `bins/solfrontier-mcp` | 新寫 | rmcp stdio server;唯一的新程式碼落點 |
| (未來) `crates/protocols/` | Phase 3 | Jupiter/Solend builders,從舊 gateway 抽取 |
| (未來) `crates/executor` | Phase 3 | watcher + CAS lease + controlled-wallet executor |

**禁止**重新引入:自製 LLM client / ReAct loop(舊 agent-runtime)、HTTP API surface(舊 api crate)、chat UI。這些由 MCP host 提供。

## 目前階段:Phase 1(唯讀)

目標:`get_quote` / `get_position` / `get_intent_status` 三個唯讀 tools 在 Claude Desktop 端到端跑通。

1. `bins/solfrontier-mcp/src/main.rs` 是**未經編譯**的骨架(在無法編譯的沙盒中寫成)。第一件事:`cargo check`,對照 docs.rs/rmcp(1.7.x,以 crates.io 最新版為準)修正宏/import 偏差。
2. 逐一把三個 stub 接上真實後端(來源檔在舊 repo `crates/gateway/src/tools/`,見 main.rs 內 TODO)。
3. 接 Claude Desktop 驗證(`claude_desktop_config.json` 加 stdio server 條目)。
4. Phase 2+ 見建議書 §4;不要跳階段。

## 工程紀律

- 體積是一級關注:release profile 已設 `opt-level="z"` / `lto="fat"` / `panic="abort"`。新增依賴前先問「會拖進多大的樹」;定期 `cargo bloat --release --crates`。
- 依賴收窄的 TODO 標記在根 `Cargo.toml`(tokio features、pubsub-client、token-2022)— 動它們時先 grep 用途。
- stdio server 的 stdout 專屬 JSON-RPC:**所有日誌走 stderr**(已在 main.rs 設定,勿改)。
- 格式化只用 `cargo fmt -p solfrontier-mcp`,永不使用 `cargo fmt --all`;每個 commit 前必須跑 `git diff --exit-code -- crates`,確認六個核心 crate 零 diff。
- 大型搬移遵守舊 DEBT.md 的 PC-1/PC-2/PC-3(路徑審計;一 PR 一主題、不混邏輯改動;由低風險到高風險)。
- 測試命名用語意名,不要沿用舊 repo 的 prompt-session 前綴(p1_/n6_/w5h_)。

## 技術債

### DEBT-MCP-1:canonical hash 尚無公開反查 API

- **現況:**`Stage2W5hFundingIntentRepository::get` 只接受 32-hex `intent_id`,`Stage2WatchRuleRepository::get` 只接受 16-byte rule UUID。64-hex canonical hash 無公開 lookup;`get_intent_status` 對這類輸入回正常 JSON `status:"unsupported_ref"`,不冒充 `not_found` 或 MCP protocol error。
- **為何暫不修:**Phase 1 的約束是不動六個核心 crate 及其邊界;在 bin 直接讀 table 或複製 SQL 也會繞過 `claw-state-store` 的公開 repository 邊界。
- **觸發條件:**Phase 2 的 `finalize_intent` 會把 canonical hash 交給用戶,屆時 status-by-hash 成為必要能力。
- **屆時二選一:**(1)在 `claw-state-store` 增加公開 hash lookup API,視為正式邊界變更並單獨評審;(2)由 MCP bin 維護自己的 `canonical_hash → intent_id` 索引,保持核心 crate 邊界不變。
