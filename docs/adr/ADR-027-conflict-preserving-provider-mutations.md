# ADR-027: Conflict-preserving provider mutations

## Decision

Every update, completion, move-equivalent update, or deletion of an existing
provider-backed knowledge/task resource is conditioned on the observed
provider revision. A mismatch returns a typed conflict; Cortex never silently
overwrites or retries against a newer external edit.

Concrete adapters use atomic replacement where the platform supports it and
return the resulting observed revision and content hash. The port contract does
not expose filesystem primitives.

## Context

The authoritative Markdown content can be edited by other devices and tools.
SQLite-style assumptions that Cortex is the sole writer therefore do not apply
to provider-backed user content.

## Consequences

- Callers re-read and explicitly reconcile after conflicts.
- Failed/conflicting mutations cannot advance planner, review, automation, or
  other Cortex-owned state as though the content mutation succeeded.
- Operation IDs remain bound to the exact provider resource or create scope.
- Audit records safe before/after revision and hash metadata without copying
  content or local paths.
- Provider unavailability and stale sync state remain explicit; local success
  does not imply global synchronization.

## Alternatives rejected

Last-writer-wins was rejected because it silently loses external edits.
Automatic text merging was rejected because v0.2 does not implement a general
CRDT or ambiguous Markdown merge engine. Exposing temp files and rename
operations in application contracts was rejected because atomic-write mechanics
belong to the concrete adapter in SCRUM-91.

