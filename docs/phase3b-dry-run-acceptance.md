# Phase 3b-1 read-only dry-run evidence

This record separates two different evidence grades:

1. a real historical SQLite specimen proving read-only fail-closed handling;
2. an offline schema-v2 builder fixture proving the `ready` transaction shape.

Neither observation is a signed Solend execution. Phase 3b-1 has no lease,
keypair, simulation, signing, broadcast, submission, or confirmation path.

## Code and command

The code under test is commit `0b6e1a0` on
`feat/phase3-executor-dryrun`. On 2026-07-31 the local-only test was run with:

```powershell
cargo test -p solfrontier-mcp `
  watch_tests_local_acceptance_database_family_copy_is_read_only `
  -- --ignored --nocapture
```

Result: `1 passed; 0 failed`. The test never opens the original acceptance
database. It first copies the complete `.db`/`-wal`/`-shm` family to a unique
temporary directory, opens only that copy through the production
`ReadOnlyWatchStore`, and asserts `PRAGMA query_only == 1`.

The original and copied families were each compared byte-for-byte before and
after the scan. Both comparisons passed. The source/backup sizes and SHA-256
values are independently recorded in
[the Phase 2 acceptance record](phase2-mainnet-acceptance.md#驗收資料庫離線備份).

## Real historical specimen output

The specimen is the real Phase 2 0.1 USDC intent
`32000000a0860100000000009a62dace`. The test supplies deterministic mock clocks
at their maximum values so both expired boundaries are unambiguous and no
network call is made. These clock values are test inputs, not claimed chain
observations.

```json
{
  "mode": "dry_run",
  "database_access": "sqlite_read_only_query_only",
  "current_confirmed_slot": 18446744073709551615,
  "slot_error_class": null,
  "rows": [
    {
      "status": "unsupported_action",
      "intent_id": "32000000a0860100000000009a62dace",
      "rule_id_hex": "32000000a0860100000000009a62dace",
      "rule_lifecycle": {
        "status": "active",
        "completed": false,
        "revoked": false,
        "execution_nonce": 0
      },
      "candidate": {
        "classification": "unsupported_action",
        "findings": [
          { "code": "unsupported_schema_version" },
          { "code": "unsupported_action" },
          { "code": "wall_clock_expired" },
          { "code": "slot_expired" }
        ],
        "clocks": {
          "now_ms": 9223372036854775807,
          "funding_expires_at_ms": 1785287167381,
          "wall_clock_eligible": false,
          "current_confirmed_slot": 18446744073709551615,
          "rule_expires_at_slot": 435850484,
          "slot_clock_eligible": false
        },
        "amounts": {
          "action_input_amount_raw": null,
          "rule_max_input_amount_raw": "100000",
          "funding_amount_raw": "100000",
          "all_equal_and_nonzero": false
        }
      }
    }
  ],
  "summary": {
    "total": 1,
    "unsupported_action": 1
  },
  "scan_errors": []
}
```

This proves that the watcher can consume the historical WAL-backed database
shape, preserve it, classify the schema-v1 carrier explicitly, retain both
deadline findings, and continue without an RPC account read. It does **not**
prove that a live schema-v2 candidate reaches `ready`.

## Offline `ready` builder cross-check

The active test
`watch_tests_ready_report_is_unsigned_and_matches_mainnet_instruction_proof`
uses the checked-in mainnet USDC reserve snapshot plus controlled-wallet and
obligation fixtures. The reserve was captured at slot `435,907,990`, contains
`last_update_slot = 435,907,953`, and the test evaluates it at slot
`435,907,960` (age 7, within the canonical 16-slot limit).

It compares the assembled output with the predecessor transaction
[`2jynv…HUVnJ`](https://explorer.solana.com/tx/2jynvLkVoD9TQfFH3nvMacNQZoJRpK9146TAonds1LFeBsjkBvzn2jFHsgFZASCSR5LRJGQqNgcLMQN2zSLHUVnJ):

| Index | Instruction | Program | Data hex | Cross-check |
|---:|---|---|---|---|
| 0 | set compute-unit limit (`400000`) | `ComputeBudget111111111111111111111111111111` | `02801a0600` | predecessor W5i constant/encoding |
| 1 | set compute-unit price (`50000`) | `ComputeBudget111111111111111111111111111111` | `0350c3000000000000` | predecessor W5i constant/encoding |
| 2 | `RefreshReserve` | `So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo` | `03` | reserve, Pyth, Switchboard and Clock metas checked in order |
| 3 | deposit liquidity and obligation collateral (`500000` raw) | `So1endDq2YkqhipRh3WViPa8hdiSpxWy6z3Z6tMCpAo` | `0e20a1070000000000` | all 14 pubkey/signer/writable metas checked against the schema table |

The resulting serialized transaction is decoded again and pinned to:

```json
{
  "kind": "unsigned_transaction_with_placeholder_blockhash",
  "sendable": false,
  "fee_payer": "BPfDMmeMBmCbMC1rWh7hwigMBoKGBrKwXxSeUu9hhs5L",
  "recent_blockhash": null,
  "signature_slots": 1,
  "all_signature_slots_default": true,
  "input_amount_raw": "500000"
}
```

The all-zero blockhash and default signature are intentional. This test is a
structure/account-source proof using offline fixtures; it is not a fresh
schema-v2 funded mainnet observation and must not be cited as an executed
round trip.
