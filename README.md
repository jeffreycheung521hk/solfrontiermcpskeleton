# solfrontier-mcp

MCP rebuild of SolFrontier — a policy-gated, fail-closed Solana bounded-intent
control plane, exposed as an MCP stdio server.

- Start here: [`CLAUDE.md`](CLAUDE.md) (working rules) and
  [`docs/重構建議書.md`](docs/重構建議書.md) (full architecture rationale & roadmap).
- Predecessors: [Solfrontier2026](https://github.com/jeffreycheung521hk/Solfrontier2026)
  (post-deadline fixes, canonical) / [testingcrypto2](https://github.com/jeffreycheung521hk/testingcrypto2)
  (hackathon submission).

## Quick start (Phase 1)

```bash
cargo check --workspace
cargo run --bin solfrontier-mcp   # stdio MCP server
```

The state-store path is selected with `--db PATH` or `SOLFRONTIER_DB`; the
default is `./data/solfrontier.db`.

`get_position` reads its mainnet endpoint only from `SOLFRONTIER_RPC_URL`.
Leaving it unset does not prevent startup or affect the other tools;
`get_position` returns normal JSON with `status: "config_missing"`.

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
   `get_quote`, `get_position`, and `get_intent_status`.
3. Call `get_position` with `{"wallet":"<BASE58_WALLET_PUBKEY>"}`.
4. Expect `ok` or `no_position`. An `ok` response contains each obligation
   pubkey, lending market, and deposits with exact raw cToken
   `deposited_amount`. USDC estimates remain `null` with an
   `estimate_unavailable_reason`.
5. Confirm neither the tool response nor stderr contains the configured
   endpoint or its query string.
