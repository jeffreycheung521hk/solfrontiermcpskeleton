# Solfrontier / ClawSolana — MCP 重構建議書

**日期:** 2026-07-27 · **對象 repo:** `Solfrontier2026`(以此為準,`testingcrypto2` 為 Hackathon 提交版)

---

## 0. 一句話結論

你朋友的三點意見都成立,而且互相關聯:**架構問題的根源是 gateway 變成了 god-crate,體積問題的根源是自己養了一整套 LLM agent 執行環境 —— 而 MCP 化恰好同時解決這兩件事。** 這個項目的核心資產(typestate 簽名管線、policy engine、fail-closed 審批)完全不用重寫,要做的是「把周邊削掉,把核心用 MCP 包起來」。

---

## 1. 現況診斷(用數字說話)

Off-chain workspace(11 個 library crates + 4 個 bins)共 **153,202 行 Rust**;另有 2 個 on-chain programs(22,880 行,不在本次重構範圍)和一個 Next.js frontend。off-chain 部分分佈如下:

| Crate | LOC | 佔比 | 評語 |
|---|---:|---:|---|
| `gateway` | **121,975**(src 83,637 + tests 38,338) | **80%** | god-crate,問題核心 |
| `api` | 5,929 | 3.9% | Axum HTTP 層 |
| `state-store` | 5,643 | 3.7% | ✅ 邊界正確 |
| `types` | 4,933 | 3.2% | ✅ 邊界正確 |
| `solana-core` | 4,146 | 2.7% | ✅ 邊界正確 |
| `agent-runtime` | 3,204 | 2.1% | 自製 LLM stack,MCP 化後可整個刪除 |
| `tool-system` | 2,439 | 1.6% | ✅ 抽象正確,是 MCP 化的天然接口 |
| `risk-engine` | 2,159 | 1.4% | ✅ 邊界正確 |
| `wallet-engine` | 1,640 | 1.1% | ✅ 核心資產,不可動 |
| 其餘 | ~1,100 | 0.7% | observability (465) / channels (262) / 4 bins (407) |

### 1.1 做對了的部分(不要重寫)

你們的 `ARCHITECTURE.md` 其實寫得很好,以下是真正的資產:

- **Typestate 簽名管線**(`TransactionProposal → Simulated → Approved → Signed`,編譯期保證)—— 你們自己的 DEBT.md 也標明 "Do not touch",正確。
- **INV-1 ~ INV-8 系統不變量**,特別是 fail-closed、human-signs-AI-proposes。
- **`Tool` trait + capability-gated registry**(`tool-system`)—— 這個抽象和 MCP 的 tool 模型幾乎一比一對應,是 MCP 化最省力的原因。
- 底層 crates(`types` / `state-store` / `solana-core` / `wallet-engine` / `risk-engine`)邊界清晰。

### 1.2 做錯了的部分(你朋友說的「沒做好架構」具體是什麼)

1. **`gateway` 吸收了一切。** 80% 的程式碼在一個 crate 裡:Jupiter/Solend 整合、watcher、chat bridge、LLM intent extractor、審批、生命週期、daemon 接線全部混在一起。`daemon.rs` 一個檔現已 **3,062 行** —— DEBT.md D-2 在 2,341 行時已自認過大,之後又長了三成。

2. **Hackathon 沉積層。** `stage2_*` 系列 11 個檔共 **18,875 行**,是趕 deadline 時「新功能開新檔貼在 gateway 旁邊」的產物。檔名本身(stage2、w5h、w5i)就是 prompt-session 編號,不是架構語意。

3. **自己養了一套 agent 執行環境。** `agent-runtime` 裡有手寫的 Anthropic + OpenAI client、ReAct loop、personas、planner、conversation 管理;gateway 裡又有另一個 `stage2_llm_intent_extractor`(gpt-4o)。這是 2024 年式的做法 —— 今天這整層應該由 MCP host(Claude Desktop / Claude Code / 任何 MCP client)提供,你們不應該擁有這些程式碼。

4. **兩套 tool 系統。** `crates/tool-system/src/tools/` 和 `crates/gateway/src/tools/` 並存(DEBT.md D-1 的 Orca vs Jupiter 不對稱是它的症狀)。

5. **Frontend 承擔了 agent host 的角色。** Next.js chat UI + review card + funding card,其中 chat 部分在 MCP 世界裡是 host 的職責,你們只需要保留「Phantom 簽名頁」這一小塊。

---

## 2. MCP 重構方案

### 2.1 核心洞察

這個項目的本質是:**一個帶審批和策略閘的 Solana 交易工具箱**。這正是 MCP server 的形狀。現在的架構是「我們自己造 agent 來呼叫工具箱」;MCP 化之後變成「任何 agent(Claude、其他 host)透過標準協議呼叫工具箱」。你們的安全主張 *"The bound policy contract is the product, not the AI"* 在 MCP 架構下反而更純粹 —— 因為 AI 徹底移出了你們的程式碼庫。

```
現在:                              MCP 化後:

Next.js chat ──► api ──► agent-    MCP Host(Claude Desktop/Code,
                         runtime    任何 client)← 這層不再是你的程式碼
                         (自製LLM)        │ stdio (JSON-RPC)
                            │             ▼
                         gateway    solfrontier-mcp (rmcp server)
                         (80%LOC)     ├─ tools: propose/simulate/
                            │         │   quote/position/status
                            ▼         ├─ risk-engine (policy gate)
                         Solana       ├─ wallet-engine (typestate)
                                      └─ state-store (audit)
                                            │
                                            ▼
                                         Solana
                              (Phantom 簽名頁保留為獨立小型靜態頁)
```

### 2.2 MCP tools 設計(草案)

用官方 Rust SDK [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk)(`#[tool]` 宏 + schemars 自動生成 JSON Schema —— 你們 workspace 已經有 `schemars` 依賴,`ToolSpec` 遷移成本低)。

| MCP tool | 對應現有程式碼 | 說明 |
|---|---|---|
| `propose_intent` | `stage2_phase5c_draft` + canonical hash | 回傳 typed DraftIntent + SHA-256 hash,零 DB 寫入(保留現有語意) |
| `finalize_intent` | `stage2_w5h_funding_confirm` | 落庫 → 回傳 Phantom funding URL(deep link 到簽名頁) |
| `get_quote` / `get_position` / `get_balances` | `tools/get_jupiter_quote` 等 | 唯讀查詢,直接搬 |
| `simulate_transaction` | `solana-core` simulation | 唯讀 |
| `get_intent_status` | lifecycle / durable_pending | 查詢狀態機 |
| `list_pending_approvals` / `approve` | approval_store | approve 需 human-in-loop,見 2.4 |

**Resources**(唯讀掛載):audit log、policy 規則現況、mainnet proof 文件。
**Prompts**:預置的 intent 模板(如 conditional Solend deposit)。

### 2.3 傳輸層:stdio 優先

- **stdio transport** 完全符合並強化 INV-7(local-only):不再需要開 HTTP port、不再需要 bearer token、不再需要 rate limiting —— `api` crate 的大部分(auth、rate limit、session routes)直接刪除。
- 之後如需遠端,再加 streamable HTTP(rmcp 支援),那時才把 auth 加回來。

### 2.4 人手簽名(INV-1)在 MCP 下怎麼保住

這是唯一需要想清楚的地方,因為 MCP host 不能替用戶做 Phantom 簽名:

1. **Approve 閘:** `approve` tool 不由 LLM 自主呼叫 —— MCP host 本身的 tool-approval UI(如 Claude 的允許/拒絕對話框)提供第一層人手確認;你們的 policy engine 維持第二層 fail-closed。
2. **鏈上簽名:** `finalize_intent` 回傳一個帶 intent hash 的 URL,指向保留下來的**獨立靜態簽名頁**(現有 `bridge/index.html` 的擴充版):用戶開瀏覽器 → Phantom 簽 **一次** `TransferChecked` + Memo → 後端 memo-exact + delta-exact 驗證 → `budget_reserved`。現有流程不變,只是入口從 chat UI 改成 host 給的連結。
3. Watcher/executor(W5i)照舊:controlled-wallet 在簽好的 bounds 內執行,主錢包永不簽 Solend 指令。

**所有八條 INV 在新架構下逐條保持成立**,其中 INV-7 變得更強(無 port)。

### 2.5 目標 crate 佈局

```
crates/
  types            (不動)
  state-store      (不動)
  solana-core      (不動)
  wallet-engine    (不動 — typestate 管線)
  risk-engine      (不動)
  tool-system      (改造:Tool trait → rmcp tool adapter,一次寫好)
  protocols/       (從 gateway 抽出:jupiter/, solend/ — 按你們自己的
                    §7.1 builders/executors 準則分層)
  executor         (從 gateway 抽出:watcher + CAS lease + controlled-wallet)
bins/
  solfrontier-mcp  (新:rmcp stdio server,取代 clawd + api 大部分)
  keygen           (保留)
web/
  signing-page     (縮減:只剩 Phantom funding/簽名頁,靜態,無 chat)
刪除:
  agent-runtime    (整個 — LLM client、ReAct、personas、planner)
  channels         (CLI channel 由 MCP host 取代)
  api              (大部分 — stdio 不需要 HTTP surface)
  frontend chat    (Next.js chat/review 介面)
  bins/claw        (CLI client 由 MCP host 取代)
```

---

## 3. 編譯體積問題

### 3.1 為什麼大

- **4 個 bins 各自靜態連結整個世界**(tokio + solana-sdk 系 + sqlx + axum + reqwest),每個 binary 都是全量複製。
- `tokio features=["full"]`、`solana-client` + `solana-pubsub-client` + `solana-transaction-status` + `solana-account-decoder` 全家桶。
- 你們的 release profile 其實已經做對一半:`lto="thin"`、`codegen-units=1`、`strip=true` 都有了。

### 3.2 建議(按性價比排序)

```toml
[profile.release]
opt-level = "z"        # 3 → z:體積優先,對這種 IO-bound 服務性能損失可忽略
lto = "fat"            # thin → fat
codegen-units = 1      # 已有
strip = true           # 已有
panic = "abort"        # 移除 unwinding 機制,可觀縮減
```

1. **Bins 合併為一個**,用 clap subcommand(`solfrontier mcp` / `solfrontier keygen`),4 份靜態連結變 1 份 —— 這是最大單項節省。
2. **砍 features:** `tokio` 改列舉實際用到的 features;確認 `solana-pubsub-client`、`spl-token-2022` 是否真的在用(grep 之後不用就移出 workspace deps);MCP stdio 化後 `axum`/`tower`/`tower-http` 從預設 build 移除。
3. **量測工具**(在你本機跑,見 3.3):
   ```bash
   cargo install cargo-bloat cargo-udeps
   cargo bloat --release --crates -n 20   # 哪些 crate 佔體積
   cargo +nightly udeps                   # 找沒用到的依賴
   ```
4. MCP 化本身就是最大的減量:刪掉 agent-runtime(reqwest LLM 呼叫鏈)、api(axum 全家)、channels 之後,依賴樹直接少一截。

### 3.3 給你的操作提醒

我在這個沙盒**刻意完全沒有編譯**(Rust target/ 動輒數 GB,會爆磁碟配額)。上面的量測命令請在你自己機器跑;第一次 `cargo bloat` 的輸出貼給我,我可以幫你逐項分析。

---

## 4. 遷移路線(尊重你們 DEBT.md 自己的規則)

你們 DEBT.md 的 PC-1/PC-2/PC-3(路徑審計、一 PR 一主題、由低風險到高風險)寫得很好,以下路線遵守它:

| Phase | 內容 | 風險 | 產出 |
|---|---|---|---|
| **0. 量測與凍結** | 本機跑 `cargo bloat`/`udeps` 存底;gateway 停止加新功能(attrition 原則) | 零 | 體積基準線 |
| **1. MCP 骨架** | 新 bin `solfrontier-mcp` + rmcp;先只包 3 個唯讀 tools(quote/position/status),接上 Claude Desktop 驗證端到端 | 低 | 可 demo 的 MCP server,舊系統不動 |
| **2. 寫路徑遷移** | `propose_intent`/`finalize_intent`/簽名頁 URL 流程遷入;approval 走 host 確認 + policy gate | 中 | 完整 bounded intent loop 走 MCP |
| **3. 抽 protocols/executor** | Jupiter/Solend 按 §7.1 builders/executors 從 gateway 抽出;watcher 獨立 | 中高(路徑大改,遵守 PC-2 一次 PR) | gateway 瘦身 |
| **4. 刪除舊層** | agent-runtime、channels、api HTTP surface、frontend chat、`claw` CLI 移除;bins 合併;size profile 上線 | 低(純刪除) | 體積目標達成 |

每個 Phase 結束都是可運行狀態;Phase 1 之後隨時可以拿去 demo(「用 Claude 直接操作 bounded Solana intent」本身就是比原 chat UI 更有說服力的 pitch)。

---

## 5. 風險與明確不做的事

- **不動 typestate 管線、不動 `types`/`state-store`/`wallet-engine` 邊界** —— DEBT.md 已標記,且它們是審計/證明的信任錨。
- **不追求「大爆炸重寫」。** 153K 行裡真正要刪/搬的集中在 gateway 和 agent-runtime;底層五個 crate 原封不動,mainnet proof 的可信度得以延續。
- **on-chain programs(`clawsol-intent`/`clawsol-authority`)不在本次範圍**,MCP 是 off-chain 控制面的事。
- **主要風險:** Phase 3 的 gateway 抽取會 cascade 大量 `use` 路徑 —— 嚴格按 PC-2「一 PR、不混邏輯改動」執行,並先跑 PC-1 路徑審計。

---

## 附:給你朋友的一句回應

> 「原意有趣但架構沒做好」—— 同意,但要補充:壞的是 20%(gateway 沉積層 + 自製 agent stack),好的是那 80% 沒人注意的地方(typestate、fail-closed、crate 邊界)。MCP 化不是推倒重來,是把這個項目「本來就該是的形狀」顯影出來:它從第一天起就是一個 policy-gated tool server,只是當時 MCP 還不夠普及,所以自己造了 host。現在把 host 還給生態,留下真正有價值的控制面。
