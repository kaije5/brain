# Cortex v0.2 vault provider boundary threat model

## Scope

Extends the [v0.1 threat model](cortex-v0.1.md) for SCRUM-90's
provider-neutral knowledge/task contracts, policy and audit resource targets,
daemon configuration boundary, and fake-provider composition seam. Concrete
Markdown parsing, path confinement, atomic filesystem writes, watching, and
sync operations are implemented and tested in later Stories.

## Trust and authority boundaries

- The configured provider is authoritative for user-authored documents/tasks.
- `cortexd` remains the final policy and side-effect execution authority.
- Vault content, resource metadata, revisions, hashes, errors, and future sync
  status are external untrusted input.
- Cortex-owned runtime state remains in the daemon-controlled store.
- Models and clients receive capabilities and safe results, never raw
  filesystem/database access or credentials.

## Threats and contract mitigations

- **Provider-type or transport coupling:** vendor, sync, and filesystem types
  could enter core APIs and become privileged assumptions. Provider-neutral,
  bounded value types normalize at the adapter edge; architecture tests reject
  such dependencies in domain/application code.
- **Resource substitution and replay:** an operation ID could be replayed for a
  different provider resource. Command identity binds principal, workspace,
  capability, provider/scope/resource target, and operation ID.
- **Concurrent-edit loss:** a newer external edit could be overwritten. Every
  existing-resource mutation requires the observed revision and returns a typed
  conflict on mismatch; no hidden last-writer-wins retry is allowed.
- **Authority ambiguity:** legacy SQLite and the provider could both mutate the
  same content. SCRUM-90 creates no adapter, dual-write, or fallback. Legacy is
  isolated before cutover and removed by SCRUM-93 after verified replacement.
- **Audit disclosure:** titles, bodies, task descriptions, paths, provider
  diagnostics, or credentials could leak through audit. Audit targets and
  metadata are typed and redacted; tests use canary values to prove exclusion.
- **Unbounded provider data:** hostile identifiers, metadata, result sets, or
  errors could exhaust resources. Contract values and query/result sizes are
  bounded before application use; unknown failures map to safe errors.
- **False freshness:** local provider availability could be presented as global
  synchronization. Degraded/freshness state is explicit and provider-neutral;
  callers cannot infer remote freshness from local success.
- **Path disclosure or escape through resource identity:** a filesystem path
  could expose host structure or become an authorization target. Public
  resource IDs are opaque and stable; roots/scopes remain infrastructure
  configuration. SCRUM-91 separately proves traversal and symlink confinement.
- **Policy bypass during create:** a resource ID does not yet exist. Policy
  evaluates a provider scope target before dispatch, then audit records the
  resulting resource reference and revision/hash.

## Residual and deferred risks

SCRUM-90 does not read or write the filesystem. Parser abuse, path traversal,
symlink escape, atomic replacement failure, watcher races, conflict files, and
real sync behavior remain deferred to SCRUM-89, SCRUM-91, SCRUM-92, and
SCRUM-94. Their adapters must preserve this contract without widening trust.

The temporary legacy implementation remains attack surface until SCRUM-93, but
SCRUM-90 introduces no new consumer or mutation through it. The release cannot
ship with both authorities mutable.

## Verification expectations

Tests cover validation bounds, provider substitutability, resource-targeted
allow/deny decisions, destructive classification, replay mismatch, conflict and
degraded outcomes, audit redaction, and safe configuration diagnostics.
Repository dependency inspection proves domain/application public APIs contain
no Obsidian, sync-product, or filesystem types and no new knowledge/task
consumer depends on the legacy repositories.

