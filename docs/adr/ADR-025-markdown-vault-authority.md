# ADR-025: Markdown vault authority for user knowledge and tasks

## Decision

The configured provider-backed Markdown vault is authoritative for
user-authored personal knowledge and task records. Cortex remains authoritative
for policy, grants, audit, operation identity, AI memories, automation,
planning/orchestration, connector state, and rebuildable derived indexes.

This supersedes the broad content-ownership implication of
[ADR-004](ADR-004-sqlite.md). SQLite remains Cortex's durable runtime store but
does not remain a second mutable canonical body store for user-authored
documents/tasks after the SCRUM-93 cutover.

## Context

The v0.1 implementation models notes and tasks as SQLite-owned aggregates. The
v0.2 product direction makes ordinary Markdown editable by Cortex and external
editors. Treating both representations as writable authorities would require a
permanent bidirectional synchronization system and permit ambiguous conflicts
or silent data loss.

## Consequences

- Before cutover, the existing SQLite implementation remains isolated and
  operational while the replacement is built.
- New knowledge/task behavior targets provider-neutral contracts and never
  extends the legacy repository abstraction.
- After verified migration and cutover, legacy note/task mutation paths and
  canonical tables are removed by SCRUM-93.
- Provider writes remain Cortex-policy-governed, attributable, audited,
  idempotent where applicable, and revision-safe.
- Human-editable content has one mutable authority at every runtime stage; no
  dual-write or hidden fallback mode exists.

## Alternatives rejected

Permanent SQLite/Markdown dual authority was rejected because two independent
writers cannot provide deterministic ownership without a substantially more
complex merge system. Keeping SQLite canonical and exporting Markdown views was
rejected because external edits would not be authoritative. Removing the
existing implementation before the replacement path exists was rejected
because it would leave current clients without working note/task behavior.

