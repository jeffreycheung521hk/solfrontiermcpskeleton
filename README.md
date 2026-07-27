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

Register in Claude Desktop (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "solfrontier": {
      "command": "/path/to/target/release/solfrontier-mcp"
    }
  }
}
```
