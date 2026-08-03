# Phase 4:退款 rail(INV-1 第二條窄例外)

本文件記錄 `solfrontier-mcp refund` 的設計、它為何構成 INV-1 的**第二條**窄例
外,以及這條例外的邊界在哪裡。它必須與 PR #17 的 `watch --execute` 例外讀得
一樣窄。

## 為什麼需要

兩次 mainnet funding 嘗試把 0.4 USDC 留在 controlled ATA:

| Intent | 原因 | 狀態 |
|---|---|---|
| `5b000000…`(2026-08-01) | executor 在 deadline 後 107 秒才啟動 | 卡住 |
| `58000000…`(2026-08-02) | 客戶端 memo 預篩丟棄了唯一正確的交易 | 卡住 |

錢**不是消失,是卡住**:controlled ATA 的 authority 就是 controlled wallet。
在此之前沒有任何路徑能在**不冒重複送出風險**的前提下把它退回去。

## 原本就存在的部分

凍結的 state-store 早已具備完整退款狀態機,本 PR **未修改任何凍結 crate**:

- `lease_refund_if_expired_or_past` — `budget_reserved | expired → refunding`,
  且原子 WHERE 硬性要求 `expires_at_ms <= now_ms`
- `mark_refunded_if_refunding` — `refunding → refunded`,寫入 `refund_signature`
- `refund_signature` 欄位與狀態機註解 — migration `0012`
- execution / refund lease **互斥**:同一 row 不可能兩條終局路徑並存

## 唯一的缺口,以及它為何不能用記帳解決

`refund_signature` 由 `mark_refunded_if_refunding` 寫入,而那是**確認之後**。
daemon 若在「簽名 → 廣播 → 崩潰」之間死亡,row 停在 `refunding` 而沒有任何
記錄說它送出了什麼。此時重簽會產生第二筆帶新 blockhash 的交易;若第一筆已
落鏈,就是從共用錢包**雙重退款**。

`refund_journal.rs` 以關閉 `DEBT-MCP-1` 的同一模式補上:bin 自有的 SQLite,
放在主 DB 旁,只存衍生資料,`synchronous = FULL`。**journal 永遠不授權任何狀
態轉換**,它只告訴恢復程序「已經對哪些位元組做過承諾」。主 DB 仍是唯一真相。

裁決者是 **blockhash 過期**——鏈上事實,不是我們對自己記帳的信心:

| 狀態 | 動作 |
|---|---|
| 鏈上找到 | reconcile |
| 不在鏈上、仍在有效期 | **重送同一份位元組**,絕不重簽 |
| 不在鏈上、已過 `last_valid_block_height` | 重簽是**可證明**安全的 |
| journal 讀不到 | **停止**——無法區分「從沒送出」與「送了但丟失」 |

`JournalLookup::Unavailable` 與 `Absent` 刻意分開。合併它們會讓一個**損壞的
journal 授權一次新簽名**,而那正是本模組存在的理由。

## 為何這是 INV-1 的例外

把資金移出 controlled ATA 需要 controlled wallet 簽名。因此本 rail 與 PR #17
同級:非 MCP、operator 明確啟動的 CLI,釘死單一錢包、單一 mint、單一交易形狀。
**任何 MCP tool 或 LLM 都無法觸及它。**

### operator 的權限刻意極窄

operator 只能指定 **intent**。金額、來源 ATA、目的地 ATA、目的地錢包**全部從
funding row 推導**(`plan_from_row`)。沒有任何旗標可以改變送多少或送去哪裡。
有一條測試釘死這件事:若日後需要一個不是 DB 欄位的參數,代表這條 rail 長出了
它被設計成不該有的權力。

### gate 順序,不可省略

```
recover → lease → 重新推導 → build → simulate → policy → Approved typestate
        → sign → JOURNAL → broadcast → finalized → 驗雙邊 delta → mark_refunded
```

- **recover 在最前面。** 前一次嘗試可能已經在鏈上,事後才發現就是太遲。
- **lease 之後重新讀 row 並重新推導。** 被簽的 plan 必須來自 CAS 之後的 row,
  不是 CAS 之前那次讀取。
- **JOURNAL 夾在 sign 與 broadcast 之間**,因為那是它唯一有用的位置。寫入失
  敗即中止且不廣播:未廣播的簽名無害,未記錄的廣播不是。
- **只接受 `finalized`。** `confirmed` 是 funding 的容忍度,不是退款的:
  confirmed 區塊仍可能回滾,而一筆被鏈遺忘的退款會留下 `refunded` 狀態與仍在
  controlled ATA 的錢。

### 刻意複製而非重構

`refund_wallet.rs` 複製 `watch_wallet.rs` 的形狀,沒有把它一般化。execute rail
是本專案唯一有主網證據的路徑;把它的驗證器放寬到同時接受純 SPL 轉帳,等於鬆掉
正是讓它可信的那些檢查,而把該重構混進一個引入新金流 rail 的 commit,正是 PC-2
要防止的事。兩個各自只接受單一形狀的窄驗證器,比一個接受兩種的通用驗證器安全。

對 `watch_wallet.rs` 只加了 `LoadedControlledWallet::signer_ref()`,回傳的是
凍結 keystore 的 handle 而非金鑰材料;execute rail 行為零改變。

### policy 註記

退款是一筆 SPL 轉帳,而 policy set 有一條 `LegacyTokenTransferPresent` 拒絕。
`TransferChecked` 攜帶 mint 與 decimals——**正是該條件用來區分不透明 tag-3
`Transfer` 的那兩個欄位**——且兩者在驗證器中逐位元釘死,所以 mint 與金額條件
真的看得見這筆指令。`is_legacy_token_transfer: false` 是誠實的,不是繞過。

## 尚未交付(必須在真正退款前補齊或明確接受)

- **落鏈後的自動 `mark_refunded`。** `verify_refund_landed` 已完成且已測試,但
  編排目前在 `finalized` 之後停下並要求人工核對,尚未自動呼叫
  `mark_refunded_if_refunding`。
- **`ResendSame` 的自動執行。** 恢復判定會正確回報該重送,但目前 rail 在偵測
  到既有 journal 記錄時一律中止並要求人工介入。這是刻意的保守選擇。
- **驗收 DB 的寫入政策。** 兩筆卡住的資金在被 `STATUS.md` 宣告為不可寫稽核底
  稿的 DB 裡。退款後寫入 `refunded` 會修改該底稿;不寫則讓底稿與鏈永久矛盾。
  **此決定尚未做出,必須在首次真實退款前明確記錄。**
- **`DEBT-P2-FUNDING-1` 不因本 PR 關閉。** 單筆 refund rail 不提供 cursor
  pagination、逐列隔離,也不提供可稽核的全量退款 sweep。

## 邊界與理由終點

本例外只覆蓋:mainnet USDC、釘死的 controlled wallet、單筆全額退款、測試資金。
不得外推成 generic autonomous custody。擴大 mint、允許部分退款、管理非測試資
金或宣告 production-ready 之前,必須重新評審並補齊上述未交付項目。
