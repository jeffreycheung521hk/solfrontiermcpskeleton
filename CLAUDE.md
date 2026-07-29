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
| `bins/solfrontier-mcp` | 新寫 | rmcp stdio server;所有 MCP Rust 新碼的落點 |
| `web/signing-page` | Phase 2 靜態頁 | 單一 HTML 入口 + 本地 vendored JS 的 build-free Phantom 入金目錄;零後端接觸、簽名時不下載外部託管程式碼,由使用者自行以只綁 `127.0.0.1` 且只公開該目錄的靜態伺服器啟動;MCP binary 不開 HTTP port、不自動開瀏覽器 |
| (未來) `crates/protocols/` | Phase 3 | Jupiter/Solend builders,從舊 gateway 抽取 |
| (未來) `crates/executor` | Phase 3 | watcher + CAS lease + controlled-wallet executor |

**禁止**重新引入:自製 LLM client / ReAct loop(舊 agent-runtime)、HTTP API surface(舊 api crate)、chat UI。這些由 MCP host 提供。

`web/signing-page` 是使用者自行啟動的 loopback-only 靜態檔案,不是 MCP HTTP API;不得把其伺服器、路由或自動開瀏覽器邏輯塞進 MCP binary。

## 目前階段:Phase 2 已完成;Phase 3 尚未開始

Phase 2 已於 2026-07-29 以 0.1 USDC 完成 Solana mainnet 全流程驗收，
並在 PR #4 合併後於 `main` 重跑完整 gate。公開交易、驗證項目與
`confirmed` commitment 的殘餘風險說明見
`docs/phase2-mainnet-acceptance.md`。

Phase 1 已完成:`get_quote`、`get_position`、`get_intent_status` 三個唯讀 tools 已分別接上 Jupiter、Solana RPC/Solend 與 state-store 真實後端,並通過 stdio MCP 驗證。

Phase 2 第一切片 `propose_intent`、第二切片 `finalize_intent` 已完成;第三切片加入本地簽名頁與明確標示會寫 DB 的 `confirm_funding`:

1. MCP host 直接提供由 schemars schema 驗證的 typed 參數;不移植舊 `stage2_llm_intent_extractor`。
2. `propose_intent` 只驗證條件、精確解析 USDC 金額、計算向後相容的 canonical draft hash,並產生隨機且非正典的 `draft_id`;此時 `No DB row exists at this point`。`draft_id` 不得進入 draft hash 或 canonical rule hash 的 preimage。
3. `finalize_intent` 重算並核對 draft hash,在 bin 自有 sidecar consume `draft_id`,讀取可信的 Solana slot/native APR 與 Save APY,再依舊系統的非事務順序建立 WatchRule 與 funding intent。此 tool 會寫資料庫,但仍然零簽名、零交易建構、零廣播。
4. Controlled wallet/ATA 延續舊系統固定值;外部出資者由 finalize 的 `user_wallet` 明確提供,不得用 controlled wallet 冒充。
5. 第三切片加入使用者自行以 loopback 靜態伺服的 Phantom 簽名頁與 `confirm_funding`。MCP binary 仍只提供 stdio、絕不持有私鑰或自動簽名;簽名頁只組 Memo → TransferChecked 並交由匹配的 Phantom 錢包明確批准。
6. `confirm_funding` 以 `confirmed` commitment 讀取交易;歷史欄位名 `funding_finalized_slot` 實際保存 `getTransaction.result.slot`,名稱不得被解讀為 `finalized` 保證。
7. 全部鏈上證據驗證通過後才依序做 `funding_required → funding_submitted → budget_reserved`。第二個 CAS 失敗留下 `funding_submitted` 時,相同 signature 的重試會重新讀取並完整驗證交易,再冪等重試第二個 CAS,不得轉成 invalid。

### Phase 2 funding 的刻意安全偏離

- **全通過才 flip:**舊實作先把 funding 改成 submitted,再驗證 Memo/金額,失敗時用 `mark_funding_invalid_if_submitted`。新 MCP 實作先完成所有鏈上驗證;拒絕或 pending 均完全不改 lifecycle,只有全部通過後才執行兩個 CAS。這避免未確認或惡意交易污染主 DB 狀態。
- **驗證真正出資者:**舊驗證器只釘 Memo 與收款 token delta,沒有證明登記的 `user_wallet` 實際出資。新實作另外要求該 wallet 是 signer、其登記 ATA 精確扣款,並核對兩側 ATA owner、mint 與 controlled ATA。沒有任何下游依賴「任何人都可代付」的舊漏洞。
- **忠實搬遷原則:**忠實搬遷適用於有相容性意義的行為語意,例如非交易式孤兒 row、timestamp 落差與過期後到帳的退款狀態;不適用於安全漏洞。上述兩項是經明確記錄及評審的安全收緊。

### 已接受風險:以 `confirmed` 而非 `finalized` 確認入金

- **風險:**沿用舊系統的 `confirmed` commitment 後,區塊理論上仍可能因叢集分叉而回滾;此時 funding 已標記為 `budget_reserved`,未來 executor 可能動用受控錢包資金執行一筆實際未收到款的意向。
- **接受理由:**這是舊系統已在 Solana 主網實測驗證過的行為;`confirmed` 回滾需要主網叢集分叉,實務上極罕見。Phase 2 目前仍屬測試規模,因此暫時接受此風險容忍度。
- **與安全偏離的區別:**「全通過才 flip」與「驗證真正出資者」修補的是攻擊者可主動利用的漏洞;commitment 等級則是風險容忍度參數。一般第三方或 MCP 呼叫者無法單方面觸發 Solana 叢集分叉來製造 `confirmed` 回滾。
- **重新評估觸發條件:**單筆金額超出測試規模,或系統開始管理任何非測試資金時(兩者取最早),必須重新評估並明確決定是否改為等待 `finalized` 後才 flip 到 `budget_reserved`;評審完成前不得默認沿用測試期容忍度。

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
- **測試缺口:**repository 是具體型別,本切片沒有可注入 funding insert/readback 失敗的 seam,所以實際 rule-only 故障路徑無測試覆蓋;此處只記錄缺口,不為測試而改動核心 crate 邊界。
- **觸發條件:**`Phase 3 executor 上線前,必須決定 watcher 如何對待 orphan/rule-only rows`。
- **補測觸發條件:**一旦 bin 取得可注入的 funding repository seam、state-store 新增正式 transaction/recovery API,或最遲進入 Phase 3 executor 合併評審(三者取最早),必須加入 funding insert/readback 故障測試,斷言 rule-only row、draft tombstone 與恢復行為。

### DEBT-P2-FINALIZE-2:funding-only orphan

- **現況:**舊 bridge 在 WatchRule insert 失敗且公開 `get` 也無法確認既有 row 時,仍繼續嘗試寫 funding intent。主 DB 沒有跨 repository transaction/FK,因此可能留下沒有 WatchRule 的 funding-only orphan;本切片為忠實相容而保留此缺陷。
- **目前處理:**sidecar 只為經公開 repository API 確認的 WatchRule 建立 hash 映射,不替 funding-only orphan 背書,也不把衍生索引冒充主資料。若 funding row 已寫入但 `rule_persisted == false`,新 MCP 回應層 fail closed:回 `status:"rule_unconfirmed"`、`funding_actionable:false`,並完全省略 `funding`/`signing`,避免用戶把資金送進註定無規則可匹配的流程。
- **測試缺口:**純回應 guard 已有單元測試;但 WatchRule repository 是具體型別,無法注入 insert 加 readback 皆失敗,故實際 funding-only orphan 路徑仍無測試覆蓋。此處只記錄缺口,不改變保留的非交易寫入順序。
- **觸發條件:**`Phase 3 executor 上線前,必須決定 watcher 如何對待 orphan/rule-only rows`。
- **補測觸發條件:**一旦 bin 取得可注入的 WatchRule repository seam、state-store 新增正式 transaction/recovery API,或最遲進入 Phase 3 executor 合併評審(三者取最早),必須加入 WatchRule insert/readback 雙失敗測試,同時斷言 orphan row 被保留而回應仍為 `rule_unconfirmed` 且不含入金指引。

### DEBT-P2-FINALIZE-3:DB 與回傳 funding timestamps 不一致

- **現況:**舊 wiring 在 consume draft 並解析 wallet 後、market reads 前擷取回傳用時間,bridge 則在網絡讀取完成後才用另一個 `now` 寫入 funding row。故**新插入**的 DB `created_at_ms`/`expires_at_ms` 可能比 tool 回傳值晚一段網絡延遲;本切片逐字保留這個 fresh-insert 差異。
- **安全例外:**deterministic `intent_id` collision 走「回既有行」時,不得沿用舊 outer DTO 以本次 request wallet 與新 deadline 拼出 hybrid 指引。MCP bin 會完整核對既有 WatchRule/funding identity;只在同一 funding wallet、仍為 `funding_required` 且既有 deadline 未過期時,回傳主 DB 既有 wallet/ATA/amount/timestamps。wallet、rule shape 或 identity 衝突皆 fail closed 且不給 funding 指引;既有 deadline 已過或 lifecycle 已前進時亦不給可簽指引。
- **風險:**簽名頁倒數依回傳 deadline 顯示,而 lifecycle/watcher 以主 DB deadline 為準;兩者在邊界附近可能對「已過期」有短暫不同判斷。
- **觸發條件:**Phase 3 executor 合併評審前,必須明定 DB timestamps 與 tool 回傳 timestamps 的唯一權威來源及過期邊界,並讓 signing page、watcher/lifecycle 使用同一套判定;未完成不得關閉本債。

### DEBT-P2-FINALIZE-4（已關閉）:故障回應測試缺口

- **關閉方式:**Phase 2 funding(②-3)開工時已補上兩條 finalize-level 離線契約測試。損毀 SQLite fixture 釘死 `sidecar_unavailable`、主 DB 零寫入且無 `funding`/`signing`;`MockMarketSource` 故障 fixture 釘死固定 error class、`draft_consumed:true`、同 draft 不可重用、主 DB 零寫入且無 `funding`/`signing`。
- **回歸門檻:**後續修改 `IntentSidecar`/claim-to-response 映射、consume-before-market 順序、`FinalizeMarketDataSource` 或錯誤分類/回應欄位時,上述測試必須繼續通過;不可移除或弱化 fail-closed 斷言。

### DEBT-P2-FUNDING-1:過期入金無自動退款 handler

- **現況:**funding row 的硬期限為 `expires_at_ms`(目前窗口 180,000ms);WatchRule 另有 `created_at_slot + 480` 的 lease 期限。沿用舊語意,鏈上證據全部正確但在 funding deadline 後才到帳的入金仍會被接受並記錄,但已不能當作正常可執行資金。
- **目前處理:**`confirm_funding` 以交易 `blockTime`(缺失時才用明確標示的確認時鐘 fallback)對 funding row `expires_at_ms` 判定 late,並回報「此入金於過期後到達,已記錄為可退款;退款目前需人工處理」。回應另行揭露 WatchRule slot deadline;它只決定 executor 能否取得 lease,不取代 funding deadline。
- **風險:**目前沒有自動退款 handler 或已文件化的完整人工退款程序,資金可能停留在 controlled ATA。回應與 README 不得把 late funding 呈現為一般成功或暗示退款已完成。
- **觸發條件:**Phase 3 executor 上線前,必須決定並評審退款路徑(自動退款或明確、可操作且可稽核的人工程序);未完成不得讓 executor 消費此類 late funding,也不得關閉本債。
