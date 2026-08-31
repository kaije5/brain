# Cortex v0.1 threat model

## Scope

Cortex is local-first. `cortexd` owns mutable SQLite state and makes final policy decisions; the CLI and MCP gateway are authenticated adapters.

## Threats and v0.1 mitigations

- **Malicious MCP clients:** [authenticated principal derivation and capability authorization](../adr/ADR-010-capability-authorization.md) are enforced again by `cortexd`.
- **Prompt injection:** [capability schemas and deterministic policy](../adr/ADR-005-capability-boundary.md) constrain model-proposed actions.
- **Malicious documents and connector data:** [memory provenance](../adr/ADR-009-memory-provenance.md) retains evidence, while deterministic validation controls persistence.
- **Compromised API credentials:** [local-first operation](../adr/ADR-003-local-first.md) and opaque secret references limit credential exposure; no secret is persisted in domain data.
- **Local privilege-boundary attacks:** [authenticated local IPC](../adr/ADR-015-authenticated-local-ipc.md) fronts the sole mutable SQLite owner.
- **Accidental destructive actions:** [recoverable deletion](../adr/ADR-018-recoverable-deletion.md) provides restore before purge.
- **Poisoned memory:** [provenance-bearing assertions](../adr/ADR-009-memory-provenance.md) preserve sources, corrections, and conflict sets for review.
- **Hallucinated tool arguments:** [typed shared capabilities](../adr/ADR-005-capability-boundary.md) validate requests before state changes.
- **Audit leakage:** the [single daemon owner](../adr/ADR-004-sqlite.md) writes audit records locally and adapters expose only safe views.
- **Unauthorized remote access:** the [outbound-only bridge](../adr/ADR-016-outbound-mcp-bridge.md) has no public Cortex listener and authenticated gateway pairing.
- **Future relay compromise:** the [outbound-only bridge](../adr/ADR-016-outbound-mcp-bridge.md) treats the relay as stateless transport with no database access, storage credentials, or authorization role.

## Residual risks

A compromised local user account remains within the host trust boundary. v0.1 documents and constrains this risk but cannot make a compromised host trustworthy.
