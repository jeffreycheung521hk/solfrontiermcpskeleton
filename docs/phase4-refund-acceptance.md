# Phase 4 refund acceptance — 2026-08-03

The first successful money recovery in this project. Three stranded amounts
returned from the controlled ATA to the wallet that funded them, each through
the full gate chain, each with both balance deltas verified exact at
`finalized` commitment before the ledger was touched.

Every claim below is re-derivable from the public ledger. Run
`sfcli audit docs/phase4-refund-acceptance.md` to check them.

## What was stranded, and why

| Intent | Amount | Cause |
|---|---|---|
| `32000000a0860100000000009a62dace` | 100000 raw | 2026-07-29 Phase 2 verified funding only; there was no executor, so the money arrived and nothing ever moved it |
| `5b000000400d0300000000009a62dace` | 200000 raw | 2026-08-01 the executor was started 107 s after the funding deadline |
| `58000000400d0300000000009a62dace` | 200000 raw | 2026-08-02 a client-side memo prefilter discarded the one correct transaction |

The first was not previously known to be stranded. It sat for five days
because Phase 2's acceptance was about funding, and nobody re-read the balance
afterwards.

## The refunds

Transaction `2qEajvMZcq5kw6H5vTfwRMYPLQmxVTk1C5uMKBMwQyhrh94NRoty89t7A1Xr1tfqvHrEwXGM6KHq268oF75jWf7n`
was finalized in slot 436953407 and returned 200000 raw for intent
`5b000000400d0300000000009a62dace`.

Transaction `sYhNVv7B7qPXmBfQV8TcsW75GNCX2tC89cueUCQczgaE6dpYetsKgoWCiFNScyDhjFz3NZ7K6xveUjbdSfCGeDG`
was finalized in slot 436953540 and returned 200000 raw for intent
`58000000400d0300000000009a62dace`.

Transaction `4rXc5YWYyohnohK6AL1DjHBZDTyYr2Qjtoav1Cw34zuiwEZfEvFHCGADdRqW29nTo5gAcUBdJpaZpudMovBfm1ru`
was finalized in slot 436953806 and returned 100000 raw for intent
`32000000a0860100000000009a62dace`.

All three moved USDC from the controlled ATA
`7LFdKcSV7JQYi3or5y9phHVPjhGigu5DDjUakAFbBmk3` to the funder's ATA
`4bDArC4UVoBjecHUZaCKqhuhTYPoiKS4qpQQpxuvm1qn`, owned by
`C4QQjzWxnJ5QFAbkzhQJ3wTzyX6nw1vyFvJwbPXJGPNW`.

The balance of the Controlled USDC ATA went 1253487 → 753487 across the three
refunds — exactly 500000, the sum of the three recorded amounts and not a
lamport more.

## The gate chain, per refund

```
recover → lease (budget_reserved → refunding) → re-read row → re-derive plan
→ build unsigned → simulate → risk policy → Approved typestate
→ sign → JOURNAL BEFORE BROADCAST → broadcast once → finalized
→ verify BOTH deltas exact → mark_refunded_if_refunding
```

Nothing in that sequence is optional, and two properties are worth naming:

- **The operator named an intent and nothing else.** The amount, the source,
  the destination and the destination's owner were all re-derived from the
  funding row. There is no flag that could have sent a different amount or
  sent it elsewhere.
- **`finalized` was required, not `confirmed`.** A rolled-back block would have
  left a row marked `refunded` with the money still in the controlled ATA.

## What the first attempt proved

The first execute run halted with `refund_journal_unavailable` and moved
nothing.

The halt was correct in kind: a missing journal can mean the record of an
already-signed transaction has been deleted, and re-signing there is how a
double refund happens. But the reasoning was incomplete — on a first refund the
journal legitimately does not exist yet, and the two cases are
indistinguishable from the file alone.

The main database resolves them. The lease moves a row to `refunding` *before*
anything is signed, so a row still at `budget_reserved` proves no signature can
exist. That is why the main database stays the source of truth and the journal
holds derived data only.

Recorded here because it is the more useful result: the fail-closed default
caught an error in its own author's reasoning rather than letting it through.
It stopped, it said why, and nothing moved.

## What this does NOT close

- **`DEBT-P2-FUNDING-1` stands.** This is a single-intent rail. There is no
  cursor pagination, no row-level decode isolation, and no auditable sweep that
  could find every refundable row on its own. Three amounts were refunded
  because a human enumerated them.
- **753487 raw remains in the controlled ATA** with no funding row against it,
  left over from earlier testing. The rail refunds the exact recorded amount;
  it does not drain an account, and that residue needs a separate decision.
- **Recovery is not automatic.** `recovery_action` is written and tested
  against every crash window, but an existing journal entry aborts the run and
  asks for a human. Nothing here has yet exercised a resend.
- **The acceptance databases were written.** `STATUS.md` declares them
  non-writable audit artifacts; the owner explicitly authorised the write on
  2026-08-03, on the grounds that an audit record contradicting the chain is
  worse than one that has been appended to. The pre-refund state is in git
  history and in the Phase 2 and Phase 3b-2 acceptance documents.

## A line-ending hazard worth recording

Two of the three databases initially failed to open with
`migration 1 was previously applied but has been modified`.

Nothing had been modified. `core.autocrlf = true` means the migration `.sql`
files hash differently depending on how they were checked out, and sqlx records
the checksum at creation time:

```
LF   → 011FEB4C94C384CBB5D30DFA   (phase2-mainnet-20260729-04.db)
CRLF → CA4A709DED0835CEFF8FDCE2   (phase3-mainnet-20260801-01.db, demo-20260802-run.db)
```

`0001_initial.sql` has exactly one commit in its entire history. The refunds
were run in two batches with the working tree converted accordingly. A
`.gitattributes` rule pinning `*.sql` to LF would prevent this from recurring
and has not yet been added.
