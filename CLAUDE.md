# CLAUDE.md — solfrontier-mcp

## 這個 repo 是什麼

SolFrontier(前身 ClawSolana / Solfrontier2026)的 MCP 重構版:一個 policy-gated、fail-closed 的 Solana bounded-intent 控制面,以 **MCP stdio server** 形式暴露工具,由外部 MCP host(Claude Desktop / Claude Code)取代舊有的自製 agent 層。

**開工前必讀:`docs/重構建議書.md`** — 完整的現況診斷、目標架構、四階段遷移路線。本檔只是摘要。

舊程式碼參考(不在本 repo,需要時叫用戶提供路徑或 clone):
- `github.com/jeffreycheung521hk/Solfrontier2026`(deadline 後修正版,以此為準)
- `github.com/jeffreycheung521hk/testingcrypto2`(Hackathon 提交版,含 ARCHITECTURE.md / DEBT.md 全文)

## 系統不變量(絕對,任何 PR 不得違反)

繼承原 ARCHITECTURE.md 的 INV-1 ~ INV-8,重點:

- **INV-1** Human signs, AI proposes — 使用者／主錢包一律由人簽;任何 MCP tool 都不得持有使用者私鑰或觸發自動簽名。PR #17 唯一允許的窄例外是下文完整記錄的非 MCP `watch --execute` controlled-wallet rail:必須由 operator 明確啟動、只讀取釘死的受控執行錢包 keypair,且仍受 canonical intent、funding、雙時鐘、simulation/policy 與 CAS gate 約束。此例外不得外推成任意 action、任意錢包或 MCP/LLM 可觸發的簽名權。
- **INV-2** Typestate 管線(`Proposal → Simulated → Approved → Signed`)不可繞過、不可改造 — `crates/wallet-engine` 是 "do not touch" 區。
- **INV-3** Audit trail 只加不改(state-store AuditRepository 無 update/delete)。
- **INV-5** 簽名前必先 simulation,無跳過旗標。
- **INV-6** Fail-closed:policy 不匹配 → 需人手;檢查失敗 → block。
- **INV-7** local-only:stdio transport,不開 HTTP port。加遠端傳輸需先補 auth 並另行評審。
- **INV-8** 核心 crates `#![forbid(unsafe_code)]`。

## Crate 地圖

| 位置 | 狀態 | 說明 |
|---|---|---|
| `crates/observability` `crates/solana-core` `crates/wallet-engine` `crates/risk-engine` | **原封搬入,不動邊界** | 四個未修訂的 Solfrontier2026 mainnet-proof 信任錨 |
| `crates/types` `crates/state-store` | **公開修訂後重新凍結** | [PR #9](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/pull/9):types append-only 加 schema v2 並以 v1 golden hashes 證明舊指紋不變;state-store 只加無約束 TEXT action parser/round-trip,無 DB migration |
| `bins/solfrontier-mcp` | MCP 入口 | rmcp stdio server;負責 tools、transport、runtime/config 與後端協調 glue,純 protocol logic 下沉 `crates/protocols` |
| `web/signing-page` | Phase 2 靜態頁 | 單一 HTML 入口 + 本地 vendored JS 的 build-free Phantom 入金目錄;零後端接觸、簽名時不下載外部託管程式碼,由使用者自行以只綁 `127.0.0.1` 且只公開該目錄的靜態伺服器啟動;MCP binary 不開 HTTP port、不自動開瀏覽器 |
| `crates/protocols/` | Phase 3 進行中 | 純 Jupiter/Solend wire types、decoders 與 unsigned builders;不得依賴 approval/pending state、DB、network、signing 或 submission |
| `crates/executor` | Phase 3b-1 已交付;Phase 3b-2 execution 由 PR #17 引入、真鏈驗收另記 | SDK/DB/RPC 無關的 fail-closed candidate/condition 驗證核心;write-capable lease、simulation、controlled-wallet signing、broadcast 與 finality adapter 只允許留在明確的 bin/runtime 邊界 |

**禁止**重新引入:自製 LLM client / ReAct loop(舊 agent-runtime)、HTTP API surface(舊 api crate)、chat UI。這些由 MCP host 提供。

`web/signing-page` 是使用者自行啟動的 loopback-only 靜態檔案,不是 MCP HTTP API;不得把其伺服器、路由或自動開瀏覽器邏輯塞進 MCP binary。

## 目前階段:Phase 2 與 Phase 3b-1 已完成;Phase 3b-2 execution 由 PR #17 引入、主網驗收待獨立記錄

Phase 2 已於 2026-07-29 以 0.1 USDC 完成 Solana mainnet 全流程驗收，
並在 PR #4 合併後於 `main` 重跑完整 gate。公開交易、驗證項目與
`confirmed` commitment 的殘餘風險說明見
`docs/phase2-mainnet-acceptance.md`。

Phase 3 的 protocols 與 reserve decoder 搬遷、schema v2 公開修訂及
canonical `SolendDeposit` finalize 寫入均已完成。`crates/executor` 與
`solfrontier-mcp watch` 預設仍是 read-only dry-run:只掃描、重算指紋、
驗證雙時鐘/三方金額/鏈上帳戶與條件,再輸出帶零 blockhash 及預設
signature slots 的不可提交 unsigned transaction;未帶 `--execute` 時
不讀 keypair、不 lease、不簽名、不廣播且主 DB 零寫入。

PR #17 引入 Phase 3b-2 的非 MCP `watch --execute` rail。operator 明確
啟動後,每輪必須先完成全批 scan/precheck;若無 scan failed/truncated,
再對每筆通過者依序做 `budget_reserved → executing` CAS、fresh rebuild、
stderr 完整交易揭露、wallet-engine simulation、risk-engine policy、
`Approved` typestate、controlled-wallet signing、broadcast 與只以
`finalized` 為成功的確認。execute preflight 報告必須明示同行程已存在
write-capable repository connection,不得只標成 read-only。
孤兒逐列分類後跳過,不會令完整循環崩潰;多筆 ready 可依序處理。
這是 mainnet 測試資金切片,不是 production-ready、restart-safe 或完整
reconciliation 的宣告;PR 合併與獨立 mainnet 驗收前亦不得寫成已交付。

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

### Phase 3b-2 controlled-wallet execution 的刻意偏離與已知邊界（PR #17）

- **偏離內容:**`watch --execute` 不呼叫 `clawsol-authority ExecuteAction`,不讀 `AuthorizationRecord` PDA,也不把 on-chain authority 放進 Solend CPI loop。通過所有 gate 後,由釘死的 controlled-wallet keypair 直接簽署並呼叫 Solend;這必須在文件、audit 與驗收證據中明說,不得包裝成已部署的 user-delegated PDA execution。
- **理由:**目前部署的 authority action byte 只支援既有 1/2 路徑,Solend CPI builder 仍為 withdraw-only,並不支援 canonical `SolendDeposit=3`。為了驗收已由 Phase 2 funding 綁定、金額受限且可重算的 deposit intent,PR #17 採用前身已有 mainnet 證據的 controlled-wallet direct rail,而不是假造一條不存在的 on-chain grant。
- **相對前身 W5i 的刻意安全偏離:**前身 `stage2_chat_execute.rs:53-59` 明載 deposit 直接使用受控錢包簽名、沒有經過 review pipeline。PR #17 不複製該 bypass:它把同一份 fresh unsigned transaction 依序送入 wallet-engine simulation、真實 risk-engine policy、明確為 auto-approved 的 `Approved` typestate,再由包裝 signer 捕捉簽名結果;approved message、signed message 與提交 bytes 內的 message 必須逐位元相同且簽名需通過 `Transaction::verify()`。忠實搬遷不適用於前身違反 INV-2/INV-5 的安全缺陷。
- **人類可審交易揭露:**CAS 成功後、wallet review 前先以 dry-run 同 schema 輸出完整 placeholder-blockhash shape;simulation 綁定 fresh blockhash 後、policy/signing 前再輸出 exact unsigned message。後者除 blockhash 外必須與 canonical builder message 逐位元相同。任一「簽名前交易 audit」序列化失敗都不得進 signer;安全 release 該 lease 後仍須中止當輪其餘 candidates。若較後面的 execution-attempt report 序列化失敗,同樣中止其餘 candidates並輸出固定錯誤類別,但不得聲稱能回滾已發生的 broadcast 或狀態寫入。
- **前身實作／意圖矛盾修正（非 DEBT）:**前身 `stage2_chat_execute.rs:1952-1955` 在讀取 `confirmationStatus` 前就以 `err != null` 回傳 terminal `Failed`,但同檔約 `:1054` 的狀態機註解明確宣告該路徑是「tx finalized but failed on-chain」並以 `mark_failed` 結案。Phase 3b-2 依宣告語意收緊:processed/confirmed 即使觀測到 `err` 仍為 pending,不 release、不重播且保持 `executing`;只有 `confirmationStatus == finalized` 且 `err != null` 才 terminal-fail。這與 completed 只接受 finalized 的原則一致,並避免非最終分叉上的暫時失敗污染正式鏈結果。
- **人類授權邊界:**使用者只以 Phantom 人手簽 funding;主錢包私鑰永不進 daemon。執行端的授權是 operator 明確啟動非 MCP CLI、固定 controlled wallet、精確 canonical identity/三方金額、fresh account/condition/雙時鐘檢查、simulation/policy 與單一 CAS 的交集。MCP host/LLM 不能把一般 tool call 升格成 `--execute`。
- **限縮與理由終點:**此偏離只覆蓋固定 mainnet Solend USDC deposit 與測試資金,不代表 generic autonomous custody。若要走 Authorization PDA、擴大 action/mint/wallet、管理非測試資金或宣告 production-ready,必須先完成對應鏈上支援／審計及下列 pagination、退款與 crash-recovery 債務;不得以 PR #17 的驗收外推。

## 工程紀律

- 體積是一級關注:release profile 已設 `opt-level="z"` / `lto="fat"` / `panic="abort"`。新增依賴前先問「會拖進多大的樹」;定期 `cargo bloat --release --crates`。
- 依賴收窄的 TODO 標記在根 `Cargo.toml`(tokio features、pubsub-client、token-2022)— 動它們時先 grep 用途。
- stdio server 的 stdout 專屬 JSON-RPC:**所有日誌走 stderr**(已在 main.rs 設定,勿改);`watch` 子命令的人類可審 JSON 報告亦只寫 stderr,不得污染 stdout。
- 格式化只對受影響的**非 legacy-fmt** package 執行 `cargo fmt -p <package>`,永不使用 `cargo fmt --all`;CI 以「workspace 全部成員減去六個 legacy-fmt package」動態導出 fmt 檢查範圍,新 crate 會自動納入。一般 PR 每個 commit 前必須跑 `git diff --exit-code -- crates/types crates/observability crates/state-store crates/solana-core crates/wallet-engine crates/risk-engine`,確認六個核心 crate 零 diff。[PR #9](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/pull/9) 是一次性公開例外,只允許列明的 types/state-store schema diff;該 PR 每個 commit 仍須證明其餘四個核心 crate 零 diff,且 `bins/solfrontier-mcp/src/finalize.rs` 除顯式釘住 v1 的相容性護欄外不得改變寫入行為。PR-B 恢復 guard 後六者重新凍結。
- GitHub Actions `gate` 是合併權威(PR #6):`Windows validate` 負責 fmt/check/test。基於本機磁碟、MSVC linker 與 OpenSSL 環境不穩定的實測,本機不再負責全量 gate;只保留受影響 package 的 fmt 與 `cargo check` 級輕量預檢,不得以本機未跑全量測試為由繞過或取代 CI。
- **磁碟量測護欄:**沙箱內的磁碟容量讀數不具決策效力;若 `Get-PSDrive` 同時回報 `Used=0` 與 `Free=0`,一律視為儀器故障而非磁碟事件。任何磁碟清理、停止編譯或搬移資料的決策前,必須在沙箱外以至少兩種獨立方法交叉驗證(優先使用 .NET `DriveInfo` 與 `Win32_LogicalDisk`);不得再因單一沙箱讀數觸發任何清理。41 個舊 SolFrontier/sf2026 工作樹是審計引用底稿,永久列為不可刪。
- **PR 分級:**低風險 PR 在一輪 `Windows validate` 全綠後即可合併。高風險 PR(碰資金/簽名/controlled wallet、六個凍結 crate、schema 或 executor)必須一輪 `Windows validate` 全綠,再經明確覆核後才可合併;不得把 CI 綠燈當成安全評審的替代品。
- **Release 與體積權威:**`Windows release artifact` 只在 push 到 `main` 時自動執行;若某 PR 明確宣稱影響體積(例如 Phase 4),可在該分支以 `workflow_dispatch` 手動先量,但分支量測只供預估。`docs/size-baseline.txt` 只在階段邊界追加,每筆必須註明 GitHub Actions run id;正式體積一律採合併後 `main` 的 run,不得混入本機產物、推測值或分支手動 run。
- **CI 等待不空轉:**等待 CI 時,從當前分支頭疊出下一個有明確依賴的分支繼續工作(父 PR 合併後再 rebase/retarget),或先完成不污染當前 PR 範圍的文件工作;文件優先順序先做 `TRUST.md`。
- 大型搬移遵守舊 DEBT.md 的 PC-1/PC-2/PC-3(路徑審計;一 PR 一主題、不混邏輯改動;由低風險到高風險)。
- 測試命名用語意名,不要沿用舊 repo 的 prompt-session 前綴(p1_/n6_/w5h_)。

## 技術債

### DEBT-P3-SCHEMA-1:鏈上 Authorization PDA 尚不支援 SolendDeposit

- **現況:**`ActionSpec` 的 Borsh tag 是 `withdraw=0 / Jupiter=1 / deposit=2`;這與 `WatchRuleActionType::to_u8()` 的鏈下 routing/audit 值 `1 / 2 / 3` 是兩個命名空間。已部署的 `clawsol-authority` 只接受 action byte 1/2,未知值 fail closed,Solend CPI builder 亦仍是 withdraw-only。因此 `SolendDeposit=3` 目前絕不可宣稱為已部署 PDA grant。
- **目前邊界:**PR #17 的 Phase 3b-2 controlled-wallet direct rail 刻意不呼叫 `clawsol-authority` 或 Authorization PDA;它只能依上節的非 MCP、固定錢包、測試資金邊界運作。PR #9 因此只建立鏈下 canonical identity;PR #17 亦不得把 direct signature 宣稱為 PDA grant。舊 W5i 雖 import `stage2_executor` 的五個示範常數,但沒有走 `validate_request_shape` 或該授權執行路徑。
- **觸發條件:**若日後任何 Solend deposit 改走 Authorization PDA / `ExecuteAction` 授權路徑,必須先更新、重新部署並獨立審計鏈上程式與 Solend CPI builder,再升 watch-rule schema version;完成前該路徑必須 fail closed。

### DEBT-P3-SCHEMA-2:SolendDeposit decimals 是執行時推導值

- **現況:**`input_mint` 進 canonical 指紋,但 decimals 不進。`input_mint` 不在 Solend deposit 的 14 個 account meta 中,其證據等級是間接的:runtime 必須要求 `action.input_mint == decoded reserve liquidity mint == source ATA mint`,核對 ATA owner 與 pre/post token balance,並在目前 USDC-only rail fail closed 要求 `decimals == 6`。
- **理由:**固定 USDC rail 的 decimals 可由 mint/reserve 真實狀態重算,避免在 action 重複一個可驗證的衍生值;這不放寬 ATA、mint、owner 或 amount 的三方等值檢查。
- **觸發條件:**允許非 USDC mint、可配置 decimals 或 Token-2022 前,必須重新評審 decimals 與 token program 是否要進 canonical 指紋,並按結論升 schema version;完成前不得擴大 mint 白名單。

### DEBT-MCP-1（已關閉）:canonical hash 公開反查缺口

- **關閉方式:**Phase 2 finalize 在 MCP bin 維護與主 DB 分離的衍生 sidecar,記錄 `canonical_rule_hash → intent_id`;沒有修改六個核心 crate、repository API 或主資料庫 schema,也沒有從 bin 對主 DB 寫直接 SQL。
- **唯一真相源:**主 DB 的 WatchRule 與 funding intent 是唯一真相。sidecar 只負責解析識別碼;每次命中後仍須透過公開 repository API 讀回 WatchRule、重算並核對 canonical rule hash,若 funding intent 存在也必須核對其 hash/ID,不得直接信任 sidecar。
- **優雅降級:**sidecar 遺失、損毀、schema 不相容、無映射或映射與主 DB 不一致時,hash 反查回正常 JSON `status:"unsupported_ref"`,不崩潰、不誤答;`intent_id`/UUID 查詢仍以主 DB 為準。

### DEBT-P2-FINALIZE-1（可執行掃描容錯已交付;完整覆蓋/故障注入展期）:rule-only row

- **現況:**為逐字保留舊 bridge 的非事務語意,finalize 先寫 WatchRule,再寫 sidecar 與 funding intent。若程序在第一個主 DB 寫入後中斷,或後續 funding insert 失敗,會留下沒有 funding row 的 rule-only row;沒有 rollback 或 startup reconciler。
- **watcher 容錯:**Phase 3b-1 對公開 API 回傳的非終態 WatchRule,找不到 funding row 時明確分類 `orphan_rule_only`,逐列跳過且不做鏈上 account read。dry-run 在物理上沒有 lease、簽名、廣播或主 DB 寫入能力;同一批次另有後續列仍會繼續處理。此證據只涵蓋 oldest-first 的首 128 列,不得宣稱 cursor-complete。
- **仍保留的測試缺口:**離線測試可用公開 API 種出既存 rule-only row 並驗證 watcher 容錯,但 finalize repositories 仍是具體型別,無法注入「rule insert 成功、funding insert/readback 失敗」的原始中斷點,所以 draft tombstone/collision recovery 的 fault-injection 證據仍缺。不得為測試繞過 repository 邊界或改寫非事務順序。
- **展期觸發條件（PR #17 重議）:**Phase 3b-2 測試資金 rail 以「任一 `*_scan_failed` / `*_scan_truncated` 令全輪 abort、該輪 lease/sign/broadcast 均為零」防止 bounded scan 被誤當完整世界;cursor-complete/reconciliation 義務改由 `DEBT-P2-FUNDING-1` 在非測試資金／production-ready 邊界統一追蹤。故障注入仍在 bin 取得可注入 funding repository seam、state-store 新增正式 transaction/recovery API,或 production-ready(三者取最早)時補齊,並斷言 rule-only row、draft tombstone 與恢復行為。

### DEBT-P2-FINALIZE-2（budget_reserved 容錯已交付;原始 orphan 覆蓋/故障注入展期）:funding-only orphan

- **現況:**舊 bridge 在 WatchRule insert 失敗且公開 `get` 也無法確認既有 row 時,仍繼續嘗試寫 funding intent。主 DB 沒有跨 repository transaction/FK,因此可能留下沒有 WatchRule 的 funding-only orphan;本切片為忠實相容而保留此缺陷。
- **watcher 容錯:**sidecar 仍不替 orphan 背書;Phase 3b-1 對列舉到的 `budget_reserved` funding 以公開 `get` 讀回 WatchRule,缺失時明確分類 `orphan_funding_only`,逐列跳過且循環不中斷。離線真 SQLite 測試以公開 API 種出此狀態,並釘死無 account read、無 unsigned transaction 及主 DB 零寫入。
- **完整覆蓋缺口:**finalize 的原始缺陷實際會留下 `funding_required` orphan;目前凍結 repository 沒有該狀態的 bounded scan,所以 Phase 3b-1 看不到這種原始 orphan。`orphan_funding_only` 名稱只代表被 `list_budget_reserved` 列舉到的子集合,不得宣稱本債已全部清償。
- **仍保留的測試缺口:**finalize 的純回應 guard 已有測試,但 WatchRule repository 是具體型別,仍無法注入「insert 與 readback 同時失敗」來重演 orphan 的原始建立路徑。既存 orphan 的 watcher 行為已有測試;建立當下的 fault-injection 證據尚未具備。
- **展期觸發條件（PR #17 重議）:**測試資金的 `watch --execute` 遇任一 `*_scan_failed` / `*_scan_truncated` 必須在全輪第一個 lease 前 abort,所以不以 write-capable flag 本身冒充完整 orphan coverage。pagination/逐列隔離改由 `DEBT-P2-FUNDING-1` 在非測試資金／production-ready 邊界統一觸發;WatchRule insert/readback 雙失敗測試仍在 bin 取得可注入 WatchRule repository seam、state-store 新增正式 transaction/recovery API,或 production-ready(三者取最早)時交付,並斷言 orphan row 被保留、finalize 仍回 `rule_unconfirmed` 且不含入金指引。

### DEBT-P2-FINALIZE-3（已清償）:persisted funding timestamps 是唯一 wall-clock 權威

- **清償方式:**[PR #8](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/pull/8) 在 fresh insert 後經 `Stage2W5hFundingIntentRepository::get` 讀回主 DB row;tool 頂層、nested `funding` 與 signing-page fragment 的 `created_at_ms`/`expires_at_ms` 全部逐值使用該 persisted row,不再由 bin 另造 deadline。
- **值等價保證:**保留舊有 pre-market clock sample 及 persisted deadline 的生成順序,只改 authority、不改 DB deadline 值。fresh insert 與 deterministic collision 測試均斷言回應、funding payload、URL fragment 精確等於 repository 讀回值。
- **過期邊界:**在輸出可簽 handoff 前重新讀 clock;`now_ms >= funding.expires_at_ms` 即不可 action,回應不得含 `funding`、`signing` 或 `signing_page_url`。WatchRule 的 `expires_at_slot` 仍是獨立的 slot-clock 權威,由 Phase 3 executor 與 persisted wall-clock deadline 共同 fail closed。
- **回歸門檻:**任何修改 finalize persistence、collision recovery、signing URL 或 expiry 判定的 PR,都必須保留 repository readback equality 與 exact-endpoint 測試;不得重新引入 response-only timestamp。

### DEBT-P2-FINALIZE-4（已關閉）:故障回應測試缺口

- **關閉方式:**Phase 2 funding(②-3)開工時已補上兩條 finalize-level 離線契約測試。損毀 SQLite fixture 釘死 `sidecar_unavailable`、主 DB 零寫入且無 `funding`/`signing`;`MockMarketSource` 故障 fixture 釘死固定 error class、`draft_consumed:true`、同 draft 不可重用、主 DB 零寫入且無 `funding`/`signing`。
- **回歸門檻:**後續修改 `IntentSidecar`/claim-to-response 映射、consume-before-market 順序、`FinalizeMarketDataSource` 或錯誤分類/回應欄位時,上述測試必須繼續通過;不可移除或弱化 fail-closed 斷言。

### DEBT-P3-FINALIZE-1:SolendDeposit 目前只允許全額單次執行

- **現況:**PR-C 將同一個精確金額投影到 canonical `ActionSpec::SolendDeposit.input_amount_raw`、`WatchRule.max_input_amount_raw` 與 persisted funding `amount_raw`,並以 finalize 端測試釘死三方精確等值。規則仍為 `one_shot:true`、`used_amount_raw:0`;目前沒有 partial-fill 或分批執行語意。
- **安全邊界:**任何三方不等、非 `SolendDeposit` action,或嘗試以 `used_amount_raw`／executor request 覆蓋該金額都必須 fail closed;不得簽名或取得 execution lease。
- **觸發條件:**任何引入部分執行(partial fill)、分批執行、剩餘額重試,或允許 `0 < used_amount_raw < max_input_amount_raw` 的變更,必須先重新評審並文件化 canonical action、rule cap、funding amount、實際執行額與退款／會計的關係,按結論升 watch-rule schema version並補齊跨 finalize→funding→executor 的不變量測試;完成前不得放寬三方精確等值。

### DEBT-P3-WATCH-1（PR #17 重議）:execute scan flag 全輪 abort;擴容債併入 funding

- **仍存在的底層限制:**凍結 repository 的 `list_budget_reserved` 與 `list_pending_lifecycle_limit` 只接受 limit,沒有 cursor/keyset。每類先取 129、明確回報 `*_scan_truncated`,再只處理 oldest-first 128 列;任一損毀列亦會因整批 `collect()` 讓該 table cycle 回 `*_scan_failed`。這些事實沒有被 PR #17 修復。
- **PR #17 的 write-capable 邊界:**dry-run 可逐列回報既有 finding,但 `watch --execute` 必須先完成整輪 scan/precheck。只要出現任何 `*_scan_truncated` 或 `*_scan_failed`,整輪在第一個 lease/sign/broadcast 前 abort;不得執行「旗標前已掃到的 ready row」。孤兒分類不是 scan flag,必須逐列跳過並繼續;多筆不同的 ready row 可依序處理。
- **重議結論:**全輪 abort 足以讓明確隔離、低筆數、測試資金的 Phase 3b-2 mainnet 驗收 fail closed,但不證明 eventual coverage、損毀列隔離或完整 orphan reconciliation。cursor/keyset pagination 與 row-level decode/error result 不再以「第一個 write-capable flag」為觸發點,改併入 `DEBT-P2-FUNDING-1`,並在管理任何非測試資金或宣告 production-ready 前(兩者取最早)交付與獨立評審。

### DEBT-P3-EXECUTION-1（PR #17 新增）:`executing` 沒有 crash recovery / reaper

- **目前安全狀態:**execution CAS 必須先把 `budget_reserved → executing` 再簽名。可證明未 broadcast 的 build/simulation/policy failure 才能 release;只有 finalized observation 的明確 `err` 才能 terminal-fail;已取得 signature 但尚未 `finalized`、poll timeout、或 daemon 在 broadcast/完成寫入窗口崩潰時,必須保留 `executing` 作為 ambiguity barrier。任何自動重簽、重送新 message、退回 `budget_reserved` 或退款都可能造成雙重消費。
- **未來 CAS 注意:**目前公開 funding repository 的 `mark_failed` 並非 `executing`-only CAS。本切片只會在同一 executor 已取得 lease 且觀測到 finalized error 後呼叫它,所以既有測試資金流程沒有合法競爭寫者;但任何未來 reaper、退款或人工 recovery 一旦可與 finality poll 並行,都必須把 terminal failure 改成綁定 `executing`（最好連 signature）的 CAS,並在上述 recovery 觸發點一併測試競爭更新。
- **尚缺能力:**PR #17 不交付 lease TTL、startup reaper、durable signature-first journal 或跨 WatchRule/funding 兩步完成寫入的 crash repair。`executing` row 不再被一般 ready scan 自動執行;mainnet 測試若進入此狀態只能先以已知 signature 做人工鏈上 reconciliation,不得把第二次 `watch --execute` 當 recovery。
- **保留的兩步完成缺陷:**finalized success 仍依前身順序先 `WatchRule → completed`,再把 funding row 從 `executing → completed`;兩者不是單一 transaction。第二步零更新或故障時會留下 `rule=completed / funding=executing`,並在回應中明示需人工 reconciliation。這延續 `DEBT-P2-FINALIZE-1/2` 已記錄的跨 repository 非交易式邊界,不得被綠燈成功路徑掩蓋;其修復併入本債的 durable recovery 觸發點。
- **觸發條件:**首次出現停在 executing 需人工對鏈的實例時，或任何連續值守運行（非驗收場景）開始之前。屆時必須另行交付並評審 durable recovery:枚舉所有 `executing`、保留/恢復 canonical signature、以鏈上 finality 分流 completed/failed/仍 ambiguous、冪等修復兩份完成狀態,且在證明舊交易不可能落鏈前禁止新簽名與退款。測試須覆蓋 lease 後每一個 crash window及兩步寫入中斷。

### DEBT-P3-EXECUTION-2（PR #17 新增）:final revalidation 與 funding CAS 不是單一快照

- **精確現況:**execute process 同時持有 read-only/query-only preflight pool 與 write-capable repository pool。公開 `lease_execution_if_budget_reserved` 的原子 WHERE 只涵蓋 `intent_id`、`status = budget_reserved`、`expires_at_ms > lease_now_ms`;它會排除 execution/refund/status 競爭並在含端點語意下重驗 persisted funding deadline,但不涵蓋 funding hash/amount/wallet 欄位、WatchRule lifecycle/slot deadline或即時 reserve/account/condition facts。
- **現有緩解與證據邊界:**funding identity/amount/wallet topology 在公開 repository 中沒有 insert 後更新 API;target-specific revalidation 在 CAS 前重新讀 exact row/rule,再取後置 confirmed slot 把守最終 slot/reserve-staleness gate。CAS 後只從保留的 validated canonical plan 組交易,完整輸出後再經 wallet-engine shape/simulation/policy/bytes-identity gate。這能阻止參數替換,但兩個 pool 本身不會令多次讀取與 CAS 成為同一 SQLite transaction,鏈上 facts 亦不可能被該 CAS 鎖住。
- **具體競爭例:**WatchRule 可在 final revalidation 後、funding CAS 前被 revoke;funding CAS 不含 WatchRule lifecycle predicate,而凍結 repository 的 `mark_completed` 亦沒有 `revoked = 0` WHERE。若交易其後 finalized,目前可形成 `status=completed` 與 `revoked=1` 並存的矛盾 row。測試資金邊界接受此缺口,但不得把 target-specific reread 描述為原子 snapshot。
- **觸發條件:**首次管理任何非測試資金或宣告 production-ready 前(兩者取最早),必須另行評審並交付正式 snapshot/lease 契約:至少讓可變的 DB eligibility 欄位與 lease 在同一 transaction/CAS 中綁定,明確定義鏈上 condition 的最大 TOCTOU 容忍窗口,並以競爭更新及條件翻轉測試證明 fail-closed。完成前此 rail 只准用測試資金。

### DEBT-P3-KEY-1（PR #17 新增）:frozen `SecretKeystore` 內部 key material 零化尚未可證

- **目前護欄:**bin 只從 `SOLFRONTIER_CONTROLLED_WALLET_KEYPAIR` 載入釘死的 controlled-wallet keypair,且在載入結果成功或失敗後都明確以 `fill(0)` 清除 caller-owned JSON file buffer 與 bin 自行解析的 raw-key `Vec<u8>`。固定錯誤類別與日誌都不得包含 path、key bytes 或 parser/RPC 原文。
- **證據邊界:**凍結的 `crates/wallet-engine::SecretKeystore` 會把 key material 轉入其內部 `SecretKeypair`/共享儲存,但目前公開 API 與實作沒有提供可由 bin 驗證的 `Zeroize`/`Drop` 保證。caller buffers 已清除不等於 keystore 內部副本已可證零化;本 PR 不為此改動凍結 crate。
- **觸發條件:**首次管理任何非測試資金或宣告 production-ready 前(兩者取最早),必須獨立覆核並補強 frozen keystore 的 key lifetime/zeroization,以可測試的 `zeroize`/`ZeroizeOnDrop` 或經安全評審的等價機制證明所有內部副本在生命週期結束時清除;完成前不得把 caller-buffer 清零宣稱為完整記憶體零化。

### DEBT-P2-FUNDING-1（PR #17 合併掃描債）:過期入金退款與可稽核全量掃描尚未交付

- **現況:**funding row 的硬期限為 `expires_at_ms`(目前窗口 180,000ms);WatchRule 另有 `created_at_slot + 480` 的 lease 期限。沿用舊語意,鏈上證據全部正確但在 funding deadline 後才到帳的入金仍會被接受並記錄,但已不能當作正常可執行資金。
- **目前處理:**`confirm_funding` 以交易 `blockTime`(缺失時才用明確標示的確認時鐘 fallback)對 funding row `expires_at_ms` 判定 late,並回報「此入金於過期後到達,已記錄為可退款;退款目前需人工處理」。回應另行揭露 WatchRule slot deadline;它只決定 executor 能否取得 lease,不取代 funding deadline。
- **Phase 3b 邊界:**唯讀 watcher 對列舉到且 wall-clock 已到端點的 `budget_reserved` row 回 `wall_clock_expired`;它不呼叫 refund lease/API。PR #17 的 execute mode 同樣不得消費 late funding,且任一 `*_scan_failed` / `*_scan_truncated` 都必須全輪 abort。已在其他 expired/refund 狀態的 row 不在目前列舉範圍,所以 dry-run 與測試資金 execution 都不關閉本債。
- **由 DEBT-P3-WATCH-1 合併的掃描債:**兩個 bounded public list API 仍沒有 cursor/keyset,亦沒有逐列 decode/error result;目前的全輪 abort 只防止在不完整視野下簽名,不能提供 eventual coverage、完整 orphan enumeration 或可稽核退款 sweep。未來退款/reconciliation 必須與 pagination、row isolation 一起設計,避免壞列或 oldest-first 飢餓永久遮蔽可退款資金。
- **風險:**目前沒有自動退款 handler 或已文件化的完整人工退款程序,資金可能停留在 controlled ATA。回應與 README 不得把 late funding 呈現為一般成功或暗示退款已完成。
- **觸發條件:**展期至獨立 `phase3-refund/reconciliation` 切片;管理任何非測試資金或宣告 production-ready 前(兩者取最早),必須一起交付並評審:(1)自動退款 handler 或明確、可操作且可稽核的人工程序;(2)cursor/keyset pagination;(3)row-level decode/error isolation;(4)證明 eventual coverage、損毀列隔離與完整退款／orphan sweep 的測試。未完成不得讓 executor 消費 late funding,不得宣稱 scan 完整,也不得關閉本債。
