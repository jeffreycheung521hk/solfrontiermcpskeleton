# SolFrontier trust overview

> English translation: [TRUST.en.md](TRUST.en.md)
>
> **狀態註記(2026-08-01):**本文件寫於 PR #17 合併之前。下文「tree 仍
> 沒有 execution lease、signer、broadcast」等描述對應 Phase 3b-1 當時
> 狀態,為存真保留。PR #17 其後加入 operator 啟動的非 MCP
> `watch --execute` rail,並於 2026-08-01 完成首次主網執行驗收;現行
> 狀態(含保留的失敗紀錄與明確未關閉的債務)見
> [Phase 3b-2 主網驗收紀錄](phase3b-2-mainnet-acceptance.md)。

這份文件給外部覆核者一條約十分鐘可走完的證據路徑。它只整理目前
repository 已能公開重算的事實，不把 Phase 3b-1 的唯讀 dry-run、
測試 fixture 或一次成功觀測描述成已交付可簽名 executor 的安全保證。

## 十分鐘覆核路徑

1. 先看下方的凍結區，確認哪些 trust anchors 從未修訂，哪些曾經公開修訂。
2. 在 Solana Explorer 打開兩筆交易，分清「本 repo 的 Phase 2 入金驗收」
   與「前身 Solend deposit 的外部對帳樣本」。
3. 到 `crates/types/src/stage2_watch_rule.rs` 重算三個 golden hash，確認
   schema v1 的兩個身份沒有因 append-only v2 修訂而改變。
4. 查看 `bins/solfrontier-mcp/src/watch.rs` 的 read-only/query-only
   connection、四個公開 repository list/get 呼叫與
   `Transaction::new_unsigned`，再重跑 PR 描述中的 dangerous-symbol grep。
5. 到 GitHub Actions 或 PR 的 Checks 頁確認 `Windows validate` 綠燈。
6. 最後閱讀已知邊界；CI 綠燈、golden tests 和主網樣本都不會消除這些邊界。

## 凍結區現狀

| 路徑 | 現況 | 可核驗依據 |
|---|---|---|
| `crates/observability` | 原封搬入，未修訂 | 六路徑 frozen-core guard |
| `crates/solana-core` | 原封搬入，未修訂 | 六路徑 frozen-core guard |
| `crates/wallet-engine` | 原封搬入，未修訂 | 六路徑 frozen-core guard |
| `crates/risk-engine` | 原封搬入，未修訂 | 六路徑 frozen-core guard |
| `crates/types` | 經一次公開 append-only 修訂後重新凍結 | [PR #9](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/pull/9) append `SolendDeposit`、加入 schema v2，並釘死既有 v1 hashes |
| `crates/state-store` | 與 PR #9 同次公開修訂後重新凍結 | 只加入無約束 `TEXT` action type 的 parser/round-trip；沒有 DB migration |

[PR #10](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/pull/10)
把 `crates/types` 與 `crates/state-store` 加回 guard。現行
[gate workflow](../.github/workflows/gate.yml) 以 merge-base 比對上述六個
路徑；一般 PR 修改其中任何一個都會令 `Windows validate` 失敗。
PR #9 是這六個路徑至今唯一一次公開、列明範圍的修訂。

PR #9 的相容性論證不是「新 variant 看起來不會影響舊資料」，而是可重算的
append-only 證明：既有 enum declaration 不移位，兩個 schema-v1 fixture
顯式釘在 v1，修訂前後的 canonical bytes/hash 保持不變。完整欄位設計、
遷移契約和鏈上授權邊界見
[Phase 3 SolendDeposit schema amendment](phase3-solend-deposit-schema.md)。

## Runtime cutover：PR #12

[PR #12](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/pull/12)
已讓 `finalize_intent` 正式寫入 schema-v2
`ActionSpec::SolendDeposit`。舊 W5e withdraw carrier 佔位符已從
finalize production path 移除；新的 action 把 target obligation、reserve、
lending market、Solend program id、input mint 與 exact input amount 六欄
全部放進 canonical fingerprint。

finalize 同時 fail closed 地要求：

```text
action.input_amount_raw
  == watch_rule.max_input_amount_raw
  == funding.amount_raw
```

固定 finalize fixture 的新 canonical rule hash 是
`25a8cee58db0e6a722afbeb76da9fbcf073e2de44a03101105b75ff3952ec52c`。
它由 Actions runner 先從程式輸出，再在後續 commit 釘成 golden；不是人工
計算或沿用 core 的 `094818…` fixture。

資料庫沒有 migration 或舊 row 改寫。schema-v1 與 schema-v2 規則以各自的
`schema_version` 自描述，canonical-hash 反查會重讀主 DB 並重算 hash。
不同 `(threshold_bps, amount_raw)` tuple 的 v1/v2 rows 可同庫並存；如果
兩版使用完全相同的 tuple 與 pinned controlled wallet，既有 deterministic
`rule_id` 會相撞，finalize 回 `existing_rule_conflict`，保留舊 row/hash
且不產生 funding/signing instructions。Phase 3 驗收不得靠遷移或覆寫舊
0.1 USDC 標本規避此邊界。

## Phase 3b-1 read-only dry-run boundary

`crates/executor` 現在只包含 SDK/DB/RPC 無關的 candidate/condition
驗證；SQLite、confirmed RPC reads 與 `claw-protocols` builders 的 adapter
留在 `bins/solfrontier-mcp/src/watch.rs`。資料庫以 `mode=ro`、
`create_if_missing(false)` 及 `PRAGMA query_only=ON` 開啟。此 slice
沒有 lease、mutation、wallet-engine dependency、keypair/env key loading、
simulation、簽名、廣播、submission 或 confirmation path。

候選通過 canonical hash、雙時鐘、三方金額、reserve freshness、帳戶/ATA
身份與 full-precision WAD condition 後，只會組出四條指令：
compute limit → compute price → `RefreshReserve` → Solend deposit。報告中的
transaction base64 使用全零 placeholder blockhash 及全 default signature
slots，並標示 `sendable:false` / `recent_blockhash:null`；account meta 的
`is_signer:true` 只是未來簽名需求。這份 bytes 供目視與結構對帳，不能送鏈，
也不能靠就地補 blockhash/signature 升格為已審交易。

## 可公開核驗的主網證據

### 1. 本 repository 的 Phase 2 funding 驗收

[交易 `67PPnfs…JHV`](https://explorer.solana.com/tx/67PPnfsYT6qFvEwiyyTojkqNE2E6UwiYq1qCRHW6g9hpvyVMNEWymjxSSz8Zwrn9eqDzMxvEfVTqUY3xe4NJcJHV)
是 2026-07-29 的 0.1 USDC 真實入金。已記錄的流程是：

```text
typed proposal
  → human fingerprint approval
  → finalize
  → Phantom sign
  → confirm_funding
  → budget_reserved
```

交易的金額、wallet/ATA、Memo、slot、canonical hashes、DB 結果及逐項驗證
內容在 [Phase 2 主網驗收紀錄](phase2-mainnet-acceptance.md)。該次呼叫時
觀測到 `finalized`，但程式門檻仍是 `confirmed`；一次較硬的觀測不會消除
文件中已接受的 rollback 風險。

### 2. 前身的 Solend deposit 外部對帳樣本

[交易 `2jynv…HUVnJ`](https://explorer.solana.com/tx/2jynvLkVoD9TQfFH3nvMacNQZoJRpK9146TAonds1LFeBsjkBvzn2jFHsgFZASCSR5LRJGQqNgcLMQN2zSLHUVnJ)
是前身由 controlled wallet 執行的 finalized Solend deposit：slot
`420,131,308`、`500000` raw USDC。schema 文件列出 deposit instruction
的 program id、data bytes 及 14 個有序 account metas，供本 repo 的
decoder/builders 逐項對帳。

這筆交易是獨立於自造 fixture 的外部參考，能對帳 schema、program id、
data 與 account ordering；但它不是現行 dry-run `ready` 接受路徑的外部
證明，更不是「本 repo 的 Phase 3 executor 已完成真鏈 round-trip」。
不可把前身交易提升為現行執行路徑的驗收。

## 三個 canonical golden hashes

`canonical_rule_hash` 是 `WatchRule` 的 Borsh bytes 做 SHA-256；它是規則
身份，不是 Solana transaction signature。以下常數都釘在
[`crates/types/src/stage2_watch_rule.rs`](../crates/types/src/stage2_watch_rule.rs)：

| Hash | 意義 | 確切測試位置 |
|---|---|---|
| `5fd3a3b8cfeab17e9985826be642acdd44d726b5395bc6752f58076b97329adc` | schema-v1 Solend withdraw fixture；證明 append v2 後舊身份未動 | [`scenario_a_solend_apr_fixture_hash_is_stable`](../crates/types/src/stage2_watch_rule.rs#L765) |
| `ee8a1edb91a6c7b3d3c023adbdfa9df48901ccb71b1445a630a2d1038b48b7bb` | schema-v1 Jupiter fixture；第二個既有身份穩定性證明 | [`scenario_b_basket_buy_fixture_hash_is_stable`](../crates/types/src/stage2_watch_rule.rs#L774) |
| `094818b7d3ccea7b0f234b199a9bb8c8649d66508ae186c710e917074cc4b5aa` | schema-v2 canonical `SolendDeposit` fixture；釘住六個 fingerprint-bound fields 的 layout | [`scenario_c_solend_deposit_fixture_hash_is_stable`](../crates/types/src/stage2_watch_rule.rs#L783) |

同一測試模組的
[`action_borsh_discriminators_are_append_only`](../crates/types/src/stage2_watch_rule.rs#L792)
另釘住 Borsh variant 順序：withdraw `0`、Jupiter `1`、deposit `2`。
golden hashes 能偵測 canonical bytes 漂移；它們不能代替 runtime
account ownership、ATA、mint、amount 或 signer 驗證。

## CI 證據在哪裡看

- Repository 的 [Actions → gate](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/actions/workflows/gate.yml)
  顯示每次 push、PR 和手動 run。
- 每個 PR 的 **Checks** 頁可打開 `Windows validate`，查看 exact commit
  的 frozen-core diff guard、動態 workspace fmt、`cargo check --workspace`
  和 `cargo test --workspace` 結果。
- `Windows release artifact` 只在 push 到 `main` 或手動
  `workflow_dispatch` 時執行；其 job summary 記錄 binary bytes、SHA-256
  與 run id。

合併依據是 exact head commit 的綠燈，不是舊 run 的截圖。CI 證明的是
可重現的 build/test/guard 結果；碰資金、簽名、凍結區、schema 或 executor
的高風險 PR 仍須在綠燈後獨立覆核。

## 已知邊界

### 鏈上 action byte 3 尚未獲 Authorization PDA 支援

`ActionSpec::SolendDeposit` 的 Borsh discriminator 是 `2`；
`WatchRuleActionType::SolendDeposit.to_u8() == 3` 則是另一個鏈下
routing/audit namespace。已部署的 `clawsol-authority` 只接受 action
bytes `1/2`，未知值 fail closed，而且其 Solend CPI builder 仍是
withdraw-only。規劃中的 controlled-wallet direct rail 不經該程式；
若日後改走 Authorization PDA / `ExecuteAction`，必須先更新、重新部署
並審計鏈上程式，再升 schema version。

### 過期 funding 沒有自動退款

驗證正確但晚於 funding deadline 到帳的資金會被記錄為可退款，不會被
呈現為一般成功。現在沒有自動退款 handler，也沒有完整、可稽核的人工
退款程序；資金可能停留在 controlled ATA。管理非測試資金或宣告
production-ready 前，退款路徑必須獨立交付及評審。

### `executing` 沒有 crash recovery / reaper

目前 `crates/executor` 只交付純 dry-run 驗證；tree 仍沒有 execution
lease、signer、broadcast、lease TTL、crash recovery 或 reaper。未來若
交易已廣播但結果不明，不能靠自動重試假定安全；`executing` 狀態必須先
做人工鏈上對帳，直到獨立的 recovery 設計、測試和評審完成。這個邊界
不能由「dry-run 測試全綠」推論為已有崩潰復原能力。

### 唯讀掃描並非完整 reconciliation

Phase 3b-1 每類最多處理 oldest-first 128 列；公開 repository API 沒有
cursor，而且以整批 all-or-nothing 方式做 row decode。一筆損毀資料可讓
該表整個 cycle fail closed，持續 dry-run 亦可能使第 129 列之後長期飢餓。
此外，原始 finalize 缺陷留下的是 `funding_required` orphan，現有
`list_budget_reserved` 無法列舉它；報告中的 `orphan_funding_only` 只涵蓋
已進入 `budget_reserved` 的子集合。完整限制及觸發條件見
`DEBT-P2-FINALIZE-1/2` 與 `DEBT-P3-WATCH-1`。

## 證據索引

- [Repository invariants、凍結區與技術債](../CLAUDE.md)
- [SolendDeposit canonical schema、主網 account 對帳與遷移契約](phase3-solend-deposit-schema.md)
- [Phase 2 主網驗收紀錄](phase2-mainnet-acceptance.md)
- [Phase 3b-1 真資料唯讀 dry-run 與離線交易對帳](phase3b-dry-run-acceptance.md)
- [Phase 3b-2 主網執行驗收紀錄](phase3b-2-mainnet-acceptance.md)（[English](phase3b-2-mainnet-acceptance.en.md)）
- [GitHub Actions gate definition](../.github/workflows/gate.yml)
