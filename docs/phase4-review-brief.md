# Review brief: the Phase 4 refund rail

*For an experienced Rust engineer, ~20 minutes of pre-reading. Everything
links to pinned commits on GitHub; nothing needs a local checkout.*

## Ask

Review roughly 1,500 lines of new, money-moving Rust **to the standard you
would apply before approving a production PR** — block/allow, not general
impressions.

Full disclosure up front: **this code was written by an AI** (Claude, working
under the owner's control framework — frozen crates, named invariants,
fail-closed defaults, an open debt register). It moved real mainnet funds the
day it was written: four refunds recovering every stranded cent, then three
complete funding→deposit runs, the last with no human step after a single
Phantom approval. It has 25 offline tests and green CI. What it has **never
had is an independent reviewer** — the only party that has read every line is
the party that wrote it, and that party cannot be trusted to have reviewed
itself. You are the first pair of independent eyes on code that signs with a
real key.

## What the system is (60 seconds)

A policy-gated, fail-closed Solana control plane: a human defines *what
condition, what action, at most how much money*, signs **once** to fund that
envelope, and the automation executes inside it on live on-chain evidence.
The [README's "Anatomy of one successful run"](../README.md#anatomy-of-one-successful-run)
tells it in one diagram. Evidence for the claims, each re-derivable from the
public ledger: [execution acceptance, 40 claims](phase4-execution-acceptance.md)
and [refund acceptance, 12 claims](phase4-refund-acceptance.md).

Example transactions, live on mainnet:
[a refund](https://explorer.solana.com/tx/2qEajvMZcq5kw6H5vTfwRMYPLQmxVTk1C5uMKBMwQyhrh94NRoty89t7A1Xr1tfqvHrEwXGM6KHq268oF75jWf7n)
and [the fully-automatic deposit](https://explorer.solana.com/tx/5GCSA3C2m4KrisAaTZGh75XqhMz16AswE24xuJHu3TjvSokyLNyzht9FUAHafFHQDJnf2CULVdcTovV7MuNGm9Nu).

## The code under review

Branch [`feat/phase4-refund-rail`](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/tree/feat/phase4-refund-rail)
— [full diff vs `main`](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/compare/main...feat/phase4-refund-rail).
CI: [gate green](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/actions/runs/30814332193).
All links below pin commit `8051945`.

Threat model in one line: the refund signs with the controlled wallet's real
key, so the failure that matters is **paying twice** — everything else is
recoverable.

| File | Lines | What it is |
|---|---|---|
| [refund.rs](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/bins/solfrontier-mcp/src/refund.rs) | 499 | orchestration: recover → lease → re-derive → build → sign → journal → broadcast → verify → mark |
| [refund_journal.rs](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/bins/solfrontier-mcp/src/refund_journal.rs) | 333 | pre-broadcast durable record + pure crash-recovery decision |
| [refund_wallet.rs](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/bins/solfrontier-mcp/src/refund_wallet.rs) | 572 | simulation → policy → typestate → signing pipeline (deliberate copy, see Q2) |
| [refund_builder.rs](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/bins/solfrontier-mcp/src/refund_builder.rs) | 140 | unsigned transferChecked, zero blockhash, non-submittable |
| [refund_tests.rs](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/bins/solfrontier-mcp/src/refund_tests.rs) | 546 | 25 offline tests over every crash window |

## Six questions, ranked by leverage

**Q1 — Would you block this merge?**
The hot path is the crash window between signing and confirmation. Read, in
order: [recovery before anything else](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/bins/solfrontier-mcp/src/refund.rs#L282-L316)
(including the `never_leased` disambiguation — the main DB resolves an absent
journal, because the lease precedes signing),
[the CAS lease](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/bins/solfrontier-mcp/src/refund.rs#L335-L345),
[re-derive after the CAS](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/bins/solfrontier-mcp/src/refund.rs#L347-L360),
[journal BEFORE broadcast](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/bins/solfrontier-mcp/src/refund.rs#L388-L406),
[finalized-only verify, then the single CAS-bound mark](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/bins/solfrontier-mcp/src/refund.rs#L419-L470).
What did the author miss?

**Q2 — Deliberate duplication: right call or wrong?**
[refund_wallet.rs](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/bins/solfrontier-mcp/src/refund_wallet.rs#L1-L17)
copies the shape of the proven execute-rail pipeline instead of generalising
it — two narrow validators, each rejecting everything but its own single
shape, on the argument that widening the proven one loosens exactly the
checks that make it trustworthy. Including a duplicated `CapturingSigner`
([L490](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/bins/solfrontier-mcp/src/refund_wallet.rs#L490)).
When would you have chosen the abstraction instead?

**Q3 — The recovery decision, adversarially.**
[recovery_action](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/bins/solfrontier-mcp/src/refund_journal.rs#L291-L333)
is a pure function: resend the same bytes while the blockhash is valid, sign
fresh only after expiry, halt when the journal is unreadable
([`Unavailable` vs `Absent` is deliberate](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/bins/solfrontier-mcp/src/refund_journal.rs#L92-L103)).
Can you construct a sequence of crashes and RPC answers that pays twice?

**Q4 — Rust specifics an AI gets "probably right".**
[`synchronous(Full)`](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/bins/solfrontier-mcp/src/refund_journal.rs#L120-L134)
— is the durability claim in that comment actually honoured on Windows?
[`last_valid_block_height as i64`](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/bins/solfrontier-mcp/src/refund_journal.rs#L196)
— acceptable, or should it be a checked conversion? Anything else in the
diff where the idiom is subtly off?

**Q5 — The next design, before it gets built.**
The biggest open debt
([DEBT-P3-EXECUTION-1 in CLAUDE.md](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/CLAUDE.md))
is that the *execute* rail has no crash recovery for `executing`. The refund
rail's journal + blockhash-expiry pattern is the obvious candidate to promote.
How would you design that — and what about the frozen keystore's unprovable
zeroization (DEBT-P3-KEY-1) without touching the frozen crate?

**Q6 — Calibration: where does this smell AI-written?**
Name three places. The owner wants to learn to see it unaided.

## Already documented — no need to rediscover

The debt register in [CLAUDE.md](https://github.com/jeffreycheung521hk/solfrontiermcpskeleton/blob/8051945bebc592eddb58b02a9d10adc6c9c482cb/CLAUDE.md)
already records: single-intent rail only (no pagination, no auditable sweep);
recovery resend never exercised live; the two-step completion write is not
transactional; key zeroization unproven; everything is test funds on a pinned
wallet. Finding these again is confirmation, not discovery — the valuable
findings are the ones *not* on that list.
