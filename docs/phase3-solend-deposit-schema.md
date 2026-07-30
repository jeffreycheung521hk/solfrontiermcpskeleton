# Phase 3 SolendDeposit schema amendment

PR #9 is a deliberately narrow amendment to the previously frozen canonical
watch-rule boundary. It appends one action; it does not make `finalize_intent`
write that action and it does not add an executor.

The predecessor had already named the defect in
`crates/gateway/src/stage2_chat_execute.rs:61-64`: the W5i deposit was “Not a
first-class production SolendDeposit ActionSpec”; it happened only because the
orchestrator hard-coded the W5g amount and target obligation. That path imports
five demo constants from `stage2_executor` at line 112, but never enters
`validate_request_shape` or its authorization execution flow. Its safety came
from supporting exactly one compile-time amount and target, not from binding
those values into an authorization fingerprint. This amendment supplies that
missing canonical identity.

## Canonical fields

Every `SolendDeposit` field enters the Borsh preimage and therefore the
`canonical_rule_hash`.

| Field | Mainnet evidence | Execution requirement |
|---|---|---|
| `solend_program_id` | Deposit instruction program id | Must equal the audited Solend program id. |
| `reserve_pubkey` | account meta 3 (`meta[2]`) | Decode the exact account and require it is owned by `solend_program_id`. |
| `lending_market` | account meta 6 (`meta[5]`) | Must equal the market decoded from the reserve. |
| `target_obligation` | account meta 9 (`meta[8]`) | Must be the intended obligation, owned by Solend, for the controlled wallet and the canonical market. |
| `input_amount_raw` | bytes 1..9 of instruction data, little-endian | Must equal `WatchRule.max_input_amount_raw` and the persisted funding amount. |
| `input_mint` | Indirect evidence only; it is not one of the 14 account metas | Must equal the reserve's decoded liquidity mint and the mint of the controlled wallet's derived source ATA; pre/post token balances must agree. |

`input_mint` has a distinct evidence grade from the four pubkeys that appear
directly in the instruction and from the amount encoded directly in its data.
The executor must derive and verify it through reserve decoding, ATA derivation,
and token-account state. It must not claim a direct account-meta match.

The following invariant is mandatory before signing:

```text
action.input_amount_raw
  == watch_rule.max_input_amount_raw
  == funding.amount_raw
```

ATA, mint, owner, reserve, market, obligation, and program mismatches all fail
closed. Token decimals are intentionally derived rather than fingerprinted; the
current USDC-only rail must require `decimals == 6`. The trigger for reviewing
that decision is recorded in `DEBT-P3-SCHEMA-2`.

## External transaction cross-check

The external reference is the predecessor's finalized mainnet deposit:

- signature:
  `2jynvLkVoD9TQfFH3nvMacNQZoJRpK9146TAonds1LFeBsjkBvzn2jFHsgFZASCSR5LRJGQqNgcLMQN2zSLHUVnJ`
- slot: `420,131,308`
- amount: `500000` raw USDC
- deposit instruction data: `0e20a1070000000000`
  (`0e` followed by `500000u64` little-endian)

Its third top-level instruction has the following 14 ordered account metas.
`W` is writable and `S` is signer.

| # | Pubkey | W | S | Role |
|---:|---|:---:|:---:|---|
| 1 | `7LFdKcSV7JQYi3or5y9phHVPjhGigu5DDjUakAFbBmk3` | yes | no | source USDC ATA |
| 2 | `BQazv4UQNFV8t4QGVntELr1ee3bCeTN8AvdPGRutFKn7` | yes | no | controlled cToken ATA |
| 3 | `BgxfHJDzm44T7XG68MYKx7YisTjZu73tVovyZSjJMpmw` | yes | no | reserve |
| 4 | `8SheGtsopRUDzdiD6v6BR9a6bqZ9QwywYQY99Fp5meNf` | yes | no | reserve liquidity supply |
| 5 | `993dVFL2uXWYeoXuEBFXR4BijeXdTv4s6BzsCjJZuwqk` | yes | no | reserve collateral mint |
| 6 | `4UpD2fh7xH3VP9QQaXtsS1YY3bxzWhtfpks7FatyKvdY` | no | no | lending market |
| 7 | `DdZR6zRFiUt4S5mg7AV1uKB2z1f1WzcNYCaTEEWPAuby` | no | no | market authority PDA |
| 8 | `UtRy8gcEu9fCkDuUrU8EmC7Uc6FZy5NCwttzG7i6nkw` | yes | no | reserve collateral supply |
| 9 | `BdFLjCcP9mCy557vNNGVbTUuvHxXsh8hc6jXzaPra1wN` | yes | no | target obligation |
| 10 | `BPfDMmeMBmCbMC1rWh7hwigMBoKGBrKwXxSeUu9hhs5L` | yes | yes | obligation owner |
| 11 | `Dpw1EAVrSB1ibxiDQyTAW6Zip3J4Btk2x4SgApQCeFbX` | no | no | Pyth oracle |
| 12 | `nu11111111111111111111111111111111111111111` | no | no | Switchboard oracle |
| 13 | `BPfDMmeMBmCbMC1rWh7hwigMBoKGBrKwXxSeUu9hhs5L` | no | yes | transfer authority |
| 14 | `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA` | no | no | SPL Token program |

The liquidity supply, collateral mint/supply, oracle accounts, market
authority, both controlled-wallet ATAs, and token program are derived and then
checked against decoded or pinned protocol state. They are not duplicated in
the canonical action.

## Append-only and legacy-hash proof

Borsh uses the zero-based declaration order for enum variants. Before this PR,
`SolendWithdrawAllDelegated` was `0` and `JupiterBuySolWithUsdc` was `1`.
Appending `SolendDeposit` makes it `2`; no existing declaration moves.

Commit 1 first detached the two legacy golden fixtures from the mutable
“latest schema” alias and pinned them to schema v1. Commit 2 then adds v2.
Tests pin all three action tags and these hashes:

- v1 Solend withdraw fixture:
  `5fd3a3b8cfeab17e9985826be642acdd44d726b5395bc6752f58076b97329adc`
- v1 Jupiter fixture:
  `ee8a1edb91a6c7b3d3c023adbdfa9df48901ccb71b1445a630a2d1038b48b7bb`
- v2 Solend deposit fixture:
  `094818b7d3ccea7b0f234b199a9bb8c8649d66508ae186c710e917074cc4b5aa`

The state-store `action_type` column is unconstrained `TEXT NOT NULL`
(`0011_stage2_watch_rules.sql`), so adding the `solend_deposit` parser and
repository round-trip requires no database migration.

## Migration contract

Existing schema-v1 rows are neither rewritten nor rehashed. Readers continue
to deserialize v1 withdraw and Jupiter actions, and their first canonical byte
remains `1`. New canonical Solend deposits must use schema v2 (first byte `2`);
other action variants may remain v1 when reproducing an existing identity.

PR #9 deliberately pins the current finalize placeholder to v1, so merging the
schema amendment alone does not alter any newly persisted placeholder hash.
PR-C is the separately reviewed cutover that removes the placeholder and makes
new deposit drafts persist a v2 `SolendDeposit`. Expired historical intents,
including the Phase 2 acceptance specimen, are not migrated or rewritten.

## Authorization boundary

`WatchRuleActionType::SolendDeposit.to_u8() == 3` is currently an off-chain
routing and audit value only. The deployed `clawsol-authority` program accepts
only bytes 1 and 2, rejects unknown bytes, and its Solend CPI builder is
withdraw-only. The Phase 3 controlled-wallet direct execution rail does not
invoke that program or an Authorization PDA. Any future deposit through the PDA
rail is blocked on the program/schema work recorded in
`DEBT-P3-SCHEMA-1`.
