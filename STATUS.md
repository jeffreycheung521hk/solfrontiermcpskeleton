# Project status: concluded

**As of 2026-08-01, active development of this repository has concluded.**
No further features, maintenance, or security updates are planned. This file
is the project's closing entry, written under the same register discipline the
project used while alive: state exactly what was delivered, and keep every
open debt named and open.

## What was delivered

| Phase | State | Evidence |
|---|---|---|
| Phase 1 — read-only tools (`get_quote`, `get_position`, `get_intent_status`) | Delivered | live Jupiter / Solana RPC / state-store backends over stdio MCP |
| Phase 2 — `propose_intent`, `finalize_intent`, `confirm_funding` + loopback Phantom signing page | Delivered, mainnet-accepted 2026-07-29 (0.1 USDC) | [acceptance record](docs/phase2-mainnet-acceptance.md) · [tx `67PPnfs…`](https://explorer.solana.com/tx/67PPnfsYT6qFvEwiyyTojkqNE2E6UwiYq1qCRHW6g9hpvyVMNEWymjxSSz8Zwrn9eqDzMxvEfVTqUY3xe4NJcJHV) |
| Phase 3b-1 — read-only dry-run watcher | Delivered | [dry-run acceptance](docs/phase3b-dry-run-acceptance.md) |
| Phase 3b-2 — operator-started `watch --execute` rail (PR #17) | Delivered, mainnet-accepted 2026-08-01 (0.2 USDC) | [acceptance record](docs/phase3b-2-mainnet-acceptance.en.md) · [tx `hpVwPcMw…`](https://explorer.solana.com/tx/hpVwPcMwy6RWATk2ks7HPEo8bS4zGAPaL7gx4BcdpBqgs4d27NrD6YF73T8nk9gokiatT7iGx2YdrNupzcdx6J4) |
| Phase 4 — size/dependency reduction | **Not started** | — |

The Phase 3b-2 acceptance also preserves a same-day failed run that the gates
correctly refused (dual-clock expiry; no lease, no signature, no broadcast),
including the 0.2 USDC that failure left stranded in the controlled ATA. The
owner has since elected to leave those funds where they are, superseding the
acceptance record's pending-manual-refund expectation for those specific
funds. That write-off applies to those funds only — it does not close
`DEBT-P2-FUNDING-1`.

## Every debt remains open

Development stopping means the debt register's trigger conditions (e.g.
"before managing any non-test funds or claiming production-ready"; some
debts carry additional triggers) will never fire under this stewardship. The debts are therefore **permanently open
here**. Anyone forking this code must treat them as live obligations, not
historical notes:

| Debt | One-line summary |
|---|---|
| `DEBT-P2-FUNDING-1` | No refund handler for expired funding; list APIs lack cursor pagination and row-level error isolation |
| `DEBT-P3-EXECUTION-1` | `executing` has no crash recovery, reaper, or lease TTL; two-step completion writes are not atomic |
| `DEBT-P3-EXECUTION-2` | Final revalidation and the funding CAS are not one snapshot; revoke/complete race window exists |
| `DEBT-P3-KEY-1` | Key-material zeroization inside the frozen `SecretKeystore` is unproven |
| `DEBT-P3-SCHEMA-1` | The deployed on-chain authority does not support `SolendDeposit`; direct signing is not a PDA grant |
| `DEBT-P3-SCHEMA-2` | Deposit decimals are runtime-derived, not part of the canonical fingerprint |
| `DEBT-P3-WATCH-1` | Execute mode aborts the whole round on any scan flag; pagination / row-isolation obligations merged into `DEBT-P2-FUNDING-1` |
| `DEBT-P3-FINALIZE-1` | Full-amount single-shot execution only; no partial fills |
| `DEBT-P2-FINALIZE-1/2` | Rule-only / funding-only orphan creation paths lack fault-injection test evidence |

Full statements, mitigations, and trigger conditions: [CLAUDE.md](CLAUDE.md).

## Standing warnings for anyone running this code

- **Test funds only.** The executed rail is a custodial hot-wallet path with
  no refund mechanism. Nothing here is production-ready, and the repository
  itself says so in every acceptance document.
- The system invariants (INV-1 through INV-8 in [CLAUDE.md](CLAUDE.md))
  are load-bearing. In particular: humans sign, AI proposes; simulation
  before signature; fail closed; stdio only, no HTTP.
- The acceptance databases under `data/acceptance/` and the two predecessor
  repositories are audit source material — never write targets. Note that
  `data/` is gitignored: the DB families and stderr logs whose SHA-256 values
  the acceptance docs pin are operator-local artifacts, not committed files.
- Operational hard-won findings (executor must start before finalize; Solend
  reserve freshness is the real bottleneck at ~13% eligibility) are recorded
  in the [Phase 3b-2 acceptance](docs/phase3b-2-mainnet-acceptance.en.md).

## Reading order for reviewers

1. [README](README.md) — what this artifact proves and deliberately does not
2. [TRUST.en.md](docs/TRUST.en.md) — the ten-minute external review path
3. [Phase 3b-2 acceptance (EN)](docs/phase3b-2-mainnet-acceptance.en.md) — the
   success, the failure, and the measurements
4. [CLAUDE.md](CLAUDE.md) — invariants and the full debt register (Chinese)

Chinese originals are authoritative where a translation exists.
