# Phase 3b-2 Mainnet Execution Acceptance Record

> English translation of [phase3b-2-mainnet-acceptance.md](phase3b-2-mainnet-acceptance.md).
> The Traditional Chinese original is authoritative; if the two versions ever
> disagree, the Chinese text governs.

## Conclusion

**PASS (limited scope).** On 2026-08-01, SolFrontier MCP completed its first
full controlled-wallet execution loop on Solana mainnet:

`propose_intent → finalize_intent → human Phantom-signed funding →
confirm_funding → budget_reserved → watch --execute (CAS lease → simulation →
policy → sign → broadcast → finalized) → completed`

The run used 0.2 USDC of real funds. Every transaction, address, and hash below
is public on-chain data; this document contains no RPC endpoint, API key,
private key, or seed phrase.

**What this acceptance proves is strictly limited to:** a single, fixed mainnet
Solend USDC deposit, from a pinned controlled wallet, with test-scale funds,
via a non-MCP CLI explicitly started by the operator. It does **not** prove
production readiness, restart safety, complete reconciliation, or generic
autonomous custody. The "Open technical debt" section below enumerates exactly
what is still missing.

## The failed attempt earlier the same day (FAIL) — and its unresolved consequence

This record deliberately preserves the failure alongside the success; the
success must not be read without it.

| Item | Value |
|---|---|
| Intent ID | `5b000000400d0300000000009a62dace` (threshold 91 bps) |
| Canonical rule hash | `3d96e1c4418868ad543fa84420d79ffa29d494a3f7e1d93b31cc61a8e1f6b596` |
| Funding transaction | [`5caGiE8SBsPo4TujBrTVR7xFdNmn5XPjdaiEvdxuXLnfnoQJyHo5aFYrx9e8cse2BUFj8yoPEboHmgvdEBMf6ya6`](https://explorer.solana.com/tx/5caGiE8SBsPo4TujBrTVR7xFdNmn5XPjdaiEvdxuXLnfnoQJyHo5aFYrx9e8cse2BUFj8yoPEboHmgvdEBMf6ya6) |
| Database | `data/acceptance/phase3-mainnet-20260801-01.db` |

Timeline: `finalize_intent` at 18:08:14 (+08:00; deadline 18:11:14) → the
Phantom-signed funding transaction landed on chain at 18:09:01 (**on time**) →
`confirm_funding` completed at 18:10:32, leaving only 42 seconds of budget →
the executor was not started until 18:13:00. At startup, `now_ms`
1785579181348 had already exceeded `expires_at_ms` 1785579074221 by
**107.127 seconds**, and `current_confirmed_slot` 436542150 had exceeded
`expires_at_slot` 436541956 by **194 slots**.

With both clocks expired, the gate refused to acquire the execution lease.
**Nothing was signed, nothing was broadcast, no state was changed.** This is
correct fail-closed behavior, not a malfunction; the wrapper loop stopped
correctly because `wall_clock_expired` is not in its safe-retry set.

**Unresolved consequence:** that attempt's 0.2 USDC had already entered the
controlled ATA and remains parked at `budget_reserved`. The atomic WHERE clause
of `lease_execution_if_budget_reserved` requires `expires_at_ms >
lease_now_ms`, so this row can never execute again; there is currently no
automatic refund handler (`DEBT-P2-FUNDING-1`). These funds require manual
on-chain reconciliation and a manual refund. They must **not** be treated as
cleaned up merely because the later run succeeded — the two runs are distinct
intents funded with distinct money.

## Publicly verifiable evidence (the successful run)

| Item | Value |
|---|---|
| Deposit transaction | [`hpVwPcMwy6RWATk2ks7HPEo8bS4zGAPaL7gx4BcdpBqgs4d27NrD6YF73T8nk9gokiatT7iGx2YdrNupzcdx6J4`](https://explorer.solana.com/tx/hpVwPcMwy6RWATk2ks7HPEo8bS4zGAPaL7gx4BcdpBqgs4d27NrD6YF73T8nk9gokiatT7iGx2YdrNupzcdx6J4) |
| Deposit on-chain time | 2026-08-01 10:57:37 UTC / 18:57:37 +08:00 |
| Deposit slot | `436548513`, `confirmationStatus: finalized`, `err: null` |
| Deposit fee | 25,000 lamports |
| Funding transaction | [`2wsxko7HiZzBpfoewzeyD4JD52RbzW6k1qFz2buvLaR1fJR8SXb7jZAQsBWeioTArQx65xxd4mNRLMwLnwRrnJt4`](https://explorer.solana.com/tx/2wsxko7HiZzBpfoewzeyD4JD52RbzW6k1qFz2buvLaR1fJR8SXb7jZAQsBWeioTArQx65xxd4mNRLMwLnwRrnJt4) |
| Funding on-chain time | 2026-08-01 10:56:55 UTC / 18:56:55 +08:00, slot `436548415` |
| Amount | 0.2 USDC / `200000` raw units |
| USDC mint | `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v` |
| Funding wallet | `C4QQjzWxnJ5QFAbkzhQJ3wTzyX6nw1vyFvJwbPXJGPNW` |
| Source USDC ATA | `4bDArC4UVoBjecHUZaCKqhuhTYPoiKS4qpQQpxuvm1qn` |
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
| Reserve liquidity supply (recipient) | `8SheGtsopRUDzdiD6v6BR9a6bqZ9QwywYQY99Fp5meNf` |
| Collateral mint | `993dVFL2uXWYeoXuEBFXR4BijeXdTv4s6BzsCjJZuwqk` |
| Target obligation | `BdFLjCcP9mCy557vNNGVbTUuvHxXsh8hc6jXzaPra1wN` |
| Final DB state | `funding=completed`, `watch_rule=completed`, `is_terminal: true` |

Funding transaction Memo, verbatim:

```text
claw:w5h:5a000000400d0300000000009a62dace:a28eaf630890d0830ca822c754766689eb75549ed559025d404cb46dec08f1d5
```

The canonical draft hash is a pure function of the typed parameters. The value
in the table was obtained after acceptance by replaying `propose_intent` with
exactly the same parameters (`database_writes: 0`, `network_calls: 0`,
`signatures: 0`, `db_row_exists: false`); `finalize_intent` had already
recomputed and checked the same value at run time, so the two are necessarily
equal. `draft_id` is a random, non-canonical correlation value that enters no
fingerprint, so it is not recorded here.

## On-chain balance changes (verified independently of program output)

| Account | Pre | Post | Delta |
|---|---:|---:|---:|
| Controlled USDC ATA `7LFdKcSV…` | 1,253,487 | 1,053,487 | −200,000 |
| Obligation `BdFLjCcP…` collateral | 2,121,745 | 2,275,267 | +153,522 cToken |

The deposit uses Solend's `Deposit Reserve Liquidity and Obligation
Collateral` instruction: collateral is credited directly to the obligation and
leaves no cToken ATA balance on the controlled wallet. The observed controlled
cToken balance was `0` both before and after, which matches the instruction's
semantics — nothing was lost. +153,522 cToken against 200,000 raw USDC implies
an exchange rate of about 1.3028, consistent with a mature USDC reserve.

## Full timeline (+08:00)

| Time | Event |
|---|---|
| 18:51:12 | Operator starts the `watch --execute` loop in their own PowerShell (before finalize) |
| 18:51:12–18:55:56 | Loop idles safely on the empty DB with `"rows": []`, `scan_errors: []` |
| 18:55:56.714 | `finalize_intent` writes the WatchRule/funding row; the 180-second absolute deadline starts |
| 18:56:55 | Human Phantom-signed funding transaction lands on chain |
| 18:57:10 | Watcher detects the new signature (a baseline of 23 pre-existing signatures excludes old transactions) |
| 18:57:11 | `confirm_funding` → `budget_reserved`, 106 seconds remaining |
| 18:57:11–18:57:36 | Loop hits `reserve_stale` 12 consecutive times, failing closed each time without taking a lease |
| 18:57:36.9 | Fresh reserve window caught: CAS lease → rebuild → simulation |
| 18:57:37.3 | risk-engine `verdict=approved` → sign → broadcast |
| 18:57:37 | Deposit transaction on chain, slot `436548513` |
| 18:57:52.5 | `finalized` observed; `completed` written; loop ends itself with `STOP completed` |
| (18:58:56.714) | Nominal funding-row deadline; this run's deposit landed 79 seconds ahead of it |

73 attempts in total, 12 of them `reserve_stale`. `abort_remaining_cycle` was
`false`; no `*_scan_failed` / `*_scan_truncated` occurred.

## The gate chain actually verified

1. `propose_intent` is pure computation: no DB row, no network, no signature.
2. `finalize_intent` recomputes and checks the draft hash before creating the
   WatchRule, sidecar mapping, and funding intent in the legacy
   non-transactional order; this step performs zero signing, zero transaction
   construction, zero broadcast.
3. The signing page loads only locally vendored JS over loopback; the wallet
   Phantom connects must exactly equal the `user_wallet` registered at
   finalize. Instruction order is Memo at index 0, TransferChecked at index 1.
   The MCP binary serves stdio only throughout and opens no HTTP port.
4. `confirm_funding` independently re-verifies on chain: the exact Memo text,
   signer identity, source ATA decreased by exactly `200000`, controlled ATA
   increased by exactly `200000`, and mint/owner/decimals all match — only
   after all checks pass does it run the two CAS transitions. This run had
   `arrived_after_funding_deadline: false` and `late_funding: null`; the
   late-funding path was not triggered.
5. `watch --execute` must complete a full-batch scan/precheck every round;
   this run raised no scan flag at any point.
6. Condition recomputation: `threshold_wad` `9000000000000000` (90 bps);
   measured `observed_apr_wad` `18826858686431800`, `observed_apr_floor_bps`
   `188`, `met: true`. Reserve freshness must be ≤ 16 slots; anything older is
   classified `reserve_stale` and skipped.
7. Exact three-way amount equality: `action_input_amount_raw` =
   `rule_max_input_amount_raw` = `funding_amount_raw` = `200000`.
8. Only after the CAS `budget_reserved → executing` succeeds is the
   transaction rebuilt, then passed through wallet-engine simulation
   (`simulated_cu=37181`, `derived_cu_limit=40899`,
   `applied_cu_limit=200000`) and the real risk-engine policy (`rules=6`,
   `programs_allowed=2`, `verdict=approved`, proposal
   `c7ad2c9f-4eae-44b7-baf2-eaf9ac84fe0a`).
9. The transaction contains the `RefreshReserve` and deposit Solend
   instructions; both reserve and deposit are fingerprint-bound; the
   controlled wallet is the sole signer.
10. Success is accepted only at `finalized`; after confirmation the
    `WatchRule → completed` write runs first, then the funding row
    `executing → completed`. Both steps succeeded in this run; the split
    `rule=completed / funding=executing` state did not occur.

## Why the executor must start before finalize (reproducible operational finding)

The root cause of the failed run was purely ordering: every manual step was
placed before the executor, exhausting the 180-second window. The successful
run moved `watch --execute` to **before** `finalize_intent`. This is safe
because of `bins/solfrontier-mcp/src/watch.rs:610` — a pending rule whose
funding row is not yet `budget_reserved` emits no row at all, so the scan
returns `"rows": []`, which is a safe retry, and the loop can idle
indefinitely.

Measurement also showed the true bottleneck is not the 180-second window but
the **Solend reserve refresh cadence**. Sampled continuously for 151 seconds
on 2026-08-01 (every 2 seconds, 71 samples):

| Metric | Value |
|---|---|
| Observed reserve refreshes | 3 (roughly once per 60 s) |
| Samples within the ≤ 16-slot bound | 13% (9 of 71) |
| Reserve age median / max | 62 / 146 slots |
| Longest continuous ineligible stretch | 52 s |

The eligible window is about 6.4 s and the loop period about 5.3 s, so each
window is normally caught once; this run waited about 25 s through 12
`reserve_stale` refusals before succeeding. It means every acceptance run must
budget up to ~60 s for reserve freshness. This is a property of the runtime
environment, not a program defect; `reserve_stale` is itself correct
fail-closed behavior.

Also note: `intent_id` is determined by the typed parameters (bytes 0–3 are
`threshold_bps` LE, bytes 4–11 are `amount_raw` LE), so identical parameters
reproduce the identical on-chain Memo. This run deliberately used 90 bps
instead of 91 bps so the new intent's Memo differs from the failed attempt's
Memo already on chain; `confirm_funding` additionally excluded old
transactions using a baseline of pre-existing signatures. Any future rerun of
identical parameters must keep at least one of these protections, or an old
funding transaction already on chain could be replayed to satisfy a new
intent.

## Deliberate deviations and the authorization boundary

This execution did **not** call `clawsol-authority ExecuteAction`, did **not**
read any `AuthorizationRecord` PDA, and did **not** put an on-chain authority
into the Solend CPI loop. After all gates passed, the pinned controlled-wallet
keypair signed and called Solend directly. This must never be dressed up as
deployed user-delegated PDA execution — the deployed authority program only
accepts action bytes 1/2, its Solend CPI builder is withdraw-only, and the
canonical `SolendDeposit=3` must not be claimed as a deployed PDA grant
(`DEBT-P3-SCHEMA-1`).

Relative to the predecessor's W5i path, the deviation direction is
**tightening, not loosening**: the predecessor's `stage2_chat_execute.rs:53-59`
explicitly signed deposits with the controlled wallet without any review
pipeline; this run instead pushed the same fresh unsigned transaction through
wallet-engine simulation, the real risk-engine policy, and an explicitly
auto-approved `Approved` typestate before signing.

Human-authorization boundary: the user signs funding only, by hand, in
Phantom; the primary wallet's private key never enters the daemon. The
execution side's authorization is the intersection of: an operator explicitly
starting a non-MCP CLI, a fixed controlled wallet, exact canonical
identity/three-way amounts, fresh account/condition/dual-clock checks,
simulation/policy, and a single CAS. An MCP host or LLM cannot escalate an
ordinary tool call into `--execute`. During this run,
`SOLFRONTIER_CONTROLLED_WALLET_KEYPAIR` existed only in the operator's own
PowerShell and never entered any MCP configuration.

## Open technical debt

This acceptance closed **none** of the following, and must not be cited as
having closed any of them:

- `DEBT-P2-FUNDING-1`: no automatic refund for expired funding; the 0.2 USDC
  above is a live instance. The two bounded list APIs still lack cursor/keyset
  pagination and row-level error isolation.
- `DEBT-P3-EXECUTION-1`: `executing` has no crash recovery / reaper / lease
  TTL. Both completion writes happening to succeed in this run does not prove
  interrupted scenarios are handled.
- `DEBT-P3-EXECUTION-2`: final revalidation and the funding CAS are not a
  single snapshot; the race window between WatchRule revocation and
  `mark_completed` still exists.
- `DEBT-P3-KEY-1`: zeroization of key material inside the frozen
  `SecretKeystore` remains unproven.
- `DEBT-P3-SCHEMA-1` / `-2`: PDA authorization and decimals-in-fingerprint
  remain unaddressed.
- `DEBT-P3-FINALIZE-1`: still full-amount single-shot only; no partial fills.
- `DEBT-P2-FINALIZE-1` / `-2`: fault-injection evidence for rule-only /
  funding-only orphans is still missing.

Per `CLAUDE.md`, before managing any non-test funds or claiming production
readiness (whichever comes first), each of the above must be delivered and
independently reviewed.

## Acceptance environment and databases

- Branch: `feat/phase3-executor-execute`, HEAD `4f4ed53`
  (`docs: link executor safety record to PR`), clean worktree.
- Release binary: 5,639,680 bytes, built 2026-08-01 15:18:47 +08:00.
- Executor log: `data/logs/phase3-mainnet-watch-20260801-185112.stderr.log`
  (success). The same day also has `-181259` (failure) and `-160737`
  (empty-DB idle).
- Acceptance DB family (1,196,360 bytes total):

| File | Bytes | SHA-256 |
|---|---:|---|
| `phase3-mainnet-20260801-02.db` | 376,832 | `971DB38C0A10DF81F567E9E95A419E90D78A06B79893E179E633D525AD1237AC` |
| `phase3-mainnet-20260801-02.db-wal` | 753,992 | `7A9F818088F47A4ECE68C6A575C6FA94FD16730244FB2DA3B02860E0817A94C5` |
| `phase3-mainnet-20260801-02.db-shm` | 32,768 | `47B3AF200447C460B32E7CF6106280F45B4CDC5868BA4F4CD268163292BEA22B` |
| `phase3-mainnet-20260801-02.db.mcp-intent-index.sqlite3` | 32,768 | `EBE7B3BE4B7118F17D41A3A66A9DF4584BFE2646DCB6E640FB46FB66828A3E0F` |

The hashes above were taken after acceptance completed, after `STOP
completed`. Both acceptance DB families — `-01` (the failed attempt) and `-02`
— are audit source material: never delete them and never use them as test
write targets. Tests must copy the complete `.db`/`-wal`/`-shm` family to a
temporary directory before opening.

Note for future runs: when creating a new acceptance DB, a zero-byte file is
not sufficient — the dry-run opens read-only/query-only, cannot create tables,
and will return `funding_scan_failed` and `watch_rule_scan_failed`. Open the
DB once through the MCP server first so migrations apply, confirm the dry-run
returns `"rows": []` with `"scan_errors": []`, and only then start the execute
loop; otherwise the scan flags will abort every round.
