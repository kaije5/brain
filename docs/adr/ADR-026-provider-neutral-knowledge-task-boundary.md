# ADR-026: Provider-neutral knowledge and task boundary

## Decision

Knowledge and task access crosses application-owned `KnowledgeProvider` and
`TaskProvider` ports using validated provider-neutral resource identities,
observed revisions, content hashes, provenance, bounded queries, typed mutation
results, and redacted errors.

Provider adapters normalize infrastructure data at the edge. Obsidian,
filesystem, iCloud, Obsidian Sync, Obsidian Headless, and other transport types
never enter `cortex-domain` or public `cortex-application` contracts.

## Context

Cortex must support a local Markdown implementation now without making a sync
product, path convention, or vendor API part of its core. Later providers and
test fakes must be substitutable without changing policy, planning, review,
retrieval, automation, IPC, CLI, TUI, or MCP consumers.

## Contract consequences

- Resource identity is stable across rename/move and is not represented by a
  filesystem path.
- Existing-resource mutations require the caller's observed revision and return
  the newly observed revision/hash or a typed conflict.
- Policy, operation replay identity, and audit target provider resources or
  create scopes without assuming an SQLite entity row.
- A complete in-memory fake proves substitutability and error semantics.
- Local roots, allowed paths, exclusions, and provider mode belong to daemon or
  infrastructure composition, not domain resources.
- Sync is replaceable infrastructure outside the knowledge/task contract.

## Alternatives rejected

A generic key/value repository was rejected because it cannot express stable
resource kinds, optimistic concurrency, destructive classification, or bounded
query semantics safely. Filesystem-path IDs were rejected because they leak
infrastructure and change on rename. Using MCP as Cortex's internal storage bus
was rejected because it would bypass the direct typed application boundary in
[ADR-005](ADR-005-capability-boundary.md).

