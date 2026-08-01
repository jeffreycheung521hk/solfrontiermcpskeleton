# SolFrontier trust overview (English)

> English translation of [TRUST.md](TRUST.md). The Traditional Chinese
> original is authoritative; if the two versions ever disagree, the Chinese
> text governs.
>
> **Status note (2026-08-01).** TRUST.md was written before PR #17 merged.
> Statements below such as "the tree still has no execution lease, signer,
> broadcast" describe the Phase 3b-1 state and are retained for fidelity.
> PR #17 has since added the operator-started, non-MCP `watch --execute`
> rail, and a first mainnet execution acceptance completed on 2026-08-01 —
> see the [Phase 3b-2 acceptance record](phase3b-2-mainnet-acceptance.en.md)
> for the current state, including the failed attempt it preserves and the
> debts it explicitly does not close.

This document gives an external reviewer an evidence path that takes roughly
ten minutes to walk. It collects only facts the repository can publicly
recompute today. It does not describe the Phase 3b-1 read-only dry-run, test
fixtures, or any single successful observation as a security guarantee for a
delivered, signing-capable executor.

## The ten-minute review path

1. Start with the frozen-zone table below: confirm which trust anchors have
   never been revised and which have been revised exactly once, in public.
2. Open the two transactions in Solana Explorer, keeping "this repository's
   Phase 2 funding acceptance" separate from "the predecessor's Solend deposit
   external reconciliation sample".
3. Recompute the three golden hashes in
   `crates/types/src/stage2_watch_rule.rs` and confirm the two schema-v1
   identities did not change under the append-only v2 revision.
4. Inspect the read-only/query-only connection, the four public repository
   list/get calls, and `Transaction::new_unsigned` in
   `bins/solfrontier-mcp/src/watch.rs`, then re-run the dangerous-symbol grep
   from the PR description.
5. Check the `Windows validate` green run on GitHub Actions or the PR's
   Checks page.
6. Finally, read the known boundaries. A green CI run, golden tests, and
   mainnet samples do not erase those boundaries.

## Current state of the frozen zone

| Path | Status | Verifiable basis |
|---|---|---|
| `crates/observability` | Imported untouched, never revised | six-path frozen-core guard |
| `crates/solana-core` | Imported untouched, never revised | six-path frozen-core guard |
| `crates/wallet-engine` | Imported untouched, never revised | six-path frozen-core guard |
| `crates/risk-engine` | Imported untouched, never revised | six-path frozen-core guard |
| `crates/types` | Re-frozen after one public append-only revision | [PR #9](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/pull/9) appends `SolendDeposit`, adds schema v2, and pins the pre-existing v1 hashes |
| `crates/state-store` | Re-frozen after the same public revision as PR #9 | only adds an unconstrained `TEXT` action-type parser/round-trip; no DB migration |

[PR #10](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/pull/10)
put `crates/types` and `crates/state-store` back under the guard. The current
[gate workflow](../.github/workflows/gate.yml) diffs those six paths against
the merge base; any ordinary PR touching any of them fails
`Windows validate`. PR #9 is, to date, the only public, explicitly scoped
revision of these six paths.

PR #9's compatibility argument is not "the new variant looks like it should
not affect old data" — it is a recomputable append-only proof: the existing
enum declarations do not shift position, two schema-v1 fixtures are explicitly
pinned at v1, and the canonical bytes/hashes are identical before and after
the revision. Full field design, migration contract, and on-chain
authorization boundary are in the
[Phase 3 SolendDeposit schema amendment](phase3-solend-deposit-schema.md).

## Runtime cutover: PR #12

[PR #12](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/pull/12)
made `finalize_intent` write schema-v2 `ActionSpec::SolendDeposit` for real.
The legacy W5e withdraw carrier placeholder is removed from the finalize
production path; the new action puts all six fields — target obligation,
reserve, lending market, Solend program id, input mint, and exact input
amount — into the canonical fingerprint.

Finalize also requires, failing closed:

```text
action.input_amount_raw
  == watch_rule.max_input_amount_raw
  == funding.amount_raw
```

The new canonical rule hash of the fixed finalize fixture is
`25a8cee58db0e6a722afbeb76da9fbcf073e2de44a03101105b75ff3952ec52c`. It was
first emitted by the program on an Actions runner and then pinned as golden in
a follow-up commit; it is not hand-computed and does not reuse the core
`094818…` fixture.

There is no database migration and no rewriting of old rows. Schema-v1 and
schema-v2 rules are self-describing via their own `schema_version`; canonical
hash reverse lookup re-reads the main DB and recomputes the hash. v1/v2 rows
with different `(threshold_bps, amount_raw)` tuples can coexist in the same
database. If both versions use the exact same tuple with the pinned controlled
wallet, the deterministic `rule_id` collides: finalize returns
`existing_rule_conflict`, preserves the old row/hash, and issues no
funding/signing instructions. Phase 3 acceptance must not dodge this boundary
by migrating or overwriting the old 0.1 USDC specimen.

## Phase 3b-1 read-only dry-run boundary

`crates/executor` contains only SDK/DB/RPC-independent candidate/condition
validation; the adapters for SQLite, confirmed RPC reads, and the
`claw-protocols` builders live in `bins/solfrontier-mcp/src/watch.rs`. The
database is opened with `mode=ro`, `create_if_missing(false)`, and
`PRAGMA query_only=ON`. This slice has no lease, mutation, wallet-engine
dependency, keypair/env key loading, simulation, signing, broadcast,
submission, or confirmation path.

Once a candidate passes canonical hash, dual clocks, three-way amounts,
reserve freshness, account/ATA identity, and the full-precision WAD condition,
only four instructions are assembled: compute limit → compute price →
`RefreshReserve` → Solend deposit. The transaction base64 in the report uses
an all-zero placeholder blockhash and all-default signature slots, and is
labeled `sendable:false` / `recent_blockhash:null`; `is_signer:true` in the
account metas records only a future signing requirement. These bytes exist for
visual and structural reconciliation — they cannot be sent to the chain, and
patching in a blockhash/signature in place does not promote them into a
reviewed transaction.

## Publicly verifiable mainnet evidence

### 1. This repository's Phase 2 funding acceptance

[Transaction `67PPnfs…JHV`](https://explorer.solana.com/tx/67PPnfsYT6qFvEwiyyTojkqNE2E6UwiYq1qCRHW6g9hpvyVMNEWymjxSSz8Zwrn9eqDzMxvEfVTqUY3xe4NJcJHV)
is the real 0.1 USDC funding of 2026-07-29. The recorded flow is:

```text
typed proposal
  → human fingerprint approval
  → finalize
  → Phantom sign
  → confirm_funding
  → budget_reserved
```

The transaction's amount, wallet/ATA, Memo, slot, canonical hashes, DB
results, and item-by-item verification are in the
[Phase 2 mainnet acceptance record](phase2-mainnet-acceptance.md). That call
observed `finalized`, but the program's threshold remains `confirmed`; a
single harder observation does not erase the rollback risk the documentation
has accepted.

### 2. The predecessor's Solend deposit external reconciliation sample

[Transaction `2jynv…HUVnJ`](https://explorer.solana.com/tx/2jynvLkVoD9TQfFH3nvMacNQZoJRpK9146TAonds1LFeBsjkBvzn2jFHsgFZASCSR5LRJGQqNgcLMQN2zSLHUVnJ)
is a finalized Solend deposit executed by the predecessor's controlled wallet:
slot `420,131,308`, `500000` raw USDC. The schema document lists the deposit
instruction's program id, data bytes, and 14 ordered account metas for
item-by-item reconciliation against this repo's decoder/builders.

That transaction is an external reference independent of home-made fixtures:
it can reconcile schema, program id, data, and account ordering. It is not
external proof of the current dry-run `ready` acceptance path, and it is
certainly not "this repo's Phase 3 executor has completed a real on-chain
round-trip". Predecessor transactions must not be promoted into acceptance
evidence for the current execution path.

## The three canonical golden hashes

`canonical_rule_hash` is the SHA-256 of a `WatchRule`'s Borsh bytes; it is a
rule identity, not a Solana transaction signature. All constants are pinned in
[`crates/types/src/stage2_watch_rule.rs`](../crates/types/src/stage2_watch_rule.rs):

| Hash | Meaning | Exact test location |
|---|---|---|
| `5fd3a3b8cfeab17e9985826be642acdd44d726b5395bc6752f58076b97329adc` | schema-v1 Solend withdraw fixture; proves the old identity is untouched after appending v2 | [`scenario_a_solend_apr_fixture_hash_is_stable`](../crates/types/src/stage2_watch_rule.rs#L765) |
| `ee8a1edb91a6c7b3d3c023adbdfa9df48901ccb71b1445a630a2d1038b48b7bb` | schema-v1 Jupiter fixture; second pre-existing identity stability proof | [`scenario_b_basket_buy_fixture_hash_is_stable`](../crates/types/src/stage2_watch_rule.rs#L774) |
| `094818b7d3ccea7b0f234b199a9bb8c8649d66508ae186c710e917074cc4b5aa` | schema-v2 canonical `SolendDeposit` fixture; pins the layout of the six fingerprint-bound fields | [`scenario_c_solend_deposit_fixture_hash_is_stable`](../crates/types/src/stage2_watch_rule.rs#L783) |

In the same test module,
[`action_borsh_discriminators_are_append_only`](../crates/types/src/stage2_watch_rule.rs#L792)
additionally pins the Borsh variant order: withdraw `0`, Jupiter `1`, deposit
`2`. Golden hashes detect canonical-byte drift; they are not a substitute for
runtime verification of account ownership, ATAs, mints, amounts, or signers.

## Where to see the CI evidence

- The repository's [Actions → gate](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/actions/workflows/gate.yml)
  shows every push, PR, and manual run.
- Each PR's **Checks** page opens `Windows validate` for the exact commit:
  frozen-core diff guard, dynamic workspace fmt, `cargo check --workspace`,
  and `cargo test --workspace` results.
- `Windows release artifact` runs only on pushes to `main` or manual
  `workflow_dispatch`; its job summary records binary bytes, SHA-256, and the
  run id.

The merge authority is a green run on the exact head commit, not a screenshot
of an old run. CI proves reproducible build/test/guard results; high-risk PRs
touching funds, signing, the frozen zone, schema, or the executor still
require independent review after the green run.

## Known boundaries

### On-chain action byte 3 has no Authorization PDA support

`ActionSpec::SolendDeposit` has Borsh discriminator `2`;
`WatchRuleActionType::SolendDeposit.to_u8() == 3` is a separate off-chain
routing/audit namespace. The deployed `clawsol-authority` accepts only action
bytes `1/2`, fails closed on unknown values, and its Solend CPI builder
remains withdraw-only. The planned controlled-wallet direct rail bypasses that
program; any future move to Authorization PDA / `ExecuteAction` requires
updating, redeploying, and auditing the on-chain program first, then bumping
the schema version.

### Expired funding has no automatic refund

Funds that verify correctly but arrive after the funding deadline are recorded
as refundable and are not presented as ordinary success. There is currently no
automatic refund handler, and no complete, auditable manual refund procedure;
funds can remain parked in the controlled ATA. Before managing non-test funds
or claiming production readiness, the refund path must be independently
delivered and reviewed.

### `executing` has no crash recovery / reaper

`crates/executor` at this point delivers pure dry-run validation only; the
tree still has no execution lease, signer, broadcast, lease TTL, crash
recovery, or reaper. If a transaction is ever broadcast with an unknown
outcome, automatic retry cannot be assumed safe: an `executing` state requires
manual on-chain reconciliation until an independent recovery design is built,
tested, and reviewed. This boundary cannot be argued away by "all dry-run
tests are green".

### Read-only scanning is not full reconciliation

Phase 3b-1 processes at most 128 oldest-first rows per class; the public
repository APIs have no cursor and decode rows in an all-or-nothing batch. One
corrupt row can fail-close an entire table's cycle, and long-running dry-runs
can starve rows beyond the 128th indefinitely. Separately, the original
finalize defect leaves `funding_required` orphans, which the existing
`list_budget_reserved` cannot enumerate; the reported `orphan_funding_only`
covers only the subset that reached `budget_reserved`. Full limitations and
trigger conditions are in `DEBT-P2-FINALIZE-1/2` and `DEBT-P3-WATCH-1`.

## Evidence index

- [Repository invariants, frozen zone, and technical debt](../CLAUDE.md)
- [SolendDeposit canonical schema, mainnet account reconciliation, migration contract](phase3-solend-deposit-schema.md)
- [Phase 2 mainnet acceptance record](phase2-mainnet-acceptance.md)
- [Phase 3b-1 real-data read-only dry-run and offline transaction reconciliation](phase3b-dry-run-acceptance.md)
- [Phase 3b-2 mainnet execution acceptance record](phase3b-2-mainnet-acceptance.md) ([English](phase3b-2-mainnet-acceptance.en.md))
- [GitHub Actions gate definition](../.github/workflows/gate.yml)
