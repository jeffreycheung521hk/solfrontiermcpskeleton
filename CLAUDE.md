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

## 目前階段:Phase 2(草稿提案 → finalize)

Phase 1 已完成:`get_quote`、`get_position`、`get_intent_status` 三個唯讀 tools 已分別接上 Jupiter、Solana RPC/Solend 與 state-store 真實後端,並通過 stdio MCP 驗證。

Phase 2 第一切片 `propose_intent` 已完成;第二切片加入明確標示會寫 DB 的 `finalize_intent`:

1. MCP host 直接提供由 schemars schema 驗證的 typed 參數;不移植舊 `stage2_llm_intent_extractor`。
2. `propose_intent` 只驗證條件、精確解析 USDC 金額、計算向後相容的 canonical draft hash,並產生隨機且非正典的 `draft_id`;此時 `No DB row exists at this point`。`draft_id` 不得進入 draft hash 或 canonical rule hash 的 preimage。
3. `finalize_intent` 重算並核對 draft hash,在 bin 自有 sidecar consume `draft_id`,讀取可信的 Solana slot/native APR 與 Save APY,再依舊系統的非事務順序建立 WatchRule 與 funding intent。此 tool 會寫資料庫,但仍然零簽名、零交易建構、零廣播。
4. Controlled wallet/ATA 延續舊系統固定值;外部出資者由 finalize 的 `user_wallet` 明確提供,不得用 controlled wallet 冒充。
5. 入金簽名頁、`confirm_funding`、watcher/executor 都不在本切片;Phase 2 後續見建議書 §4,每個寫路徑切片必須獨立評審。

## 工程紀律

- 體積是一級關注:release profile 已設 `opt-level="z"` / `lto="fat"` / `panic="abort"`。新增依賴前先問「會拖進多大的樹」;定期 `cargo bloat --release --crates`。
- 依賴收窄的 TODO 標記在根 `Cargo.toml`(tokio features、pubsub-client、token-2022)— 動它們時先 grep 用途。
- stdio server 的 stdout 專屬 JSON-RPC:**所有日誌走 stderr**(已在 main.rs 設定,勿改)。
- 格式化只用 `cargo fmt -p solfrontier-mcp`,永不使用 `cargo fmt --all`;每個 commit 前必須跑 `git diff --exit-code -- crates`,確認六個核心 crate 零 diff。
- 大型搬移遵守舊 DEBT.md 的 PC-1/PC-2/PC-3(路徑審計;一 PR 一主題、不混邏輯改動;由低風險到高風險)。
- 測試命名用語意名,不要沿用舊 repo 的 prompt-session 前綴(p1_/n6_/w5h_)。

## 技術債

### DEBT-MCP-1（已關閉）:canonical hash 公開反查缺口

- **關閉方式:**Phase 2 finalize 在 MCP bin 維護與主 DB 分離的衍生 sidecar,記錄 `canonical_rule_hash → intent_id`;沒有修改六個核心 crate、repository API 或主資料庫 schema,也沒有從 bin 對主 DB 寫直接 SQL。
- **唯一真相源:**主 DB 的 WatchRule 與 funding intent 是唯一真相。sidecar 只負責解析識別碼;每次命中後仍須透過公開 repository API 讀回 WatchRule、重算並核對 canonical rule hash,若 funding intent 存在也必須核對其 hash/ID,不得直接信任 sidecar。
- **優雅降級:**sidecar 遺失、損毀、schema 不相容、無映射或映射與主 DB 不一致時,hash 反查回正常 JSON `status:"unsupported_ref"`,不崩潰、不誤答;`intent_id`/UUID 查詢仍以主 DB 為準。

### DEBT-P2-FINALIZE-1:rule-only row

- **現況:**為逐字保留舊 bridge 的非事務語意,finalize 先寫 WatchRule,再寫 sidecar 與 funding intent。若程序在第一個主 DB 寫入後中斷,或後續 funding insert 失敗,會留下沒有 funding row 的 rule-only row;沒有 rollback 或 startup reconciler。
- **目前處理:**draft 已在網絡讀取與主 DB 寫入前被 consume;失敗後必須重新 propose。新 draft 若碰到相同 `rule_id`,沿用舊 collision/readback 路徑嘗試補上 funding row。
- **觸發條件:**`Phase 3 executor 上線前,必須決定 watcher 如何對待 orphan/rule-only rows`。

### DEBT-P2-FINALIZE-2:funding-only orphan

- **現況:**舊 bridge 在 WatchRule insert 失敗且公開 `get` 也無法確認既有 row 時,仍繼續嘗試寫 funding intent。主 DB 沒有跨 repository transaction/FK,因此可能留下沒有 WatchRule 的 funding-only orphan;本切片為忠實相容而保留此缺陷。
- **目前處理:**sidecar 只為經公開 repository API 確認的 WatchRule 建立 hash 映射,不替 funding-only orphan 背書,也不把衍生索引冒充主資料。
- **觸發條件:**`Phase 3 executor 上線前,必須決定 watcher 如何對待 orphan/rule-only rows`。

### DEBT-P2-FINALIZE-3:DB 與回傳 funding timestamps 不一致

- **現況:**舊 wiring 在 consume draft 並解析 wallet 後、market reads 前擷取回傳用時間,bridge 則在網絡讀取完成後才用另一個 `now` 寫入 funding row。故**新插入**的 DB `created_at_ms`/`expires_at_ms` 可能比 tool 回傳值晚一段網絡延遲;本切片逐字保留這個 fresh-insert 差異。
- **安全例外:**deterministic `intent_id` collision 走「回既有行」時,不得沿用舊 outer DTO 以本次 request wallet 與新 deadline 拼出 hybrid 指引。MCP bin 會完整核對既有 WatchRule/funding identity;只在同一 funding wallet、仍為 `funding_required` 且既有 deadline 未過期時,回傳主 DB 既有 wallet/ATA/amount/timestamps。wallet、rule shape 或 identity 衝突皆 fail closed 且不給 funding 指引;既有 deadline 已過或 lifecycle 已前進時亦不給可簽指引。
- **風險:**簽名頁倒數依回傳 deadline 顯示,而 lifecycle/watcher 以主 DB deadline 為準;兩者在邊界附近可能對「已過期」有短暫不同判斷。
- **觸發條件:**`Phase 3 executor 上線前,必須決定 watcher 如何對待 orphan/rule-only rows`;同次評審必須明定 timestamp 的權威來源與過期邊界。
