# Task 5 report: SQLite state, audit, and idempotency

## Status

Implemented Task 5 only on `codex/cortex-v0.1`. The workspace now has the
`cortex-storage` crate with ordered SQLx migration, SQLite database startup,
read-only aggregate repository adapters, an `AtomicMutationPort`
implementation, durable idempotency outcomes, append-only redacted audit
storage, and opaque `SecretRef`/`SecretStore` contracts.

The domain changes are limited to the binding ruling: validated UUIDv7
conversions and explicit, storage-neutral aggregate rehydration constructors.
No Task 6 command/service orchestration or later task work was added.

## Inherited-work inspection

This task resumed an interrupted, uncommitted Task 5 state. Before changes,
the linked worktree was on `codex/cortex-v0.1`; the modified paths were the
workspace manifest/lockfile, the domain rehydration changes and test, and the
new `crates/cortex-storage` crate. `git diff --check` had no whitespace
errors. The required inherited focused storage command passed 7 tests before
the final audit regression was added.

The inherited implementation already used the required application-owned
`AtomicMutationPort` boundary rather than exposing a storage transaction to
application services. It also followed the binding ruling by rehydrating
domain values through validation constructors rather than exposing aggregate
fields or broad aggregate serialization.

## Changed paths

- `Cargo.toml`
- `Cargo.lock`
- `crates/cortex-domain/src/entity.rs`
- `crates/cortex-domain/src/ids.rs`
- `crates/cortex-domain/src/memory.rs`
- `crates/cortex-domain/src/note.rs`
- `crates/cortex-domain/src/provenance.rs`
- `crates/cortex-domain/src/task.rs`
- `crates/cortex-domain/tests/persistence_rehydration.rs`
- `crates/cortex-storage/Cargo.toml`
- `crates/cortex-storage/migrations/0001_initial.sql`
- `crates/cortex-storage/src/audit.rs`
- `crates/cortex-storage/src/database.rs`
- `crates/cortex-storage/src/lib.rs`
- `crates/cortex-storage/src/operation.rs`
- `crates/cortex-storage/src/repositories.rs`
- `crates/cortex-storage/src/secrets.rs`
- `crates/cortex-storage/tests/audit.rs`
- `crates/cortex-storage/tests/migrations.rs`
- `crates/cortex-storage/tests/operations.rs`
- `crates/cortex-storage/tests/repositories.rs`

## Red/green evidence

The prior implementer did not leave an initial-red transcript for the
inherited uncommitted tests. I therefore did not claim an unavailable history.
For each inherited behavior, I performed a reversible regression-removal
check: the existing focused test was run with only the responsible behavior
temporarily disabled, observed failing for the intended assertion, and the
exact implementation was restored before the final gates. None of those
counterfactual edits remains in the worktree.

| Behavior | Test evidence | Observed red evidence | Final green evidence |
| --- | --- | --- | --- |
| SQLite foreign-key enforcement and transaction rollback | `migration_is_repeatable_and_foreign_keys_reject_orphans`; `failed_mutation_rolls_back_entity_audit_and_operation` | With `.foreign_keys(false)`, the orphan insert succeeded and the failed mutation succeeded instead of rolling back. | Both pass with `.foreign_keys(true)`. |
| WAL startup and initial schema | `fresh_database_enables_wal_foreign_keys_and_all_initial_tables` | A reversible `Wal` to `Delete` edit could not start its rebuilt test binary because Windows application control returned `os error 4551`; no source conclusion was drawn from that environmental block. | Restored `Wal` test passed, including table and pragma assertions. |
| Aggregate persistence, validated rehydration, and workspace isolation | `repositories_round_trip_each_aggregate_without_crossing_workspaces`; domain `persistence_rehydration` suite | With the note lookup predicate forced false, the expected persisted note rehydrated as `None`. With `Revision::rehydrate(0)` temporarily accepted, `persisted_revision_rejects_zero` failed. | Repository round-trip and all 7 rehydration tests pass. |
| Idempotency returns the original mutation result without a second effect | `repeated_operation_id_returns_original_result_without_second_effects` | With replay return disabled, the duplicate call failed with `Storage("operation replay intentionally disabled")`. | The test passes and observes one note, one audit event, and one operation outcome. |
| Failed mutation has no entity, audit, or operation record | `failed_mutation_rolls_back_entity_audit_and_operation` | The foreign-key removal above caused the missing-source mutation to succeed, failing the expected error assertion. | The restored transaction/foreign-key path passes all zero-count assertions. |
| Audit round trip, append-only storage, redaction, and canonical capability validation | `audit_port_round_trips_only_redacted_evidence_and_is_append_only`; `audit_port_rejects_unknown_capabilities_without_persisting_them` | The new canonical-capability test was written first and failed at `audit.append(event).await.is_err()` because the writer accepted `cortex_unrecognized_capability`, which the reader could not rehydrate. | `insert_event` now shares `canonical_capability` validation with the reader; all three audit tests pass. |
| Opaque secret-reference boundary | `secret_store_boundary_resolves_only_opaque_references` | With blank-reference validation temporarily removed, `SecretRef::new("  ").is_err()` failed. | The restored boundary accepts/returns only `SecretRef`, redacts `Debug`, and rejects blank references. |

The new production correction is deliberately narrow: audit writes now reject
an unknown capability before persistence, so no durable audit row can be
accepted by the writer and rejected by the reader later. The remaining
changes after resumption are strict-Clippy corrections: code-identifier
documentation markup and small extraction of the aggregate dispatcher and
test setup helpers.

## Verification

Fresh final commands, all exit 0:

- `cargo fmt`
- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p cortex-storage --test migrations --test repositories --test operations --test audit` — 8 tests passed.
- `cargo test -p cortex-storage` — 8 integration tests and doc tests passed.
- `cargo test --workspace` — 39 integration tests and all doc tests passed.
- `git diff --check`

## Self-review

- `SqliteDatabase::connect_and_migrate` configures WAL and foreign-key
  enforcement before applying ordered SQLx migrations.
- The migration creates every Task 5 table with workspace scoping, integrity
  checks, foreign keys, unique durable `(workspace_id, operation_id)` outcomes,
  and append-only audit triggers.
- `OperationStore::execute_once` checks durable replay first; for new work it
  reserves the operation, applies every aggregate change, appends redacted
  audit evidence, and commits only once. Any `?` error drops/rolls back the
  transaction, including the operation reservation.
- Storage reads rebuild `Note`, `Task`, `Source`, and `MemoryAssertion` only
  through domain validation; malformed durable IDs, revisions, enum values,
  timestamps, or aggregate fields fail safely.
- Audit rows contain identifiers, capability, decision/result, correlation,
  target, and an empty redacted-metadata object only. The schema has no
  content/payload/prompt/secret fields, update/delete triggers reject mutation,
  and canonical capability validation is symmetric on write/read.
- `SecretStore` uses `SecretRef` for both input and output. It does not expose
  any credential value type.
- No changes were made to application service implementation, command DTOs,
  search, agent, daemon, CLI, MCP, or Task 6+ paths.

## Commit

Initial Task 5 implementation: `111ed94 feat(storage): add SQLite state, audit, and idempotency`.

The review fix-round commit follows after final staging.

## Concerns

No code-level concerns remain for Task 5. The host sometimes blocks a newly
rebuilt temporary test executable with Windows application-control `os error
4551`; the restored final source passed all required final commands. Git also
emits a non-fatal warning that the user-level global ignore file is
inaccessible. Neither affected the final test or diff results.

## Fix round 1/5: review-boundary and rollback corrections

The review fixes preserve the application-owned `AtomicMutationPort` and make
its storage ownership enforceable from downstream crates:

- `SqliteDatabase` no longer publishes `SqlitePool`. Only crate-internal test
  modules can use a `#[cfg(test)] pub(crate)` diagnostic accessor; external
  integration tests exercise typed adapters only.
- Raw migration pragma/schema/foreign-key checks now live in
  `database.rs` unit tests, and the raw audit trigger/schema check lives in an
  `audit.rs` unit test. Public integration tests cannot execute caller-supplied
  SQL.
- `SecretRef` and `SecretStore` moved to `cortex-application`, retaining the
  same reference validation and redacted `Debug` implementation without adding
  a secret value type. Storage consumes the inward application contract only.
- Global test convenience counters were removed. `SqliteAuditPort::find` now
  requires `WorkspaceId` and filters on both `workspace_id` and `id`; operation
  tests assert typed aggregate/audit lookups instead of global database counts.
- `audit_insert_failure_rolls_back_entity_audit_and_operation` uses a valid
  note mutation with an audit event whose non-canonical capability is rejected
  at audit insertion. It proves the note and audit are absent after failure;
  reusing the same operation ID with a valid audit event succeeds, proving the
  operation reservation also rolled back.
- `concurrent_duplicate_operations_return_one_durable_result` starts two Tokio
  tasks with the same operation ID, observes identical results, and verifies
  that exactly the winning note exists. It passed three consecutive focused
  runs.

### Fix-round red/green evidence

- Revised external audit test was first red with unresolved
  `cortex_application::SecretRef` and `SecretStore`; it passed after the port
  moved inward.
- Removing the public count APIs made the old operation test red with six
  missing-method errors; typed workspace-scoped assertions replaced those
  checks and the suite passed.
- For the inherited audit-failure behavior, temporarily removing
  `canonical_capability` validation made
  `audit_insert_failure_rolls_back_entity_audit_and_operation` fail because
  the mutation incorrectly succeeded. The exact write-side validation was
  restored before final verification.
- The concurrent duplicate test passed three consecutive focused runs after it
  was added.

### Fix-round verification limitation

`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace --no-run`, and `git diff --check` passed after the
fixes. The focused audit and migration integration tests executed successfully
in the final-gate attempt, but Windows application control then blocked the
newly rebuilt `operations` test binary and subsequent workspace test binaries
before execution with `os error 4551`. Retrying produced the same pre-execution
policy error. Earlier in this fix round, the complete storage suite passed
before the final concurrent test assertion was tightened, and the concurrent
test itself passed three consecutive runs. No source or test assertion failure
was observed after the final changes; runtime execution of the final full
suite remains pending an approved Code Integrity policy, signed toolchain, or
CI runner.
