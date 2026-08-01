# Phase 3b-2 主網執行驗收紀錄

## 結論

**PASS（限定範圍）。** 2026-08-01，SolFrontier MCP 在 Solana mainnet 首次完成
Phase 3b-2 的 controlled-wallet 執行全流程：

`propose_intent → finalize_intent → Phantom 人手簽名入金 → confirm_funding →
budget_reserved → watch --execute（CAS lease → simulation → policy → 簽名 →
broadcast → finalized）→ completed`

本次使用 0.2 USDC 真實資金。交易、地址與雜湊均為公開鏈上資料；本文件不含
RPC endpoint、API key、私鑰或助記詞。

**這次驗收證明的範圍僅限：**單筆、固定 mainnet Solend USDC deposit、釘死的
controlled wallet、測試規模資金、由 operator 明確啟動的非 MCP CLI。它**不**證明
production-ready、restart-safe、完整 reconciliation 或 generic autonomous
custody。下文「未關閉的技術債」逐項列明尚缺什麼。

## 可公開核驗的證據

| 項目 | 值 |
|---|---|
| Deposit transaction | [`hpVwPcMwy6RWATk2ks7HPEo8bS4zGAPaL7gx4BcdpBqgs4d27NrD6YF73T8nk9gokiatT7iGx2YdrNupzcdx6J4`](https://explorer.solana.com/tx/hpVwPcMwy6RWATk2ks7HPEo8bS4zGAPaL7gx4BcdpBqgs4d27NrD6YF73T8nk9gokiatT7iGx2YdrNupzcdx6J4) |
| Deposit 鏈上時間 | 2026-08-01 10:57:37 UTC / 18:57:37 +08:00 |
| Deposit slot | `436548513`，`confirmationStatus: finalized`，`err: null` |
| Deposit fee | 25,000 lamports |
| Funding transaction | [`2wsxko7HiZzBpfoewzeyD4JD52RbzW6k1qFz2buvLaR1fJR8SXb7jZAQsBWeioTArQx65xxd4mNRLMwLnwRrnJt4`](https://explorer.solana.com/tx/2wsxko7HiZzBpfoewzeyD4JD52RbzW6k1qFz2buvLaR1fJR8SXb7jZAQsBWeioTArQx65xxd4mNRLMwLnwRrnJt4) |
| Funding 鏈上時間 | 2026-08-01 10:56:55 UTC / 18:56:55 +08:00，slot `436548415` |
| 金額 | 0.2 USDC / `200000` raw units |
| USDC mint | `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v` |
| 出資錢包 | `C4QQjzWxnJ5QFAbkzhQJ3wTzyX6nw1vyFvJwbPXJGPNW` |
| 來源 USDC ATA | `4bDArC4UVoBjecHUZaCKqhuhTYPoiKS4qpQQpxuvm1qn` |
| Controlled wallet | `BPfDMmeMBmCbMC1rWh7hwigMBoKGBrKwXxSeUu9hhs5L` |
| Controlled USDC ATA | `7LFdKcSV7JQYi3or5y9phHVPjhGigu5DDjUakAFbBmk3` |
| Controlled collateral ATA | `BQazv4UQNFV8t4QGVntELr1ee3bCeTN8AvdPGRutFKn7` |
| Intent ID | `5a000000400d0300000000009a62dace` |
| Canonical rule hash | `a28eaf630890d0830ca822c754766689eb75549ed559025d404cb46dec08f1d5` |
| Canonical draft hash | `919f478e954582cece630e6b486edf4373d87daa3a64caea6b6aef3291d2fbbd` |
| `original_user_message_hash` | `2d36696784a816e0303aca626b96eee35d3981e4db83c9ca97734bf89dc303cb` |
| WatchRule created / expiry slot | `436548274` / `436548754` |
| Funding row expiry | 2026-08-01 10:58:56.714 UTC |
| Solend program | `So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo` |
| Lending market | `4UpD2fh7xH3VP9QQaXtsS1YY3bxzWhtfpks7FatyKvdY` |
| USDC reserve | `BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw` |
| Reserve liquidity supply（收款） | `8SheGtsopRUDzdiD6v6BR9a6bqZ9QwywYQY99Fp5meNf` |
| Collateral mint | `993dVFL2uXWYeoXuEBFXR4BijeXdTv4s6BzsCjJZuwqk` |
| Target obligation | `BdFLjCcP9mCy557vNNGVbTUuvHxXsh8hc6jXzaPra1wN` |
| 最終 DB 狀態 | `funding=completed`、`watch_rule=completed`、`is_terminal: true` |

Funding 交易 Memo 原文：

```text
claw:w5h:5a000000400d0300000000009a62dace:a28eaf630890d0830ca822c754766689eb75549ed559025d404cb46dec08f1d5
```

Canonical draft hash 是 typed 參數的純函數。上表數值是在驗收後以完全相同的
參數重放 `propose_intent`（`database_writes: 0`、`network_calls: 0`、
`signatures: 0`、`db_row_exists: false`）取得；`finalize_intent` 當時已重算並
核對過同一值，故兩者必然相等。`draft_id` 是隨機且非正典的關聯值，不進任何
指紋，因此不列入本紀錄。

## 鏈上餘額變化（獨立於程式回報自行核驗）

| 帳戶 | Pre | Post | Delta |
|---|---:|---:|---:|
| Controlled USDC ATA `7LFdKcSV…` | 1,253,487 | 1,053,487 | −200,000 |
| Obligation `BdFLjCcP…` 抵押品 | 2,121,745 | 2,275,267 | +153,522 cToken |

Deposit 走的是 Solend `Deposit Reserve Liquidity and Obligation Collateral`，
抵押品直接記入 obligation，不會在 controlled wallet 留下 cToken ATA 餘額；本次
觀測到的 controlled cToken 餘額前後皆為 `0`，與該指令語意一致，不是遺失。
+153,522 cToken 對 200,000 USDC raw 隱含匯率約 1.3028，符合成熟 USDC reserve。

## 完整時間線（+08:00）

| 時間 | 事件 |
|---|---|
| 18:51:12 | operator 在自有 PowerShell 啟動 `watch --execute` 迴圈（先於 finalize） |
| 18:51:12–18:55:56 | 迴圈在空白 DB 上以 `"rows": []`、`scan_errors: []` 安全 idle |
| 18:55:56.714 | `finalize_intent` 寫入 WatchRule/funding row，180 秒絕對期限起算 |
| 18:56:55 | Phantom 人手簽名的 funding 交易上鏈 |
| 18:57:10 | 監看程序偵測到新簽名（以 23 條既有簽名為 baseline 排除舊交易） |
| 18:57:11 | `confirm_funding` → `budget_reserved`，尚餘 106 秒 |
| 18:57:11–18:57:36 | 迴圈連續 12 次 `reserve_stale`，逐次 fail closed 不取 lease |
| 18:57:36.9 | 抓到 reserve 新鮮窗口：CAS lease → 重建 → simulation |
| 18:57:37.3 | risk-engine `verdict=approved` → 簽名 → broadcast |
| 18:57:37 | Deposit 交易上鏈，slot `436548513` |
| 18:57:52.5 | 觀測到 `finalized` 後寫入 `completed`，迴圈 `STOP completed` 自行結束 |
| （18:58:56.714） | funding row 名義期限；本次在此之前 79 秒完成 |

全程 73 次 attempt，其中 12 次 `reserve_stale`。`abort_remaining_cycle` 為
`false`，沒有任何 `*_scan_failed` / `*_scan_truncated`。

## 實際驗證的 gate 鏈

1. `propose_intent` 為純計算：無 DB row、無 network、無 signature。
2. `finalize_intent` 重算並核對 draft hash 後，才依舊系統的非事務順序建立
   WatchRule、sidecar 映射與 funding intent；此步零簽名、零交易建構、零廣播。
3. 簽名頁只從 loopback 載入本地 vendored JS；Phantom 連接的錢包必須精確等於
   finalize 登記的 `user_wallet`。指令順序為 Memo index 0、TransferChecked
   index 1。MCP binary 全程只提供 stdio，未開任何 HTTP port。
4. `confirm_funding` 從鏈上獨立重驗 Memo 全文、signer 身分、來源 ATA 精確
   減少 `200000`、controlled ATA 精確增加 `200000`、mint/owner/decimals 吻合，
   全數通過後才依序執行兩個 CAS。本次 `arrived_after_funding_deadline: false`、
   `late_funding: null`，未觸發 late-funding 路徑。
5. `watch --execute` 每輪先完成全批 scan/precheck；本次全程無 scan flag。
6. 條件重算：`threshold_wad` `9000000000000000`（90 bps），實測
   `observed_apr_wad` `18826858686431800`、`observed_apr_floor_bps` `188`，
   `met: true`。Reserve 新鮮度必須 ≤ 16 slots，未達即 `reserve_stale` 並跳過。
7. 三方金額精確等值：`action_input_amount_raw` = `rule_max_input_amount_raw` =
   `funding_amount_raw` = `200000`。
8. CAS `budget_reserved → executing` 成功後才重建交易，經 wallet-engine
   simulation（`simulated_cu=37181`、`derived_cu_limit=40899`、
   `applied_cu_limit=200000`）與真實 risk-engine policy
   （`rules=6`、`programs_allowed=2`、`verdict=approved`、proposal
   `c7ad2c9f-4eae-44b7-baf2-eaf9ac84fe0a`）。
9. 交易含 `RefreshReserve` 與 deposit 兩條 Solend 指令，reserve 與 deposit 均為
   fingerprint-bound；controlled wallet 為唯一 signer。
10. 只以 `finalized` 為成功；確認後 `WatchRule → completed`，再把 funding row
    `executing → completed`。本次兩步皆成功，未出現
    `rule=completed / funding=executing` 的裂解狀態。

## 前一次嘗試（FAIL）與其未清償後果

同日先有一次失敗嘗試，必須連同本紀錄一併保存，不得只記錄成功那次。

| 項目 | 值 |
|---|---|
| Intent ID | `5b000000400d0300000000009a62dace`（threshold 91 bps） |
| Canonical rule hash | `3d96e1c4418868ad543fa84420d79ffa29d494a3f7e1d93b31cc61a8e1f6b596` |
| Funding transaction | [`5caGiE8SBsPo4TujBrTVR7xFdNmn5XPjdaiEvdxuXLnfnoQJyHo5aFYrx9e8cse2BUFj8yoPEboHmgvdEBMf6ya6`](https://explorer.solana.com/tx/5caGiE8SBsPo4TujBrTVR7xFdNmn5XPjdaiEvdxuXLnfnoQJyHo5aFYrx9e8cse2BUFj8yoPEboHmgvdEBMf6ya6) |
| DB | `data/acceptance/phase3-mainnet-20260801-01.db` |

時間線：`finalize_intent` 18:08:14（期限 18:11:14）→ funding 交易 18:09:01
上鏈（**準時**）→ `confirm_funding` 18:10:32 完成，僅餘 42 秒 → executor
18:13:00 才啟動。啟動時 `now_ms` 1785579181348 已超過 `expires_at_ms`
1785579074221 達 107.127 秒，`current_confirmed_slot` 436542150 亦已超過
`expires_at_slot` 436541956 達 194 slots。

雙時鐘同時逾期，gate 拒絕取得 lease，**沒有簽名、沒有廣播、沒有狀態變更**。
這是正確的 fail-closed，不是故障；wrapper 因 `wall_clock_expired` 不屬安全重試
集合而正確停止。

**未清償後果：**該 0.2 USDC 已進入 controlled ATA 並停在 `budget_reserved`。
`lease_execution_if_budget_reserved` 的原子 WHERE 要求
`expires_at_ms > lease_now_ms`，故此 row 永遠無法再執行；目前亦無自動退款
handler（`DEBT-P2-FUNDING-1`）。這筆資金需人工對鏈與人工退款，**不得**因為後續
成功那次而視為已清理——兩者是不同 intent、不同資金。

## 為何要把 executor 排在 finalize 之前（可重現的操作結論）

失敗那次的根因純粹是次序：所有人手步驟都排在 executor 之前，把 180 秒窗口耗盡。
成功那次把 `watch --execute` 移到 `finalize_intent` **之前** 啟動。這安全的原因
是 `bins/solfrontier-mcp/src/watch.rs:610` —— 已有 funding row 但尚未
`budget_reserved` 的 pending rule 完全不產生 row，因此掃描回 `"rows": []`，屬
安全重試，迴圈可無限期 idle 等待。

另外實測發現真正的瓶頸不是 180 秒窗口，而是 **Solend reserve 的刷新頻率**。
2026-08-01 連續取樣 151 秒（每 2 秒一次，共 71 個樣本）：

| 指標 | 值 |
|---|---|
| 觀測到的 reserve 刷新次數 | 3（約每 60 秒一次） |
| 符合 ≤ 16 slots 的樣本比例 | 13%（71 取 9） |
| reserve age 中位數 / 最大 | 62 / 146 slots |
| 最長連續不合格時段 | 52 秒 |

合格窗口約 6.4 秒，而迴圈週期約 5.3 秒，因此每個窗口通常能被抓到一次；本次即
等待約 25 秒、12 次 `reserve_stale` 後成功。但這代表每輪驗收都必須為 reserve
新鮮度預留最多約 60 秒預算。此為執行期環境特性，非程式缺陷，`reserve_stale`
本身即為正確的 fail-closed。

亦注意：`intent_id` 由 typed 參數決定（bytes 0–3 為 `threshold_bps` LE、
4–11 為 `amount_raw` LE），完全相同的參數會重現完全相同的 Memo。本次刻意採用
90 bps 而非 91 bps，令新 intent 與失敗那次的鏈上 Memo 不同；`confirm_funding`
另以既有簽名 baseline 排除舊交易。若未來重跑相同參數，必須保留至少其中一項
防護，否則已上鏈的舊 funding 交易可被用來滿足新 intent。

## 刻意偏離與授權邊界

本次執行**沒有**呼叫 `clawsol-authority ExecuteAction`，**沒有**讀取
`AuthorizationRecord` PDA，也**沒有**把 on-chain authority 放進 Solend CPI
loop。通過所有 gate 後，是由釘死的 controlled-wallet keypair 直接簽署並呼叫
Solend。這一點不得被包裝成已部署的 user-delegated PDA execution——已部署的
authority action byte 只支援 1/2，Solend CPI builder 仍為 withdraw-only，
canonical `SolendDeposit=3` 目前絕不可宣稱為已部署 PDA grant
（`DEBT-P3-SCHEMA-1`）。

相對前身 W5i 的偏離方向是**收緊**而非放寬：前身 `stage2_chat_execute.rs:53-59`
明載 deposit 直接以受控錢包簽名、未經 review pipeline；本次則把同一份 fresh
unsigned transaction 依序送入 wallet-engine simulation、真實 risk-engine
policy、明確 auto-approved 的 `Approved` typestate 後才簽名。

人類授權邊界：使用者只以 Phantom 人手簽 funding，主錢包私鑰從未進入 daemon。
執行端的授權來自 operator 明確啟動非 MCP CLI、固定 controlled wallet、精確
canonical identity 與三方金額、fresh account/condition/雙時鐘檢查、
simulation/policy 與單一 CAS 的交集。MCP host / LLM 無法把一般 tool call 升格
成 `--execute`。本次 `SOLFRONTIER_CONTROLLED_WALLET_KEYPAIR` 只存在於 operator
自有的 PowerShell，未進入任何 MCP 設定。

## 未關閉的技術債

本次驗收**沒有**關閉下列任何一項，且不得被引用為已關閉：

- `DEBT-P2-FUNDING-1`：過期入金無自動退款；上節 0.2 USDC 即為實例。兩個
  bounded list API 仍無 cursor/keyset 與 row-level error isolation。
- `DEBT-P3-EXECUTION-1`：`executing` 無 crash recovery / reaper / lease TTL。
  本次兩步完成寫入僥倖同時成功，並不證明中斷情境已被處理。
- `DEBT-P3-EXECUTION-2`：final revalidation 與 funding CAS 不是單一快照；
  WatchRule revoke 與 `mark_completed` 之間的競爭窗口仍然存在。
- `DEBT-P3-KEY-1`：frozen `SecretKeystore` 內部 key material 零化仍不可證。
- `DEBT-P3-SCHEMA-1` / `-2`：PDA 授權與 decimals 進指紋均未處理。
- `DEBT-P3-FINALIZE-1`：仍只允許全額單次執行，無 partial fill。
- `DEBT-P2-FINALIZE-1` / `-2`：rule-only / funding-only orphan 的
  fault-injection 證據仍缺。

依 CLAUDE.md，管理任何非測試資金或宣告 production-ready 前（兩者取最早），
上述各項必須先行交付並獨立評審。

## 驗收環境與資料庫

- 分支：`feat/phase3-executor-execute`，HEAD `4f4ed53`
  （`docs: link executor safety record to PR`），worktree 乾淨。
- Release binary：5,639,680 bytes，build 於 2026-08-01 15:18:47 +08:00。
- Executor log：`data/logs/phase3-mainnet-watch-20260801-185112.stderr.log`
  （成功）。同日另有 `-181259`（失敗）與 `-160737`（空 DB idle）。
- 驗收 DB family（合計 1,196,360 bytes）：

| 檔案 | Bytes | SHA-256 |
|---|---:|---|
| `phase3-mainnet-20260801-02.db` | 376,832 | `971DB38C0A10DF81F567E9E95A419E90D78A06B79893E179E633D525AD1237AC` |
| `phase3-mainnet-20260801-02.db-wal` | 753,992 | `7A9F818088F47A4ECE68C6A575C6FA94FD16730244FB2DA3B02860E0817A94C5` |
| `phase3-mainnet-20260801-02.db-shm` | 32,768 | `47B3AF200447C460B32E7CF6106280F45B4CDC5868BA4F4CD268163292BEA22B` |
| `phase3-mainnet-20260801-02.db.mcp-intent-index.sqlite3` | 32,768 | `EBE7B3BE4B7118F17D41A3A66A9DF4584BFE2646DCB6E640FB46FB66828A3E0F` |

上列雜湊取於驗收完成後、`STOP completed` 之後。`-01`（失敗那次）與 `-02`
兩套 acceptance 檔案均為審計底稿，永久不可刪、不可作為測試寫入目標；需要
測試時必須先複製完整 `.db`/`-wal`/`-shm` family 到暫存目錄再開啟。

備註：新建 acceptance DB 時，零位元組檔案不足夠——dry-run 以 read-only /
query-only 開啟，無法建表，會回 `funding_scan_failed` 與
`watch_rule_scan_failed`。必須先以 MCP server 開啟一次令 migrations 生效，
確認 dry-run 回 `"rows": []` 且 `scan_errors: []` 後才可啟動 execute 迴圈；
否則 scan flag 會令整輪 abort。
