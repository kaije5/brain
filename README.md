# Cortex

Cortex is a local-first, terminal-native digital brain. `cortexd` is the only
process that owns the canonical SQLite state. The `brain` CLI, bounded local
agent, and paired MCP clients use the daemon's authenticated capability
boundary; none opens the database directly.

## Start here

Use the release guides in this order:

1. [Local setup](docs/operations/local-setup.md) — start the daemon and use the
   authenticated local CLI.
2. [ChatGPT MCP and the outbound-only bridge](docs/operations/chatgpt-mcp.md)
   — understand the required relay/OIDC provisioning boundary before enabling a
   remote client.
3. [Backup and restore](docs/operations/backup-restore.md) — make a stopped,
   consistent copy and validate a restore without overwriting a live instance.
4. [Diagnostics](docs/operations/diagnostics.md) — inspect safe status output,
   model-offline behavior, and redacted failure categories.

The real local CLI includes status, diagnostic, note-creation, task, and
memory-search operations. Complete, executable forms and their output modes
are documented in the local setup guide.

## Security boundary

Cortex has no public daemon listener. Remote MCP access is **outbound-only**:
the local gateway binds only to `127.0.0.1` and initiates mutually authenticated
TLS to a stateless relay. The relay is not a database owner, policy engine, or
credential store. Do not copy pairing/enrollment files or place credential
values in configuration, commands, diagnostics, or issue reports.

## Verification

The end-to-end package launches the real daemon and CLI against temporary
databases, protected pairing and platform-secret fixtures, a loopback fake
model/OIDC boundary, and an in-process mutual-TLS relay. It never contacts a
live model, identity provider, or public relay.

```text
cargo test -p cortex-e2e
```

Those scenarios prove cross-interface shared state and provenance,
deny-by-default MCP policy with redacted audit evidence, recoverable
deletion/restoration, and lexical operation while the embedding endpoint is
unavailable.
