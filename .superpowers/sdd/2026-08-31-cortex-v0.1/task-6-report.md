# Task 6 report: transactional application commands and queries

## Status

Implemented Task 6 only on `codex/cortex-v0.1`. The application layer now
provides the twelve planned note, task, and memory command methods, typed
client-safe inputs, policy enforcement, successful-operation idempotency,
optimistic revision checks, tombstone lifecycle handling, explicit history
reads, provenance validation, immutable memory correction, and redacted audit
evidence. Successful aggregate changes, their canonical result, operation
outcome, and audit event continue to commit through the application-owned
`AtomicMutationPort` boundary.

No Task 7 search, embedding, FTS, inference, daemon, CLI, MCP, or other later
task behavior was added.

## Inherited-work inspection

This task resumed an interrupted, uncommitted Task 6 state. Before further
changes, the linked worktree was on `codex/cortex-v0.1` at `f198387`, with
modified application command/query/service files, storage operation and
repository adapters, storage repository tests, dependency metadata, and five
new application test paths (including the shared support fixture). All of
those inherited files were preserved.

The inherited focused command compiled successfully, but its first runtime
attempt was stopped before test execution by Windows application control
(`os error 4551`). No initial-red transcript or implementer report was left for
the inherited tests, so this report does not claim one. Subsequent individual
test-binary retries executed all Task 6-focused tests successfully.

## Changed paths

- `Cargo.lock`
- `crates/cortex-application/Cargo.toml`
- `crates/cortex-application/src/command.rs`
- `crates/cortex-application/src/lib.rs`
- `crates/cortex-application/src/query.rs`
- `crates/cortex-application/src/service.rs`
- `crates/cortex-application/tests/memories.rs`
- `crates/cortex-application/tests/notes.rs`
- `crates/cortex-application/tests/revision_conflicts.rs`
- `crates/cortex-application/tests/support/mod.rs`
- `crates/cortex-application/tests/tasks.rs`
- `crates/cortex-storage/src/operation.rs`
- `crates/cortex-storage/src/repositories.rs`
- `crates/cortex-storage/tests/repositories.rs`
- `.superpowers/sdd/2026-08-31-cortex-v0.1/task-6-report.md`

## Implemented behavior

- `ApplicationService` exposes exact async methods `create_note`,
  `update_note`, `delete_note`, `restore_note`, `create_task`, `complete_task`,
  `delete_task`, `restore_task`, `create_memory`, `correct_memory`,
  `delete_memory`, and `restore_memory`.
- Every invocation evaluates the authenticated workspace/principal capability.
  A successful replay returns its recorded result before revalidating now-stale
  aggregate state, but it cannot bypass a capability that is currently denied.
- Completed operations retain their one canonical atomic audit row. A denied
  replay returns `PolicyDenied` without attempting a conflicting second audit
  insert for the same `(workspace_id, operation_id)`.
- Targeted mutations load explicit history within the authenticated workspace,
  validate the expected revision and lifecycle, and rely on a conditional
  storage update to close the read/commit race.
- Note and task delete/restore commands advance revisions and preserve their
  content/status. Default repositories return only active records; explicit
  `find_history` methods return tombstones.
- Default memory reads additionally omit superseded/forgotten assertions.
  Memory creation and correction require active provenance sources in the
  authenticated workspace.
- Memory correction atomically advances the predecessor to `Superseded`,
  inserts a new sourced successor linked by `supersedes`, records the operation
  outcome, and appends the successful redacted audit event.
- `OperationResultRepository` adds the read side needed for pre-validation
  replay while `AtomicMutationPort::execute_once` remains the authoritative
  race-safe transaction and idempotency boundary.

## Red/green evidence

The inherited tests had no retained initial-red evidence, and the first attempt
to execute them was blocked by WDAC after compilation. The following new
behavior was completed with witnessed runtime red/green cycles:

1. `repeated_operation_still_requires_the_current_capability` was added before
   the policy/idempotency ordering change. The focused command executed three
   tests and failed exactly at the new assertion because the replay returned
   its cached success before policy evaluation (`2 passed; 1 failed`). Moving
   policy evaluation ahead of replay return produced a green focused run
   (`3 passed; 0 failed`).
2. The same test was tightened before the final preflight adjustment to require
   one canonical audit row for a completed operation. It failed with
   `left: 2, right: 1`, proving that a denied replay attempted a second audit.
   Preflight now detects the existing outcome after evaluating policy and
   returns `PolicyDenied` without duplicating the operation audit. The complete
   revision/idempotency binary then passed all three tests.

No runtime result is inferred from a WDAC-blocked invocation. Compile-only
checks and runtime checks are listed separately below.

## Verification

Fresh static/compile evidence, all exit 0:

- `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test -p cortex-application --no-run` — all application unit and
  integration test executables compiled.
- `cargo test --workspace --no-run` — all workspace test executables compiled.
- `git diff --check`

Fresh Task 6 runtime evidence, all listed tests passed:

- `cargo test -p cortex-application --test memories` — 3 passed.
- `cargo test -p cortex-application --test notes` — 2 passed.
- `cargo test -p cortex-application --test tasks` — 1 passed.
- `cargo test -p cortex-application --test revision_conflicts` — 3 passed.
- `cargo test -p cortex-storage --test repositories` — 1 passed.
- `cargo test -p cortex-storage --test operations` — 4 passed.

The combined `cargo test -p cortex-application` gate is not recorded as green.
It executed the application unit target and both atomic mutation contract tests
successfully, then WDAC blocked the pre-existing `capability_contract` binary
before execution with `os error 4551`. A combined four-binary Task 6 command
also intermittently stopped at a blocked binary; running the same Task 6
binaries individually produced the passing evidence above.

## Self-review

- All twelve methods use the same preflight, error-audit, and atomic-commit
  orchestration rather than direct repository writes.
- Mutation success audit evidence contains only authenticated identifiers,
  capability, decision/result, target, operation, and correlation metadata;
  client payload text is never included.
- Ordinary note/task/memory repository reads filter deleted records, and
  ordinary memory reads also filter non-active semantic status. Lifecycle and
  correction commands use explicit history access.
- The SQLite adapter checks workspace, entity identity, and revision on every
  replacement; lifecycle updates are conditional on workspace/entity/revision.
- Memory source links are inserted inside the same transaction as the memory
  record, predecessor supersession, audit event, and operation outcome.
- No raw SQLx transaction or pool escapes into the application service, and no
  inherited Task 4/5 boundary was removed.
- No `TODO`, `FIXME`, debug print, panic, `unwrap`, or `expect` was introduced in
  the Task 6 paths.

## Commit

The Task 6 implementation and this report are committed together with message
`feat(application): add audited knowledge and task commands`. The exact commit
ID is reported by the implementing agent after the commit is created.

## Concerns

The code-level Task 6 checks found no remaining known defect. The environment
still intermittently blocks newly built Rust test executables with Windows
application-control `os error 4551`; therefore the monolithic package gate is
not runtime-green in this report even though every Task 6-focused binary passed
individually and all workspace targets compile under strict Clippy. Re-run the
full application/workspace runtime suites on an approved Code Integrity policy,
signed toolchain, or CI runner before release completion.

Git also emits a non-fatal warning that the user-level global ignore file is
inaccessible. It does not affect the repository diff or build/test results.

## Review fix round 1: replay binding and source lifecycle

The independent Task 6 review found two related defense-in-depth gaps. Durable
operation results were keyed only by workspace and operation ID, so a currently
granted principal or a different allowed command could receive another
command's result. Ordinary SQLite source lookup also returned tombstones, and
the application validation trusted every source returned by its repository.

The fix remains inside Task 6 and preserves the accepted atomic boundary:

- `OperationIdentity` binds every recorded result to the authenticated
  principal, typed `Capability`, and original command target (`None` for create,
  `Some(entity_id)` for targeted commands). `RecordedOperation` carries that
  identity with the canonical `MutationResult` across the application-owned
  read port.
- `ApplicationService::preflight` still evaluates current policy first, then
  returns a replay only when principal, capability, and target all match. A
  mismatch returns the fixed client-safe `Conflict { entity: "operation" }`.
- `OperationStore` persists a version-2 replay envelope containing principal,
  canonical capability name, target, and result. Both its initial replay path
  and reservation-race loser path call the same identity validator before
  returning a result.
- `AtomicMutation::new` requires the typed capability and command target and
  verifies the success audit uses the canonical capability name. The existing
  mutation/result/audit transaction boundary is otherwise unchanged.
- Ordinary SQLite `SourceRepository::find` now requires `lifecycle = 'active'`.
  `validate_sources` independently requires `Lifecycle::Active`, so a faulty or
  history-returning adapter cannot authorize tombstoned provenance.
- No Task 7 search, FTS, embedding, ranker, inference, daemon, CLI, or MCP work
  was introduced. No unsafe code or production `unwrap`/`expect` was added.

### Fix-round red evidence

Each regression was written and executed before its production fix:

1. `cargo test -p cortex-application --test memories` ran five tests and failed
   the two new tombstoned-source cases. Both failures returned `Ok(MutationResult
   { .. })` where `Err(NotFound { entity: "source" })` was required; the other
   three tests passed.
2. `cargo test -p cortex-application --test revision_conflicts` ran six tests
   and failed the three new granted-principal, allowed-capability, and target
   mismatch cases. Each returned the original `Ok(MutationResult { .. })`
   instead of `Err(Conflict { entity: "operation" })`; the existing three tests
   passed.
3. `cargo test -p cortex-storage --test operations` ran seven tests and failed
   the three durable principal/capability/target mismatch cases for the same
   reason. The existing four storage operation tests passed.
4. `cargo test -p cortex-storage --test repositories` failed its one test
   because ordinary source lookup returned `Some(Source { lifecycle: Deleted,
   .. })` instead of `None`.

### Fix-round green and gate evidence

Fresh successful checks after the fix:

- `cargo fmt --check` — PASS.
- `cargo clippy --workspace --all-targets -- -D warnings` — PASS.
- `cargo test --workspace --no-run` — PASS; every workspace unit and
  integration target compiled.
- `cargo test -p cortex-application --test atomic_mutation_contract` — 2 passed.
- `cargo test -p cortex-application --test memories` — 5 passed, including both
  tombstoned create/correct rejection cases with no mutation and one redacted
  rejected audit decision.
- `cargo test -p cortex-application --test revision_conflicts` — 6 passed,
  including different granted principal, different allowed capability, and
  different target conflicts.
- `cargo test -p cortex-application --test notes` — 2 passed.
- `cargo test -p cortex-application --test capability_contract` — 4 passed.
- `cargo test -p cortex-storage --test repositories` — 1 passed, including the
  active-only ordinary source lookup assertion.
- `cargo test -p cortex-storage --lib` — 3 passed.
- `git diff --check` — PASS (only the pre-existing inaccessible global-ignore
  warning was emitted).

The first post-fix `operations` execution reached runtime and passed all three
new durable replay mismatch tests plus three existing tests. Its only failure
was the audit-rollback test after that test had been changed to induce an audit
primary-key collision: the assertion incorrectly expected the deliberately
pre-seeded audit row to be absent. That test-only expectation was corrected to
require the pre-seeded row to remain intact. After this correction,
`cargo test -p cortex-storage --test operations` compiled but Windows
application control blocked the test executable before execution with `os error
4551`. A release-profile retry was also blocked while launching the `quote`
build script. Therefore the final seven-test operations binary is compile-green
but not claimed runtime-green in this fix round.

The combined `cargo test -p cortex-application` run passed the unit target,
atomic contract (2), capability contract (4), and memories (5), then WDAC
blocked the notes binary. Individual retries made notes green, while tasks and
policy were blocked before execution. These environmental blocks are reported
as blocks, not passing results.
