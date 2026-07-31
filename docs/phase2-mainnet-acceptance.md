# Phase 2 主網驗收紀錄

## 結論

**PASS。** 2026-07-29，SolFrontier MCP 在 Solana mainnet 完成第一筆
Phase 2 全流程驗收：

`AI typed proposal → human fingerprint approval → finalize → Phantom sign → confirm_funding → budget_reserved`

本次使用 0.1 USDC 真實入金。交易、地址與雜湊均為公開鏈上資料；本文件
不含 RPC API key、私鑰、助記詞或其他秘密。

## 可公開核驗的證據

| 項目 | 值 |
|---|---|
| Transaction | [`67PPnfsYT6qFvEwiyyTojkqNE2E6UwiYq1qCRHW6g9hpvyVMNEWymjxSSz8Zwrn9eqDzMxvEfVTqUY3xe4NJcJHV`](https://explorer.solana.com/tx/67PPnfsYT6qFvEwiyyTojkqNE2E6UwiYq1qCRHW6g9hpvyVMNEWymjxSSz8Zwrn9eqDzMxvEfVTqUY3xe4NJcJHV) |
| 鏈上時間 | 2026-07-29 01:04:52 UTC / 09:04:52 +08:00 |
| 金額 | 0.1 USDC / `100000` raw units |
| USDC mint | `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v` |
| 出資錢包 | `C4QQjzWxnJ5QFAbkzhQJ3wTzyX6nw1vyFvJwbPXJGPNW` |
| 來源 USDC ATA | `4bDArC4UVoBjecHUZaCKqhuhTYPoiKS4qpQQpxuvm1qn` |
| Controlled wallet | `BPfDMmeMBmCbMC1rWh7hwigMBoKGBrKwXxSeUu9hhs5L` |
| 收款 controlled ATA | `7LFdKcSV7JQYi3or5y9phHVPjhGigu5DDjUakAFbBmk3` |
| Intent ID | `32000000a0860100000000009a62dace` |
| Canonical rule hash | `ac85744ed56adc7e7e5f85007b4a6397d41ca835f3646a627b39d7289782dcca` |
| Canonical draft hash | `96eea70c3a0b75bc5220e36939c81cfb30b9b313479609f3471958537b5fb0b0` |
| Transaction slot | `435850261` |
| WatchRule expiry slot | `435850484` |
| Funding row expiry | 2026-07-29 01:06:07.381 UTC |
| 驗收時觀測的 confirmation | `finalized`（程式門檻仍為 `confirmed`） |
| 最終 DB 狀態 | `budget_reserved` |

交易 Memo 原文：

```text
claw:w5h:32000000a0860100000000009a62dace:ac85744ed56adc7e7e5f85007b4a6397d41ca835f3646a627b39d7289782dcca
```

## 實際驗證內容

1. `propose_intent` 對已核對的 typed 參數產生上述 draft hash，並回報
   DB writes、network calls、signatures 均為 0。
2. 人工核對 fingerprint 後才呼叫 `finalize_intent`。它建立
   WatchRule/funding row，回傳完整 funding 指引與 180 秒絕對期限。
3. 簽名頁只從 loopback 載入本地 vendored `@solana/web3.js`，Phantom
   連接錢包必須精確等於 finalize 登記的出資錢包。
4. Phantom 簽署的指令順序為 Memo index 0、TransferChecked index 1；
   頁面沒有接觸私鑰，也沒有把 RPC key 放進瀏覽器。
5. `confirm_funding` 從鏈上獨立驗證：
   - Memo 全文精確相等；
   - 登記錢包是 signer；
   - 來源 ATA 精確減少 `100000`；
   - controlled ATA 精確增加 `100000`；
   - mint、兩側 owner、ATA 與 decimals 全部吻合；
   - 交易至少達到程式要求的 `confirmed` commitment。
6. 驗證全部通過後才依序執行兩個 CAS：
   `funding_required → funding_submitted → budget_reserved`。
7. 以 intent ID 與 canonical rule hash 各自呼叫
   `get_intent_status`，兩次都解析到相同 intent 並回傳
   `budget_reserved`；WatchRule 仍為 `active`。

本次 transaction `blockTime` 比 funding row 的絕對期限早 75.381 秒，
`arrived_after_funding_deadline` 為 `false`，因此沒有觸發 late-funding
人工退款路徑。交易 slot 距 WatchRule expiry 尚有 223 slots。

## RPC 與信任邊界

最初使用的 Solana 公共 endpoint 在命令列可取得 blockhash，但瀏覽器帶
`Origin: http://127.0.0.1:8080` 時，CORS preflight 成功後的 JSON-RPC
POST 回 HTTP 403。簽名頁因此改用經相同 Origin 實測成功的 keyless
`https://solana-rpc.publicnode.com`，而 CSP 是**替換**舊 origin，不是
同時開放兩個來源。

此 RPC 只提供 recent blockhash，不是金錢的信任錨。金額、收款地址、
mint、Memo 與指令順序來自已核對的 finalize 回應，`confirm_funding`
又從鏈上獨立重驗。惡意或不可用的 blockhash provider 可以令交易失敗
或過期，但不能把轉帳改送到其他地址。

## Commitment 風險沒有因本次觀測而消失

本次呼叫 `confirm_funding` 時，RPC 已回報交易為 `finalized`；但程式碼
要求的門檻仍是 `confirmed`。這次較硬的觀測是運行時機結果，不是新的
安全保證，也沒有關閉 CLAUDE.md 已記錄的 confirmed rollback 接受風險。
當系統開始管理超出測試規模或非測試資金時，必須重新評估是否改為等到
`finalized` 才轉成 `budget_reserved`。

## 對應版本與合併

- 真鏈驗收程式 commit：`53d7092` (`fix: use browser-compatible public rpc`)
- Vendored runtime commit：`4d89596`
- 文件信任邊界 commit：`7dbd708`
- PR：[#4](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/pull/4)
- 驗收程式進入 `main` 的 merge commit：`43f9827`

## 驗收資料庫離線備份

2026-07-31 已在確認 SQLite 三檔無開啟程序後,把
`data/acceptance/phase2-mainnet-20260729-04.db{,-wal,-shm}` 複製到
`E:\SolFrontierAcceptanceBackup\phase3b-disk-diagnostic-20260731\`。
來源與備份合計皆為 757,896 bytes,逐檔大小及 SHA-256 完全相同:

| 檔案 | Bytes | SHA-256 |
|---|---:|---|
| `phase2-mainnet-20260729-04.db` | 4,096 | `1FE8F6113488865C546D2FAA55B21482662CE4BE19D4F505EEEFA09BC3131489` |
| `phase2-mainnet-20260729-04.db-wal` | 721,032 | `61AA916B62A5FD14D56B692606E5B5A29B02C96FC4C31BA95E08322065B643C9` |
| `phase2-mainnet-20260729-04.db-shm` | 32,768 | `E58278973DCE35EEFE4FB2EB3DE46E6522CC23B4CAD7BF1F9609C57679686538` |

此路徑是本機可移除 E 槽上的災難復原索引,不是 repo 內容;原始
acceptance 三檔與備份均不得作為測試寫入目標。測試必須複製完整
`.db`/`-wal`/`-shm` family 到暫存目錄後才可開啟。

## 合併後 main gate

在 `main@43f9827` 重新執行：

- `cargo fmt -p solfrontier-mcp -- --check`：通過
- `cargo check --workspace`：通過
- `cargo test --workspace`：350 passed、0 failed、3 ignored
- `cargo build --release`：通過
- release binary：5,321,728 bytes（5.075 MiB）
- `cargo bloat --release --crates -n 20`：通過；`.text` 3.6 MiB，
  file size 5.1 MiB
- offline MCP stdio smoke：六個 tools 全部可見；`propose_intent`
  回傳相同 draft hash 且零 side effects；stdout 僅 JSON-RPC
- signing page：vendored SRI、inline JavaScript syntax、loopback HTTP、
  CSP、CORS preflight 與 Origin-bearing blockhash POST 全部通過
- `git diff --exit-code -- crates`：通過

workspace tests 第一次曾遇 Windows linker 的瞬時 `LNK1104` 檔案鎖；
確認沒有殘留 cargo/rustc/MCP/test process、未修改任何程式後原命令重跑
即全綠，沒有測試案例失敗。
