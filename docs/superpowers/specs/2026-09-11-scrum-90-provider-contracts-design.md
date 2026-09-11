# SCRUM-90: Provider-neutral knowledge and task contracts

## Status and scope

Approved design for SCRUM-90. This Story establishes the contracts that later
Stories implement and cut over. It does not implement Markdown parsing,
filesystem access, indexing, migration, client-surface cutover, or removal of
the legacy SQLite note/task implementation.

The implementation starts from `main` at `3c286e8`. Existing SQLite-backed
note/task repositories remain isolated legacy code for the migration sequence.
SCRUM-90 does not wrap them, extend them, or present them as provider
implementations. SCRUM-93 deletes them after the replacement path and verified
cutover exist.

## Authority boundary

The configured knowledge/task provider is authoritative for user-authored
documents and personal task records. Cortex remains authoritative for policy,
grants, audit, operation identity, AI memories, automation, planning and
orchestration, connector state, and derived indexes.

Provider-backed content has one mutable authority. Cortex may store provider
references, observed revisions, hashes, provenance, orchestration links, and
rebuildable derived state, but not a second independently mutable canonical
body. Obsidian, iCloud, Obsidian Sync, Obsidian Headless, filesystem paths, and
transport metadata stay outside `cortex-domain` and public
`cortex-application` contracts.

## Domain vocabulary

Provider-neutral value types live in `cortex-domain`:

- `ProviderId`: validated, bounded identifier for a configured provider.
- `ProviderResourceId`: validated, bounded opaque identity assigned by the
  provider; it is not a path and remains stable across rename/move.
- `ProviderResourceRef`: provider ID, resource ID, workspace ID, and resource
  kind (`Knowledge` or `Task`).
- `ObservedRevision`: opaque, bounded revision token supplied by the provider.
- `ContentHash`: validated SHA-256 digest of the exact authoritative bytes.
- `ProviderProvenance`: provider resource reference plus observed revision and
  content hash.

These types validate blank, oversized, control-character, nil, and malformed
inputs as applicable. Debug output must not expose user-authored content, local
paths, transport state, or secrets.

Knowledge/task DTOs are provider-neutral application data, distinct from the
legacy canonical `Note` and `Task` aggregates:

- `KnowledgeDocument` contains the resource reference, title, body, structured
  metadata required by the port, and provenance.
- `ProviderTask` contains the resource reference, stable task identity, title,
  status and scheduling metadata required by later Stories, plus provenance.
- query inputs are workspace-scoped and bounded; mutation inputs include an
  operation ID and the expected observed revision for existing resources.

SCRUM-89 owns the final Markdown/frontmatter representation. SCRUM-90 therefore
does not encode YAML keys, directory conventions, Obsidian syntax, filenames,
or paths into these contracts.

## Application ports

`cortex-application` owns separate `KnowledgeProvider` and `TaskProvider`
traits. Both expose bounded reads and revision-aware mutations. The complete
surface supports:

- get and bounded list/search queries;
- create;
- update with an expected observed revision;
- delete with an expected observed revision;
- task completion as a revision-aware task mutation.

All methods return typed provider results. Successful mutations return the
resource reference and newly observed provenance. Reads distinguish found,
not-found, and degraded/stale observations without inventing global freshness.
Mutations distinguish validation, authorization, not-found, revision conflict,
degraded/unavailable, and safe internal failure. Provider error types never
carry raw file paths, external response bodies, credentials, or user content.

A complete in-memory fake implements both ports in contract tests. Later
Markdown adapters must pass the same behavior contract without changing domain
or application consumers.

## Policy, audit, and operation identity

Authorization and audit target a new provider-neutral `ResourceTarget`:

- `CortexEntity(EntityId)` for Cortex-owned runtime state during the migration
  period;
- `ProviderResource(ProviderResourceRef)` for authoritative knowledge/tasks;
- `ProviderScope { provider_id, workspace_id, resource_kind }` for creates,
  where no resource ID exists yet.

The target is part of command identity so replaying an operation ID against a
different provider, scope, or resource is rejected. Audit evidence records the
redacted provider/resource identity and before/after revision/hash metadata
needed for attribution. It never copies title, body, task description, local
root, relative path, or provider diagnostics.

Destructive provider mutations retain destructive capability classification.
Provider calls occur only after Cortex policy allows the exact principal,
workspace, capability, and target. A failed or conflicting provider mutation
does not record success or advance dependent Cortex state.

## Configuration and composition

`cortexd` owns configuration and adapter composition. Configuration declares:

- one local vault root;
- a bounded non-empty set of allowed logical scopes;
- bounded exclusions;
- an explicit provider mode suitable for future read-only/read-write policy.

The filesystem path types remain in `cortexd`/infrastructure configuration and
are converted into provider construction inputs, never domain resources.
Configuration contains no credentials or sync-transport state. Missing,
invalid, inaccessible, or contradictory values return typed, safe startup
diagnostics. Tests can inject the in-memory fake without a filesystem,
Obsidian, or live sync process.

SCRUM-98 composes only the boundary and fake-provider test seam. The concrete
Markdown provider and its path-confinement enforcement belong to SCRUM-91.

## Error and degraded-state behavior

- Invalid identifiers, revisions, hashes, bounds, or configuration fail before
  provider dispatch.
- Missing resources return typed not-found results without leaking provider
  details.
- Revision mismatches return explicit conflict data sufficient to re-read; no
  last-writer-wins retry is hidden in the port.
- Provider unavailability is explicit and may include safe freshness state;
  callers cannot claim the local view is globally synchronized.
- Unknown provider failures map to a redacted internal error.
- All collections, identifiers, metadata, and public error strings are bounded.

## Legacy isolation and migration

SCRUM-90 adds a parallel contract boundary, not a compatibility abstraction.
The existing `NoteRepository`, `TaskRepository`, `Note`, `Task`, and SQLite
aggregate mutations are not made to implement the new ports. No new feature
may depend on those legacy APIs. Their current callers remain operational until
the provider-backed path is implemented and switched over in later Stories.

This deliberate temporary coexistence is not dual authority: before cutover,
legacy SQLite remains the only active note/task implementation; after cutover,
the provider is the only mutable authority and SCRUM-93 removes the obsolete
paths. No runtime dual-write or fallback mode is introduced.

## TUI impact

SCRUM-90 has no new end-user behavior. It defines internal contracts and daemon
composition seams only, so the TUI is intentionally unchanged. SCRUM-88 owns
the visible vault configuration, freshness, conflict, and index-lag surfaces.
The TUI must eventually consume these contracts through daemon/application
capabilities and may never open the vault or database directly.

## Verification strategy

Implementation follows TDD per independently reviewable Jira subtask:

1. SCRUM-101 records this spec, superseding ADRs, and the threat boundary.
2. SCRUM-102 introduces domain resource/provenance types and the complete port
   surface, proven by an in-memory fake.
3. SCRUM-99 generalizes policy, operation identity, and redacted audit targets.
4. SCRUM-98 adds validated daemon configuration and fake-provider composition.
5. SCRUM-96 adds contract/architecture tests and a dependency inspection that
   proves new consumers depend on ports rather than legacy repositories or
   filesystem/Obsidian types.

Focused tests cover every validation and result branch, operation replay target
binding, allow/deny/destructive policy, audit redaction, conflict/degraded
results, configuration failures, and fake-provider startup. Story completion
also requires `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets --locked`, `cargo test --workspace`,
and `git diff --check`, followed by independent review and green PR checks.

No core test requires Obsidian desktop, a sync subscription, a network service,
or access to the user's real vault.

## Alternatives rejected

- Wrapping the SQLite repositories in the provider traits was rejected because
  it legitimizes the authority that this work supersedes and creates a hidden
  compatibility path.
- Defining one generic untyped storage trait was rejected because it weakens
  capability, validation, and mutation semantics.
- Encoding filesystem paths as public resource IDs was rejected because paths
  are mutable, expose infrastructure details, and do not preserve identity
  across rename/move.
- Implementing Markdown parsing or safe filesystem writes in this Story was
  rejected because SCRUM-89 and SCRUM-91 own those independently reviewable
  behaviors.

