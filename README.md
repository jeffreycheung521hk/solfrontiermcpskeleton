# solfrontier-mcp

MCP rebuild of SolFrontier — a policy-gated, fail-closed Solana bounded-intent
control plane, exposed as an MCP stdio server.

## What this artifact proves — and deliberately does not

**Proves, verifiably on Solana mainnet:** an AI-proposed, human-funded,
policy-gated execution loop ran end-to-end with real money — typed proposal →
human Phantom signature → on-chain funding verification → CAS lease →
simulation → risk-engine policy → controlled-wallet signing → finalized-only
confirmation. Two acceptances — the funding leg alone, then the full loop —
each with a public transaction:
[Phase 2 funding, 0.1 USDC (2026-07-29)](https://explorer.solana.com/tx/67PPnfsYT6qFvEwiyyTojkqNE2E6UwiYq1qCRHW6g9hpvyVMNEWymjxSSz8Zwrn9eqDzMxvEfVTqUY3xe4NJcJHV)
and [Phase 3b-2 autonomous deposit, 0.2 USDC (2026-08-01)](https://explorer.solana.com/tx/hpVwPcMwy6RWATk2ks7HPEo8bS4zGAPaL7gx4BcdpBqgs4d27NrD6YF73T8nk9gokiatT7iGx2YdrNupzcdx6J4).
The [acceptance record](docs/phase3b-2-mainnet-acceptance.en.md) also preserves
a same-day **failed** run that the gates correctly refused — including the
0.2 USDC it left stranded, documented rather than hidden.

**Deliberately does not prove:** production readiness (a
[named debt register](CLAUDE.md) stays open: no refunds, no crash recovery, no
pagination, unproven key zeroization); on-chain delegated authorization (the
executed rail signs directly with a pinned wallet — no Authorization PDA);
more than one action (a single Solend USDC deposit, 0.10–1.00 USDC); or
unaided coding (built AI-assisted under a human-authored control framework —
the invariants, evidence grading, and risk acceptances in
[TRUST.en.md](docs/TRUST.en.md) are the point). Project status:
[STATUS.md](STATUS.md).

- Start here: [`CLAUDE.md`](CLAUDE.md) (working rules) and
  [`docs/重構建議書.md`](docs/重構建議書.md) (full architecture rationale & roadmap).
- Predecessors: [Solfrontier2026](https://github.com/jeffreycheung521hk/Solfrontier2026)
  (post-deadline fixes, canonical) / [testingcrypto2](https://github.com/jeffreycheung521hk/testingcrypto2)
  (hackathon submission).

## Quick start (Phase 2)

```bash
cargo check --workspace
cargo run --bin solfrontier-mcp   # stdio MCP server
```

The state-store path is selected with `--db PATH` or `SOLFRONTIER_DB`; the
default is `./data/solfrontier.db`.

`get_position`, `finalize_intent`, and `confirm_funding` read their mainnet
endpoint only from `SOLFRONTIER_RPC_URL`. Leaving it unset does not prevent
startup or affect the other tools; network-dependent calls return normal
`config_missing` JSON when they have enough local context to reach that
preflight. Finalize checks RPC configuration and wallet syntax before claiming
its one-shot draft, so either preflight failure can be fixed and the same draft
retried. Once those checks pass, the draft is consumed before the market read;
a later market-data failure requires a new proposal.

Finalize also reads the public Save reserve API at the pinned
`https://api.solend.fi` base to capture the predecessor-compatible APY
snapshot. There is no Save API key or base-URL override. RPC and Save failures
are reduced to sanitized error categories; endpoint URLs, query strings,
provider response bodies, and raw transport errors are not returned or logged.

`get_quote` uses the public `https://api.jup.ag` base by default. A compatible
proxy or staging endpoint can be selected with
`SOLFRONTIER_JUPITER_BASE_URL`; raw HTTP errors, response bodies, and the
configured URL are never returned or logged.

### Pure Phase 2 `propose_intent`

`propose_intent` is a pre-finalization draft calculator, not a finalized intent
or an on-chain Memo identity. This slice is deliberately pinned to
`deposit` / `solend` / `USDC` / `save` / `gt`. The decimal `amount` must be
between `0.10` and `1.00` USDC inclusive and is parsed without floating point;
`threshold_bps` must be `1..=10000`, and
`expiry_seconds_after_finalize` must be exactly `180`.

The raw `original_user_message` exists only long enough to be hashed and is
neither returned nor persisted. A valid proposal returns `status: "ok"`, a
typed draft summary, a 64-character lowercase-hex `draft_hash`, and a fresh
random UUID v4 `draft_id`; invalid input returns normal tool JSON with
`status: "invalid_input"`. `draft_id` is a non-canonical consume-once handle:
it is never included in the draft-hash or canonical-rule-hash preimage.
Proposing does not store either ID. In both success and failure cases,
`persistence.db_row_exists` is `false`, and this tool performs zero database
writes, sidecar writes, network calls, signatures, or transaction construction.

Example MCP tool arguments:

```json
{
  "action": "deposit",
  "protocol": "solend",
  "asset": "USDC",
  "display_source": "save",
  "comparison": "gt",
  "amount": "0.5",
  "threshold_bps": 50,
  "expiry_seconds_after_finalize": 180,
  "controlled_wallet": "BPfDMmeMBmCbMC1rWh7hwigMBoKGBrKwXxSeUu9hhs5L",
  "controlled_usdc_ata": "7LFdKcSV7JQYi3or5y9phHVPjhGigu5DDjUakAFbBmk3",
  "original_user_message": "If Save APY > 0.5%, deposit 0.5 USDC"
}
```

Expect `status: "ok"`, a random `draft_id`, a 64-character draft hash, and
`persistence.db_row_exists: false`.

### Write-capable Phase 2 `finalize_intent`

`finalize_intent` is deliberately advertised to the MCP host as **WRITE
DATABASE**. It accepts every proposal field again, plus the exact `draft_id`
and `draft_hash` returned by `propose_intent`, and the public `user_wallet`
that will fund the controlled USDC ATA. The external `user_wallet` is resolved
only at finalize time and is not part of the frozen draft-hash preimage.
The controlled wallet and controlled USDC ATA remain pinned to the audited
legacy values; they cannot be replaced through tool input.

Finalize first recomputes the draft hash. A mismatch returns normal JSON with
`status: "hash_mismatch"` and makes no write. Missing RPC configuration and an
invalid `user_wallet` are also rejected before any consume-once tombstone is
written, so either condition can be fixed and the same draft retried. Once
those preflight checks pass, finalize atomically claims `draft_id` in the
MCP-owned sidecar before the market read. A later `market_data_error` therefore
consumes that draft: fix the problem and call `propose_intent` again instead of
retrying the same `draft_id`.

The compatible persistence order is intentionally non-transactional:
WatchRule, derived sidecar mapping, then funding intent. On success the result
contains `intent_id`, `rule_hash`, a complete `funding` object, and—only when
`funding_actionable: true`—a `signing_page_url` under
`http://127.0.0.1:8080/index.html#...`. Funding fields include payer
wallet/ATA, controlled destination ATA, USDC mint/decimals, exact raw amount,
deadline, Memo program, and the exact Memo text
`claw:w5h:<intent_id>:<rule_hash>`. This tool does not construct, sign, or
submit the funding transaction. `funding_window_seconds` is descriptive only
and is derived from the implementation's funding-window constant; every
signing-page countdown must use the absolute `expires_at_ms` value and must
never reconstruct a deadline from that window length.

If the legacy non-transactional path writes a funding row but cannot confirm
the WatchRule, finalize returns `status: "rule_unconfirmed"` with
`funding_actionable: false` and deliberately omits both `funding` and
`signing`. Funding at that point would be stranded pending a manual refund.
The three retained partial-write/timestamp defects, response-level guard, and
their Phase 3 gates are recorded in
[`CLAUDE.md`](CLAUDE.md#技術債).

The deterministic legacy rule ID can collide with a previously finalized
draft having the same amount and threshold. In that recovery path the main DB
row remains authoritative: finalize returns its existing wallet, ATA, amount,
status, and deadline only after fully revalidating the stored rule and funding
identity. A different funding wallet or conflicting rule/row returns a normal
`existing_*_conflict` result with no funding instructions. An expired or
already-advanced existing intent is also non-actionable; finalize never
manufactures a fresh deadline for it.

The sidecar path is derived by appending
`.mcp-intent-index.sqlite3` to the configured main DB path. For example:

```text
./data/solfrontier.db
./data/solfrontier.db.mcp-intent-index.sqlite3
```

This sidecar is derived identity/consume metadata, never the lifecycle source
of truth. A canonical-hash lookup reads the mapped ID, then reloads the
WatchRule through its public repository and recomputes the rule hash; when a
funding row exists, its hash and IDs are checked too. If the sidecar is
missing, corrupt, incompatible, unmapped, or inconsistent,
`get_intent_status` returns normal JSON with `status: "unsupported_ref"`;
it does not crash or guess. UUID/`intent_id` lookup continues to use the main
DB. Do not manually edit or delete the sidecar while relying on its
consume-once history.

### Local-only Phantom funding page

`web/signing-page/index.html` is the single HTML entry point of a self-contained,
build-free static directory with no project backend. All executable JavaScript
is served locally from that directory; the signing path does not download code
from a CDN. Phantom does not inject its provider into `file://`, so the user
must serve the complete directory on loopback. The MCP binary remains
stdio-only: it does not open an HTTP port or browser.

The page vendors `@solana/web3.js` 1.98.4 at
`vendor/solana-web3-1.98.4.iife.min.js`. It was downloaded independently from
the exact former source URL
`https://cdn.jsdelivr.net/npm/@solana/web3.js@1.98.4/lib/index.iife.min.js` and
checked byte-for-byte against the authoritative npm tarball
`https://registry.npmjs.org/@solana/web3.js/-/web3.js-1.98.4.tgz`
(`package/lib/index.iife.min.js`). The upstream 463,860-byte artifact SHA-384
is
`sha384-I45YF+S0YGWIolUyTksLk9TNtTqaDgZg8e6T1OoBoJvvFmphqYNIPZw3Kl0TkZNN`.
The checked-in asset includes only the provenance header described above; its
runtime SRI/SHA-384 is
`sha384-1BCTVxwMWGkdmrSvaJBEHRYT0CcWOFIa2ugv4m58jLNYs50TzfnRE8UVuTOgpvgc`.
The upstream MIT notice is preserved beside it as
`vendor/LICENSE.solana-web3.js`. `.gitattributes` marks the executable asset as
binary so Git cannot rewrite its reviewed bytes through line-ending conversion
and invalidate SRI on another checkout.

Vendoring removes externally hosted executable code from the 180-second
signing window and lets CSP restrict scripts to the loopback origin. Phantom
and the keyless public Solana RPC remain explicit external runtime services;
the latter is used only to fetch a recent blockhash.

With Python, run this complete command from the repository:

```bash
python -m http.server 8080 --bind 127.0.0.1 --directory web/signing-page
```

Without Python, use a loopback-bound static server:

```bash
npx serve -l tcp://127.0.0.1:8080 web/signing-page
```

The loopback binding and directory restriction are security boundaries; do not
omit either. In particular, never run bare `python -m http.server 8080` with
the repository as its serve root. That default can expose the whole checkout
to the local network, including a local `.env` containing an RPC key.

The actionable URL carries the complete public funding instruction in its URL
fragment, so the fragment is not sent in the static server's HTTP request or
access log. Never put a private RPC URL, API key, seed phrase, or private key
in that URL. The page accepts only the exact `payload=` fragment emitted by
finalize, uses `funding.memo` verbatim, requires
`instruction_order == ["memo", "transfer_checked"]`, and takes token decimals
from finalize rather than hard-coding the transaction byte. It enables signing
only for `status: "funding_required"` plus `funding_actionable: true`, before
the absolute `funding.expires_at_ms`, and when the connected Phantom wallet is
the registered `funding.user_wallet`.

The page displays amount, source/destination ATAs, full Memo, canonical
identity, and remaining time. Countdown is calculated from the absolute
`expires_at_ms`; `funding_window_seconds` is never treated as remaining time.
The page pins the keyless public mainnet endpoint
`https://solana-rpc.publicnode.com` solely to fetch a recent blockhash. The
Solana public endpoint previously used here is unsuitable for this browser
handoff: its CORS preflight accepts the loopback Origin, but its JSON-RPC POST
returns HTTP 403 when that Origin is present. CSP therefore replaces that
endpoint with only the exact PublicNode origin; it does not add a second
network destination.

This blockhash provider is not a monetary trust anchor. Amount, destination,
and Memo all come from the reviewed `finalize_intent` response, Phantom shows
and signs those fixed instructions, and `confirm_funding` independently reads
the chain and revalidates them. A malicious or unavailable blockhash provider
can make the transaction fail or expire, but cannot redirect the transfer.
A provider failure or rate limit is shown immediately with an explicit manual
retry button—there is no silent retry consuming the deadline. Phantom performs
the one `signAndSendTransaction`; the page then displays the transaction
signature for `confirm_funding`.

### Write-capable `confirm_funding`

`confirm_funding` accepts `intent_id` and `tx_signature`. It reads the
transaction at Solana `confirmed` commitment, revalidates the authoritative
WatchRule and funding row, and requires all of the following before any
lifecycle write:

- Memo exactly equals `claw:w5h:<intent_id>:<rule_hash>`.
- The registered `user_wallet` is an on-chain signer.
- `TransferChecked` uses the registered user ATA, controlled ATA, USDC mint,
  exact `amount_raw`, registered authority, and finalize-provided decimals.
- The user ATA's token balance decreases by exactly `amount_raw`; the
  controlled ATA increases by exactly the same amount.
- Both token-account owners, both canonical ATAs, the mint, and transaction
  success all match the main DB identity.

An unconfirmed transaction returns `pending_confirmation` and can be retried
with the same arguments without changing the DB. RPC errors are reduced to
fixed safe classes. Any evidence mismatch returns `verification_failed` and
does not flip lifecycle state. Only a fully valid proof runs the two public
CAS operations in order:
`funding_required → funding_submitted → budget_reserved`.

The predecessor actually requested `confirmed`, not `finalized`. The retained
state-store field name `funding_finalized_slot` stores the top-level
`getTransaction.result.slot`; the name is not a finalized-commitment guarantee.
If the second CAS is interrupted, the row may remain `funding_submitted`.
Retry the same `intent_id` and `tx_signature`: the transaction is fetched and
fully validated again, the first CAS is idempotent for that signature, and the
second CAS resumes. `mark_funding_invalid_if_submitted` is not used by this
validate-before-flip path.

Late funding is judged by the transaction block time (or an explicitly labeled
confirmation-clock fallback) against the authoritative funding row
`expires_at_ms`. A valid late payment preserves predecessor behavior and is
recorded, but the response explicitly says it is refundable and currently
requires manual handling. The separate WatchRule `expires_at_slot`
(`created_at_slot + 480`) is also returned because it controls whether the
future executor can obtain a lease. There is no automatic refund handler yet;
funds may remain in the controlled ATA.

### Manual Phase 2 funding acceptance

The completed 0.1 USDC mainnet acceptance and post-merge gate are recorded in
[`docs/phase2-mainnet-acceptance.md`](docs/phase2-mainnet-acceptance.md).

> **180-second hard window:** use the funding row's absolute `expires_at_ms`.
> Start the static page first, then complete
> `propose → finalize → open page → Phantom sign → confirm_funding` in one
> continuous flow. A late payment is recorded but held for manual recovery.

> **Phase 3b rule identity must be unused before starting:** `derive_rule_id`
> takes `threshold_bps`, `amount_raw`, and `controlled_wallet`, but the
> controlled wallet is a compile-time constant. In this rail, identity is
> therefore effectively determined only by `(threshold_bps, amount_raw)`:
>
> ```text
> intent_id = hex(
>   threshold_bps as u32 little-endian
>   || amount_raw as u64 little-endian
>   || first 4 decoded bytes of the pinned controlled wallet
> )
> ```
>
> The Phase 2 acceptance specimen already occupies `(50, 100000)` / 0.1 USDC,
> whose `intent_id` is `32000000a0860100000000009a62dace`; do not reuse that
> tuple for Phase 3b. The 0.5 USDC / 50 bps example below maps to
> `3200000020a10700000000009a62dace`. Before `propose_intent`, call
> `get_intent_status` with the prospective identity:
>
> ```json
> {
>   "intent_ref": "3200000020a10700000000009a62dace"
> }
> ```
>
> Continue only when it returns `status: "not_found"`. If any row already
> exists, choose a different approved amount or threshold; never migrate,
> overwrite, or manually edit the historical database row.

1. Build the release binary and keep the real `SOLFRONTIER_RPC_URL` only in the
   MCP host environment. The Save read uses its pinned public endpoint.
2. In another terminal, start the signing page with the exact loopback command
   above.
3. Restart the MCP host and verify `tools/list` contains `get_quote`,
   `get_position`, `get_intent_status`, `propose_intent`, `finalize_intent`,
   and `confirm_funding`.
4. Call `propose_intent` with the example arguments above. Immediately call
   `finalize_intent` with the exact proposal fields, returned `draft_id` and
   `draft_hash`, plus the Phantom funding wallet:

   ```json
   {
     "action": "deposit",
     "protocol": "solend",
     "asset": "USDC",
     "display_source": "save",
     "comparison": "gt",
     "amount": "0.5",
     "threshold_bps": 50,
     "expiry_seconds_after_finalize": 180,
     "controlled_wallet": "BPfDMmeMBmCbMC1rWh7hwigMBoKGBrKwXxSeUu9hhs5L",
     "controlled_usdc_ata": "7LFdKcSV7JQYi3or5y9phHVPjhGigu5DDjUakAFbBmk3",
     "original_user_message": "If Save APY > 0.5%, deposit 0.5 USDC",
     "draft_id": "<COPY_FROM_PROPOSE>",
     "draft_hash": "<COPY_FROM_PROPOSE>",
     "user_wallet": "<PHANTOM_FUNDING_WALLET_PUBKEY>"
   }
   ```

5. Approve the host's database-write confirmation. Continue only if the
   response says `funding_actionable: true`; immediately open its
   `signing_page_url` and compare the amount, controlled ATA, full Memo, and
   remaining time.
6. Connect the registered Phantom wallet. A different wallet must be visibly
   rejected. Confirm Memo is instruction 0 and TransferChecked is instruction
   1, then sign and send. If the public blockhash RPC fails, use the explicit
   manual retry once; if the timer reaches zero, start again from propose.
7. Copy the returned signature and immediately call:

   ```json
   {
     "intent_id": "<INTENT_ID_FROM_FINALIZE>",
     "tx_signature": "<PHANTOM_TRANSACTION_SIGNATURE>"
   }
   ```

8. If the result is `pending_confirmation`, retry those exact arguments—do not
   sign a second payment. Success is `budget_reserved`. Confirm both
   `get_intent_status(intent_id)` and `get_intent_status(rule_hash)` resolve
   that same main-DB lifecycle.
9. If `late_funding.refundable` is true, retain the signature and stop. The
   payment was recorded after expiry but is not normal executable funding;
   refund currently requires manual handling.

### Phase 3b watcher: default dry-run and explicit execution

`watch` is a non-MCP operator subcommand. Without `--execute` it is an
audit-only dry-run: it opens the existing SQLite database with `mode=ro` and
`PRAGMA query_only=ON`, and it cannot run migrations, acquire a lease, change
state, load a keypair, simulate, sign, broadcast, submit, or confirm a
transaction. It does not read `SOLFRONTIER_CONTROLLED_WALLET_KEYPAIR` in this
default mode.

Only the explicit `watch --execute` form constructs the separately reviewed
write-capable path. That mode revalidates the entire bounded scan result and
each candidate, acquires the CAS lease, assembles and prints the complete
unsigned shape, then lets wallet-engine attach a fresh blockhash and pass that
exact transaction through simulation, a real risk-engine policy, and the
`Approved` typestate before the pinned controlled wallet signs. The captured
signed message is checked against the approved message and the exact submitted
bytes before one-shot broadcast. This is a deliberate security correction
relative to the predecessor W5i direct-sign bypass.

The execute preflight report says
`database_access: "sqlite_read_only_preflight_with_execute_writer"`: its
candidate reads still use the `mode=ro`/`query_only` pool, but the same process
has already opened a separate write-capable state-store pool for CAS and
terminal transitions. This is an RO/RW dual-pool design, not two fixed SQLite
connections: the read pool is capped at one connection, while the writer uses
the state-store default pool capacity. After a candidate wins the CAS—and before the wallet
review/sign call—the watcher prints the complete freshly revalidated unsigned
transaction to stderr in the same schema as dry-run (four instructions, every
account meta, program ids, data hex, fee payer, amount, and serialized bytes).
After successful simulation attaches the fresh blockhash, it prints a second
report in that same schema containing the exact unsigned message entering
policy approval and signing. The wallet also normalizes only that blockhash
and byte-compares the rest of the message with the canonical builder output.
If either audit report cannot be serialized, signing is not called, the
pre-broadcast lease is released, and no later candidate in that cycle runs.

The two SQLite pools do not make all preceding reads atomic with the lease.
The public CAS is exactly `intent_id = ? AND status = 'budget_reserved' AND
expires_at_ms > lease_now_ms`: it atomically excludes a competing
execution/refund/status transition and rechecks the persisted funding deadline
at the inclusive endpoint. It does **not** predicate the funding hash, amount,
wallet topology, WatchRule lifecycle/slot deadline, or live reserve/account
facts. Funding identity/amount/wallet fields have no mutation path in the
public repository after insertion, but WatchRule and chain facts are separate
state. A target-specific read immediately before CAS minimizes that TOCTOU
window; the canonical transaction and post-CAS wallet checks prevent parameter
substitution, but do not turn those separate facts into one database
transaction. This is an accepted test-funds boundary, tracked as
`DEBT-P3-EXECUTION-2`; it must be redesigned before non-test custody or a
production-ready claim.

The execution path still does **not** call `clawsol-authority` or consume an
Authorization PDA: it is a narrowly bounded, operator-started controlled-wallet
rail, not an on-chain user-delegated grant. The bin clears both caller-owned key
JSON bytes and its parsed raw-key buffer, but zeroization of key material held
inside the frozen `SecretKeystore` is not yet independently proven; use test
funds only and see `DEBT-P3-KEY-1` in [`CLAUDE.md`](CLAUDE.md).

Use the release artifact produced by the Windows CI gate. `--once` scans one
bounded batch and exits; without it the process repeats every 30 seconds:

```powershell
target\release\solfrontier-mcp.exe --db .\data\solfrontier.db watch --once
target\release\solfrontier-mcp.exe --db .\data\solfrontier.db watch
```

Set `SOLFRONTIER_RPC_URL` in the process environment for confirmed slot/account
reads. The endpoint is never printed. Without it, DB-only blockers such as a
legacy action, hash mismatch, amount mismatch, or wall-clock expiry remain
visible, while an otherwise eligible candidate stops at `clock_unavailable`.
All dry-run JSON is written to **stderr**; stdout stays unused and remains
reserved for MCP JSON-RPC when the binary runs as the stdio server.

For an eligible schema-v2 `SolendDeposit`, the report revalidates the canonical
hash, persisted wall-clock deadline, rule slot deadline, exact three-way
amount, reserve freshness (maximum 16 slots), obligation and token-account
identities, ATA existence, and the full-precision WAD condition. It then
assembles exactly:

1. compute-unit limit;
2. compute-unit price;
3. Solend `RefreshReserve`;
4. Solend deposit-reserve-liquidity-and-obligation-collateral.

The dry-run report includes every account meta (`pubkey`, `is_signer`, `is_writable`),
program id, data hex, and serialized transaction bytes. Those bytes are
intentionally **unsubmitable**: `sendable:false` and
`recent_blockhash:null` mean the serialized message contains the all-zero
placeholder blockhash; every allocated signature slot contains only the
default signature. A signer account meta expresses a future requirement, not
a signature. Never submit this base64 or patch a blockhash/signature into it.
The explicit `--execute` path does not submit the dry-run placeholder. It
revalidates fresh facts and both clocks, acquires the CAS lease, assembles and
prints the same complete shape again, then lets the wallet pipeline attach a
fresh blockhash for simulation. It prints that exact fresh-blockhash unsigned
message before policy/signing and signs only those approved bytes.

Finality is symmetric for terminal success and failure. A processed or
confirmed observation remains pending even when it contains a transaction
error; the watcher keeps the row `executing`, does not rebroadcast, and keeps
polling the same deterministic signature. Only a finalized success completes
the intent, and only a finalized error marks it failed. If the polling budget
expires first, the result remains unknown and requires manual reconciliation.

Both public list APIs are bounded. One cycle requests 129 rows, reports
`*_scan_truncated`, and processes the oldest 128. They currently have no
cursor and deserialize a batch all-or-nothing, so a corrupt row can fail that
table's entire cycle. In addition, the original finalize-created
`funding_required` orphan is not enumerable by the available funding scan;
`orphan_funding_only` covers only enumerated `budget_reserved` rows. These
limits are tracked in `DEBT-P2-FINALIZE-1/2`; PR #17's whole-round execute
abort is recorded in `DEBT-P3-WATCH-1`, while pagination and row-isolation
ownership now lives in `DEBT-P2-FUNDING-1` in [`CLAUDE.md`](CLAUDE.md). Do not
infer complete orphan coverage from a green dry-run.

The real Phase 2 database-copy output and the separate offline `ready`
instruction cross-check are recorded, with their evidence grades kept
distinct, in
[`docs/phase3b-dry-run-acceptance.md`](docs/phase3b-dry-run-acceptance.md).

### Phase 3b-2 autonomous controlled-wallet mainnet acceptance (PR #17)

PR #17 introduces the write-capable implementation; this runbook is not by
itself evidence that a mainnet acceptance run succeeded. That result must be
recorded separately against the exact post-merge artifact. Here,
**autonomous** means that after a human funds the bounded intent and explicitly starts
`watch --execute`, one watcher round performs condition revalidation, the CAS
lease, simulation/policy checks, controlled-wallet signing, broadcast, and
finality polling without a manual database transition. It does not mean that
an MCP tool or the user's main wallet signs automatically.

Use mainnet test funds only. A successful test does not make this rail
production-ready or crash-recovering.

#### Pre-execution checklist

- [ ] Use the Windows release artifact produced by the post-merge `main` run
      for PR #17; record its Actions run id and SHA-256. Use the same SQLite
      database as `finalize_intent` and `confirm_funding`, and stop any older
      `watch` process before starting the acceptance run.
- [ ] Set `SOLFRONTIER_RPC_URL` to the intended mainnet endpoint and
      `SOLFRONTIER_CONTROLLED_WALLET_KEYPAIR` to the pinned controlled-wallet
      keypair path. Do not print, copy into chat, or commit either value.
- [ ] Verify that the keypair pubkey is exactly
      `BPfDMmeMBmCbMC1rWh7hwigMBoKGBrKwXxSeUu9hhs5L`, that this wallet has
      enough SOL for fees, and that the pinned USDC source ATA, Solend
      obligation, and collateral ATA all pass the read-only watcher checks.
- [ ] Do **not** reuse the Phase 2 `0.1 USDC / 50 bps` specimen. The recommended
      acceptance tuple is `0.2 USDC / 40 bps`
      (`amount_raw = 200000`), whose prospective deterministic identity is
      `28000000400d0300000000009a62dace`. Use it only if
      `get_intent_status` returns `status: "not_found"`.
- [ ] Precheck the live Save display APY before proposing. It must be strictly
      greater than the selected threshold with enough margin to remain true
      through funding and execution. If `40 bps` is not suitable or its
      identity already exists, choose a fresh non-`50 bps` threshold and
      recompute/check the prospective identity; never overwrite an old row.
- [ ] Keep the signing page ready and confirm that the funding wallet,
      controlled ATA, full Memo, exact amount, and remaining wall-clock window
      match the finalized response before signing.
- [ ] Plan to finish proposal, funding confirmation, and execution within both
      persisted deadlines. If the read-only precheck reports insufficient
      wall-clock or slot headroom, stop and restart from `propose_intent` with a
      fresh unused tuple. Never extend a deadline or repair status with SQL.
- [ ] Require one complete read-only `watch --once` precheck with no
      `*_scan_failed` or `*_scan_truncated`. Orphan rows are classified and
      skipped per row; they do not abort the cycle. Review every reported
      `ready` row because execute mode may process multiple ready intents
      sequentially. Either scan flag makes the whole write-capable round
      ineligible before any lease or signature.
- [ ] Take any desired evidence backup while the process is stopped, but make
      **zero manual database changes**. Do not use `sqlite3`, direct SQL,
      `dev_seed`, or repository test helpers to create, lease, complete, or
      repair the acceptance row.

#### Mainnet acceptance sequence

1. Call `get_intent_status` for
   `28000000400d0300000000009a62dace` and continue only on
   `status: "not_found"`. Precheck the live Save APY and controlled-wallet
   account prerequisites described above.
2. Call `propose_intent` for a `0.2` USDC Solend deposit with
   `threshold_bps: 40`, then immediately call `finalize_intent` with the exact
   returned draft fields and the Phantom funding wallet. Approve only the
   normal host database-write confirmation.
3. Open the returned loopback signing-page URL, have the registered human
   wallet sign the exact `0.2` USDC funding transaction, and call
   `confirm_funding` with that signature. If it returns
   `pending_confirmation`, retry the same signature; never sign a second
   payment. Continue only when `get_intent_status` reports
   `status: "budget_reserved"` and the WatchRule identity and
   `amount_raw = 200000` still match.
4. Run the read-only precheck against that same database:

   ```powershell
   target\release\solfrontier-mcp.exe --db .\data\solfrontier.db watch --once
   ```

   Continue only if the target is `ready`, its condition is still true, all
   account/amount/hash checks pass, both windows have enough headroom, and the
   report contains neither `*_scan_failed` nor `*_scan_truncated`. Orphans are
   reported and skipped; they are not a whole-cycle blocker. If multiple rows
   are ready, inspect all of them because execute mode processes them
   sequentially. If the target is not ready, stop; do not mutate the row to
   make it executable.
5. Start one write-capable watcher round. The acceptance run uses `--once` to
   isolate a single reviewed cycle; the operative mode is `watch --execute`:

   ```powershell
   target\release\solfrontier-mcp.exe --db .\data\solfrontier.db watch --execute --once
   ```

   Do not run a second executor in parallel and do not make any manual DB
   transition while it is running.
6. Require the execution report to carry one transaction signature and an
   unambiguous **finalized** result. `confirmed`, `pending`, a polling timeout,
   or an `executing` row is not acceptance success and must not be followed by
   an automatic second transaction.
7. Using only public status/RPC reads, verify all of the following:

   - the reported signature is finalized on mainnet with `err: null`;
   - the exact `200000` raw USDC amount was executed;
   - `get_intent_status(intent_id)` and
     `get_intent_status(canonical_rule_hash)` resolve to the same row;
   - the funding intent reports `status: "completed"` and the WatchRule reports
     `watch_rule_status: "completed"`;
   - the watcher execution report says both durable completion writes applied
     and associates them with that same execution signature and finalized
     slot.

If either deadline becomes too short before step 5, abandon that attempt and
restart at step 1 with a fresh unused identity. If funding was already sent,
retain its signature and stop: there is currently no automatic refund handler
or complete, operational manual-refund procedure, so funds may remain in the
controlled ATA pending explicit reconciliation. Do not repurpose the payment,
edit the database, or imply that recovery is already available. If the process
crashes or broadcast finality is ambiguous, leave the durable state in
`executing` and reconcile the known signature on-chain before any retry.

### Development smoke-test data

Reusable development fixtures live in `bins/solfrontier-mcp/src/dev_seed.rs`.
The explicit seeding entry point is an ignored integration test, not a Cargo
binary target, so it is never included in `cargo build --release` artifacts.
On PowerShell:

```powershell
$env:SOLFRONTIER_DB = "$PWD\data\mcp-smoke.db"
cargo test -p solfrontier-mcp --test dev_seed -- --ignored --nocapture
target\release\solfrontier-mcp.exe --db $env:SOLFRONTIER_DB
```

The current fixture prints the UUID to pass to `get_intent_status`. Add future
`get_position` / `get_quote` smoke fixtures to the shared module instead of
creating one-off seed binaries.

### Windows OpenSSL build workaround

The Solana 2.1 dependency graph currently enables vendored OpenSSL. On Windows,
that build requires Perl even though this workspace otherwise uses rustls. If
OpenSSL is installed at `C:\Program Files\OpenSSL-Win64`, point the current
PowerShell session at the installed libraries instead:

```powershell
$env:OPENSSL_NO_VENDOR = "1"
$env:OPENSSL_LIB_DIR = "C:\Program Files\OpenSSL-Win64\lib\VC\x64\MD"
$env:OPENSSL_INCLUDE_DIR = "C:\Program Files\OpenSSL-Win64\include"
cargo build --release
```

These variables are process-local; set them again in each new terminal. The
reverse dependency tree and the Phase 1 follow-up target are recorded in
`docs/size-baseline.txt`.

### Manual mainnet `get_position` verification

The server does not auto-load dotenv files. Keep the real provider URL outside
this repository and pass it through the environment of the MCP host. For
example, add the following to the external Claude Desktop
`claude_desktop_config.json`, replacing both path placeholders and the RPC
placeholder locally:

```json
{
  "mcpServers": {
    "solfrontier": {
      "command": "C:\\path\\to\\target\\release\\solfrontier-mcp.exe",
      "args": ["--db", "C:\\path\\to\\data\\solfrontier.db"],
      "env": {
        "SOLFRONTIER_RPC_URL": "<YOUR_PRIVATE_MAINNET_RPC_URL>"
      }
    }
  }
}
```

That external config may contain a provider key: never copy its real value
into `.env.example`, `.mcp.json`, an issue, a log, or a screenshot. Then:

1. Run `cargo build --release` with the Windows OpenSSL variables above.
2. Restart Claude Desktop and confirm `tools/list` still contains all six
   tools, including `get_position` and `confirm_funding`.
3. Call `get_position` with `{"wallet":"<BASE58_WALLET_PUBKEY>"}`.
4. Expect `ok` or `no_position`. An `ok` response contains each obligation
   pubkey, lending market, and deposits with exact raw cToken
   `deposited_amount`. USDC estimates remain `null` with an
   `estimate_unavailable_reason`.
5. Confirm neither the tool response nor stderr contains the configured
   endpoint or its query string.

### Manual mainnet `get_quote` verification

`get_quote` is a quote-only `GET /swap/v1/quote` client. It cannot build a
transaction, sign, broadcast, or create an approval. The default Jupiter base
is public and normally needs no environment configuration. If a compatible
base override is required, set `SOLFRONTIER_JUPITER_BASE_URL` only in the MCP
host environment; the server does not auto-load dotenv files.

1. Run `cargo build --release` with the Windows OpenSSL variables above.
2. Restart Claude Desktop and confirm `tools/list` contains all six tools,
   including `get_quote` and `confirm_funding`.
3. Call `get_quote` with:

   ```json
   {
     "input_mint": "So11111111111111111111111111111111111111112",
     "output_mint": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
     "amount": "10000000",
     "slippage_bps": 50
   }
   ```

4. Expect `status: "ok"`. `input_amount`, `in_amount`, `out_amount`, and
   `other_amount_threshold` must be JSON strings, not numbers.
5. Repeat with `slippage_bps: 101`; expect normal tool JSON with
   `status: "policy_blocked"`. This request must not contact Jupiter.
6. Confirm neither the tool response nor stderr contains the configured base
   URL, its query string, a raw reqwest error, or an upstream response body.
