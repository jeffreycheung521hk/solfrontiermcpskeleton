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
cargo check          # fix any rmcp 1.7 API drift first — skeleton was authored unbuilt
cargo run --bin solfrontier-mcp   # stdio MCP server
```

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
