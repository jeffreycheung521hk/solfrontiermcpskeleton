# Phase 4 execution acceptance — 2026-08-03

Three complete funding-to-deposit runs on Solana mainnet, and four refunds.
Money went in and money came back, both directions verified from the public
ledger.

Every claim below is re-derivable. Run
`sfcli audit docs/phase4-execution-acceptance.md` to check them.

## The three deposits

Transaction `49tipu9spc9Dd2sRrk1yJWPUTrvHLPViXGV7Rs7uZhgBxmnVpMvB5rfLp95JNpviyFNEugwdsdQv8pmi7nzd6y6S`
was finalized in slot 436961932 and funded intent
`5f000000400d0300000000009a62dace` with 200000 raw. Its Solend deposit,
transaction `2C33BrbKw6vLueGVWRRCf3RHQv7dUgTfZJpC91tMJwU2bYdbr7BaazYzhrRKUh4YEruLARzgwmaATpqZ3VoQLBBs`,
was finalized in slot 436962139.

Transaction `3SKBHGbEgKgdgVA3rkBmaak71km3T1kDT7pVCb5fMKEibAwYcf2tbV5FwegbTJAc7iVw4Uzyg25dGugeK7wGTM5J`
was finalized in slot 436966413 and funded intent
`60000000400d0300000000009a62dace` with 200000 raw. Its Solend deposit,
transaction `4vSvxPbKUvs9JNgFXWy8W8dF5u9wcNpTnWHVbpt9BorDPhfm6pTK5yw12QiVk9PQKgEymYV7bVXeYxuSXS1c4sK3`,
was finalized in slot 436966716.

Transaction `BxiJfCdWTFPtHprsPJx2AjPxn2TvsuPwKLsZiTvWh1uu7QCaonTs36KbjUkEmWetLDURzUL2huBN6BSxwLMJzrU`
was finalized in slot 436968530 and funded intent
`62000000400d0300000000009a62dace` with 200000 raw. Its Solend deposit,
transaction `5GCSA3C2m4KrisAaTZGh75XqhMz16AswE24xuJHu3TjvSokyLNyzht9FUAHafFHQDJnf2CULVdcTovV7MuNGm9Nu`,
was finalized in slot 436968615.

## The third run is the one that counts

The first two deposits reached Solend, but neither was a clean pass. In both,
`flow` failed to detect the payment and a human called `confirm_funding` by
hand inside the window. A pipeline that needs manual rescue at its midpoint has
not been demonstrated; it has been survived.

The third run had **no human step after the wallet approval**:

```
flow      propose -> finalize -> [human approves once in Phantom]
          -> detect -> exact proof -> confirm_funding -> budget_reserved -> exit
executor  lease -> executing -> Solend deposit -> completed
```

85 slots — about 34 seconds — between `confirm_funding` and the deposit landing
finalized. The operator pressed approve once and touched nothing else.

That is the difference between three successes and one acceptance.

## Root cause of the four failures before it

Five funding attempts preceded the successes. Four failed, and every one of them
came back to a single defect that no test could see.

`rpc.mjs` called `getTransaction` without an `encoding` parameter. Solana's
default is `json`, not `jsonParsed`:

```
accountKeys[0] typeof : string      (not {pubkey, signer, writable})
instructions[0] keys  : programIdIndex, accounts, data, stackHeight
any ix has programId? : false
any ix has parsed?    : false
```

Every check in the funding validator reads `programId`, `parsed`, and
`accountKeys[].signer`. Under `json` those fields do not exist, so the first
gate — "did the registered payer sign?" — found no signer object and returned
`payer_did_not_sign` on a payment that was correct in every respect. The
signature was then blacklisted and never fetched again: one millisecond of
misjudgement became 220 seconds of silence over money already on chain.

### The lesson worth more than the bug

**The defect was not inside any layer. It was in the contract between two
layers, and nothing asserted what crosses it.**

Replaying the same live transaction afterwards, every layer is correct: the
listing returns the payment, selection picks it, the validator accepts it with
both deltas exact. Each layer passes its own tests because each layer is right.

Every form of verification skipped exactly that boundary:

| Verification | Why it missed |
|---|---|
| 28 funding-proof tests | hand a `jsonParsed` fixture straight to the validator, never travelling through `rpc.mjs` |
| 8 hermetic end-to-end tests | **the fake RPC ignores request parameters and always answers `jsonParsed`** |
| the captured mainnet fixture | was itself fetched with `jsonParsed` explicitly set |
| the first debugging replay | injected a hand-written fetcher with `jsonParsed`, entering at the wrong layer |

The second row generalises, and it is the finding to carry forward:

> **A test double that returns what the caller *wants* rather than what the
> caller *asked for* silently certifies every malformed request as correct.**

Eight end-to-end tests were green while the request under test was broken.
Nothing in 180 tests asserted the bytes on the wire.

The fix is therefore not the missing parameter. It is
`test/rpc-request-shape.test.mjs`, which records the literal JSON-RPC request
body against a loopback server and asserts it — negative-controlled in both
directions: reverting the encoding fails 3 of its 5 tests, reverting the
listing commitment fails 1.

### Two things that made it undebuggable

**The rejection reason was discarded.** The search loop did
`else seen.add(signature)`, throwing away a precise reason and permanently
blacklisting the signature. Afterwards nothing could say which check had
refused, because nothing had recorded it.

**The error class read like a verdict about the user.**
`payer_did_not_sign` sounds like "the human failed to sign", so it invited no
suspicion. It meant "the response shape has no signer field to look at" —
absence read as a value, the same failure mode as a missing balance row
defaulting to zero.

## The four refunds

Four amounts stranded across four separate failures were all returned, each
through the full gate chain with both balance deltas verified exact at
`finalized` before the ledger was marked.

Transaction `2qEajvMZcq5kw6H5vTfwRMYPLQmxVTk1C5uMKBMwQyhrh94NRoty89t7A1Xr1tfqvHrEwXGM6KHq268oF75jWf7n`
was finalized in slot 436953407 and refunded 200000 raw.

Transaction `sYhNVv7B7qPXmBfQV8TcsW75GNCX2tC89cueUCQczgaE6dpYetsKgoWCiFNScyDhjFz3NZ7K6xveUjbdSfCGeDG`
was finalized in slot 436953540 and refunded 200000 raw.

Transaction `4rXc5YWYyohnohK6AL1DjHBZDTyYr2Qjtoav1Cw34zuiwEZfEvFHCGADdRqW29nTo5gAcUBdJpaZpudMovBfm1ru`
was finalized in slot 436953806 and refunded 100000 raw.

Transaction `rj7vdCnS9DTdyUWR6QxUL63Qw4Smvjgc59J1L68a2Ui3FRsFc5c95gH2YxHtNzbvYgN2sLPkvSJgo3M9RGmXzAp`
was finalized in slot 436960646 and refunded 200000 raw.

## Net position

The controlled ATA ended the day at 753487 raw — exactly where it stood after
the first three refunds, hours and nine intents earlier. Three deposits reached
Solend; four stranded amounts came back; nothing was lost.

**Net loss across five failed attempts and three successes: zero.**

That is only true because the refund rail existed before the failures did. A
funding attempt stopped being a one-way bet on 2026-08-03, and the four
recoveries are what made it reasonable to keep trying after each failure
instead of stopping at the first.

## What is still not closed

- **`DEBT-P2-FUNDING-1` stands.** The refund rail is single-intent. There is no
  cursor pagination, no row-level decode isolation, and no auditable sweep. The
  four refunds happened because a human enumerated four rows.
- **753487 raw remains in the controlled ATA** with no funding row against it,
  left from earlier testing. The rail refunds a recorded amount; it does not
  drain an account.
- **Refund recovery is not automatic.** `recovery_action` is written and tested
  against every crash window, but an existing journal entry aborts the run and
  asks for a human. No resend has ever been exercised.
- **The executor still scans every 30 seconds by default.** The three
  successful runs used a shell loop calling `watch --execute --once` roughly
  every 1.3 seconds, because a 30-second scan against a reserve that is fresh
  for ~6.4 seconds in every ~60 misses most of its chances. That loop is an
  operator workaround, not a product feature.
- **Nothing here is production-ready, restart-safe, or a claim about
  non-test funds.** It is a fixed USDC rail, a pinned controlled wallet, and
  test money.
