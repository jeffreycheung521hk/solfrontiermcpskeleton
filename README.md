# solfrontier-mcp

MCP rebuild of SolFrontier — a policy-gated, fail-closed Solana bounded-intent
control plane, exposed as an MCP stdio server.

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

> **180-second hard window:** use the funding row's absolute `expires_at_ms`.
> Start the static page first, then complete
> `propose → finalize → open page → Phantom sign → confirm_funding` in one
> continuous flow. A late payment is recorded but held for manual recovery.

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
