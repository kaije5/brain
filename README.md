# Cortex

Cortex is a local-first, terminal-native digital brain. The `cortexd` daemon owns the canonical SQLite state; the `brain` CLI, bounded local agent, and paired MCP clients all use its authenticated capability boundary.

## Acceptance tests

The end-to-end package launches the real daemon and CLI against temporary databases, protected pairing and platform-secret fixtures, a loopback fake model/OIDC boundary, and an in-process mutual-TLS relay that drives the real outbound tunnel and Streamable HTTP gateway. It does not contact a live model, identity provider, or public relay.

```text
cargo test -p cortex-e2e
```

The scenarios prove cross-interface shared state and provenance, deny-by-default MCP policy with redacted audit evidence, recoverable deletion/restoration, and lexical operation while the embedding endpoint is unavailable.
