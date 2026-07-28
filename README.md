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

`get_position` reads its mainnet endpoint only from `SOLFRONTIER_RPC_URL`.
Leaving it unset does not prevent startup or affect the other tools;
`get_position` returns normal JSON with `status: "config_missing"`.

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
typed draft summary, and a 64-character lowercase-hex `draft_hash`; invalid
input returns normal tool JSON with `status: "invalid_input"`. In both cases,
`persistence.db_row_exists` is `false`, and this tool performs zero database
writes, network calls, signatures, or transaction construction.

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

Expect `status: "ok"`, a 64-character draft hash, and
`persistence.db_row_exists: false`.

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
2. Restart Claude Desktop and confirm `tools/list` still contains
   `get_quote`, `get_position`, `get_intent_status`, and `propose_intent`.
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
2. Restart Claude Desktop and confirm `tools/list` contains `get_quote`,
   `get_position`, `get_intent_status`, and `propose_intent`.
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
