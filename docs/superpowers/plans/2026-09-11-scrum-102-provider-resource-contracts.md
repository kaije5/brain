# SCRUM-102 Provider Resource Contracts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Introduce validated provider resource identity/provenance types and complete application-owned knowledge/task provider ports that a filesystem-free fake can implement.

**Architecture:** `cortex-domain` owns only provider-neutral identity, revision, hash, resource-kind, and provenance values. `cortex-application` owns bounded knowledge/task DTOs, typed provider outcomes/errors, and separate asynchronous read/write ports; contract tests implement both ports with an in-memory fake and exercise real state transitions. Existing `Note`, `Task`, `NoteRepository`, and `TaskRepository` remain untouched and are not adapted.

**Tech Stack:** Rust 2024, Rust 1.97, `chrono`, `uuid`, native async trait methods, Tokio tests.

**Spec:** `docs/superpowers/specs/2026-09-11-scrum-90-provider-contracts-design.md`

## Global Constraints

- First-party Rust preserves `#![forbid(unsafe_code)]`; production code avoids `unwrap()` / `expect()`.
- Obsidian, iCloud, Obsidian Sync, Obsidian Headless, filesystem paths, and transport metadata stay outside `cortex-domain` and public `cortex-application` contracts.
- The new ports do not wrap, extend, or implement the legacy note/task repositories.
- Provider identifiers, resource identifiers, revisions, query text, DTO strings, and returned collections are bounded before application use.
- Existing-resource mutations carry an expected observed revision and return either new provenance or an explicit conflict.
- Public errors and `Debug` output never contain user content, local paths, credentials, or raw provider diagnostics.
- No test requires a filesystem, Obsidian, a sync subscription, network access, or the user's vault.
- Before PR creation run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, `cargo test --workspace`, and `git diff --check`.

---

### Task 1: Validated provider identities and provenance

**Files:**
- Create: `crates/cortex-domain/src/provider.rs`
- Modify: `crates/cortex-domain/src/ids.rs`
- Modify: `crates/cortex-domain/src/lib.rs`
- Create: `crates/cortex-domain/tests/provider_resources.rs`

**Interfaces:**
- Consumes: `DomainError`, `WorkspaceId`, and the existing UUIDv7 ID macro.
- Produces: `TaskId`, `ProviderId`, `ProviderResourceId`, `ProviderResourceKind`, `ProviderResourceRef`, `ObservedRevision`, `ContentHash`, and `ProviderProvenance`.

- [ ] **Step 1: Write failing identity-validation tests**

Create `crates/cortex-domain/tests/provider_resources.rs` with literal expectations:

```rust
use cortex_domain::{
    ContentHash, DomainError, ObservedRevision, ProviderId, ProviderProvenance,
    ProviderResourceId, ProviderResourceKind, ProviderResourceRef, TaskId, WorkspaceId,
};
use uuid::Uuid;

#[test]
fn provider_identifiers_reject_blank_oversized_and_control_input() {
    assert!(matches!(ProviderId::new(" "), Err(DomainError::Validation { field: "provider_id", .. })));
    assert!(matches!(ProviderResourceId::new("x".repeat(513)), Err(DomainError::Validation { field: "provider_resource_id", .. })));
    assert!(matches!(ObservedRevision::new("rev\n2"), Err(DomainError::Validation { field: "observed_revision", .. })));
}

#[test]
fn provider_provenance_keeps_stable_identity_separate_from_revision_and_hash() {
    let resource = ProviderResourceRef::new(
        WorkspaceId::new(),
        ProviderId::new("primary-vault").unwrap(),
        ProviderResourceId::new("01K4RESOURCE").unwrap(),
        ProviderResourceKind::Task,
    );
    let provenance = ProviderProvenance::new(
        resource.clone(),
        ObservedRevision::new("rev-7").unwrap(),
        ContentHash::new([0x2a; 32]),
    );

    assert_eq!(provenance.resource(), &resource);
    assert_eq!(provenance.observed_revision().as_str(), "rev-7");
    assert_eq!(provenance.content_hash().as_bytes(), &[0x2a; 32]);
}

#[test]
fn task_ids_require_uuid_v7_when_rehydrated() {
    assert!(TaskId::try_from(Uuid::nil()).is_err());
}
```

The production change each test catches is respectively accepting an unsafe opaque value, collapsing stable identity into mutable revision/hash metadata, or accepting an invalid durable task identity.

- [ ] **Step 2: Run the domain test and verify RED**

Run: `cargo test -p cortex-domain --test provider_resources`

Expected: compilation fails because the provider types and `TaskId` do not exist.

- [ ] **Step 3: Implement the minimal domain types**

Add `domain_id!(TaskId);` in `ids.rs`. Create `provider.rs` with:

```rust
const MAX_PROVIDER_ID_BYTES: usize = 128;
const MAX_RESOURCE_ID_BYTES: usize = 512;
const MAX_REVISION_BYTES: usize = 512;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProviderId(String);

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProviderResourceId(String);

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObservedRevision(String);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentHash([u8; 32]);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ProviderResourceKind { Knowledge, Task }

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProviderResourceRef {
    workspace_id: WorkspaceId,
    provider_id: ProviderId,
    resource_id: ProviderResourceId,
    kind: ProviderResourceKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderProvenance {
    resource: ProviderResourceRef,
    observed_revision: ObservedRevision,
    content_hash: ContentHash,
}
```

Implement `new`/getter methods. String constructors reject trimmed-empty input, byte lengths above the named maximum, and control characters via `DomainError::validation` with fields `provider_id`, `provider_resource_id`, and `observed_revision`. Export every type from `lib.rs`.

- [ ] **Step 4: Run focused domain tests and verify GREEN**

Run: `cargo test -p cortex-domain --test provider_resources`

Expected: 3 tests pass with no warnings.

- [ ] **Step 5: Run existing domain tests**

Run: `cargo test -p cortex-domain`

Expected: all domain tests pass.

- [ ] **Step 6: Commit Task 1**

```powershell
git add crates/cortex-domain/src/provider.rs crates/cortex-domain/src/ids.rs crates/cortex-domain/src/lib.rs crates/cortex-domain/tests/provider_resources.rs
git commit -m "feat: add provider resource identities" -m "SCRUM-102"
```

---

### Task 2: Bounded provider DTOs and typed outcomes

**Files:**
- Create: `crates/cortex-application/src/provider.rs`
- Create: `crates/cortex-application/src/knowledge.rs`
- Create: `crates/cortex-application/src/task_provider.rs`
- Modify: `crates/cortex-application/src/lib.rs`
- Create: `crates/cortex-application/tests/provider_dtos.rs`

**Interfaces:**
- Consumes: Task 1 provider types plus existing `OperationId` and `WorkspaceId`.
- Produces: `ProviderFreshness`, `ProviderRead<T>`, `ProviderPage<T>`, `ProviderMutation`, `ProviderError`, `KnowledgeDocument`, knowledge mutation/query DTOs, `ProviderTask`, task mutation/query DTOs, `ProviderTaskStatus`, `ProviderTaskPriority`, and `TaskSchedulingMetadata`.

- [ ] **Step 1: Write failing bounded-DTO and safe-error tests**

Create `crates/cortex-application/tests/provider_dtos.rs` covering one behavior per test:

```rust
use std::num::NonZeroUsize;

use cortex_application::{
    KnowledgeQuery, ProviderError, ProviderFreshness, ProviderPage, ProviderRead,
};
use cortex_domain::WorkspaceId;

#[test]
fn provider_queries_reject_blank_oversized_and_excessive_limits() {
    assert!(KnowledgeQuery::new(WorkspaceId::new(), " ", NonZeroUsize::new(1).unwrap()).is_err());
    assert!(KnowledgeQuery::new(WorkspaceId::new(), "x".repeat(4097), NonZeroUsize::new(1).unwrap()).is_err());
    assert!(KnowledgeQuery::new(WorkspaceId::new(), "safe", NonZeroUsize::new(101).unwrap()).is_err());
}

#[test]
fn provider_pages_reject_more_items_than_the_contract_limit() {
    assert!(ProviderPage::new(Vec::<u8>::from_iter(0..101), ProviderFreshness::Current).is_err());
}

#[test]
fn provider_errors_are_redacted_and_typed() {
    assert_eq!(format!("{:?}", ProviderError::Internal), "Internal");
    assert_eq!(ProviderRead::new(7_u8, ProviderFreshness::Stale).freshness(), ProviderFreshness::Stale);
}
```

Add construction tests using literal values for `KnowledgeDocument`, `ProviderTask`, expected revisions on update/delete/complete inputs, and `ProviderMutation` carrying previous/current provenance. Assert create has no previous provenance, update has both previous and current provenance, and delete has previous but no current provenance. Assert task scheduling metadata round-trips `due_at`, `deadline_at`, non-zero duration, `earliest_start`, split preference, project, and context without introducing Markdown/frontmatter or path fields.

The production changes caught are removal of query/result bounds, loss of explicit freshness, unsafe error payloads, omission of expected revisions, or leakage of representation-specific fields.

- [ ] **Step 2: Run DTO tests and verify RED**

Run: `cargo test -p cortex-application --test provider_dtos`

Expected: compilation fails because the DTO/outcome modules do not exist.

- [ ] **Step 3: Implement shared provider outcomes**

Create `provider.rs` with exact shared shapes:

```rust
pub const MAX_PROVIDER_RESULTS: usize = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderFreshness { Current, Stale }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderRead<T> { item: T, freshness: ProviderFreshness }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderPage<T> { items: Vec<T>, freshness: ProviderFreshness }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderMutation {
    resource: ProviderResourceRef,
    previous: Option<ProviderProvenance>,
    current: Option<ProviderProvenance>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderError {
    Validation { field: &'static str },
    NotFound { resource: ProviderResourceRef },
    Conflict { current: ProviderProvenance },
    Unavailable,
    Internal,
}
```

Implement constructors/getters. `ProviderMutation::created`, `ProviderMutation::updated`, and `ProviderMutation::deleted` enforce the three valid previous/current shapes and derive the resource reference from provenance. `ProviderPage::new` rejects more than 100 items with `ProviderError::Validation { field: "provider_results" }`. Keep `ProviderError` payloads typed and content-free.

- [ ] **Step 4: Implement knowledge DTOs**

Create `knowledge.rs` with `MAX_PROVIDER_TEXT_BYTES = 1_048_576`, `MAX_PROVIDER_QUERY_BYTES = 4096`, public DTO structs with private fields/getters, and constructors that validate title/body/query bounds before dispatch:

```rust
pub struct KnowledgeDocument { provenance: ProviderProvenance, title: String, body: String }
pub struct KnowledgeQuery { workspace_id: WorkspaceId, text: String, limit: NonZeroUsize }
pub struct KnowledgeCreate { workspace_id: WorkspaceId, operation_id: OperationId, title: String, body: String }
pub struct KnowledgeUpdate { resource: ProviderResourceRef, operation_id: OperationId, expected_revision: ObservedRevision, title: String, body: String }
pub struct KnowledgeDelete { resource: ProviderResourceRef, operation_id: OperationId, expected_revision: ObservedRevision }
```

Reject wrong resource kinds in update/delete constructors with `ProviderError::Validation { field: "resource_kind" }`. Queries reject blank/control/oversized text and limits above 100. Titles reject blank/control/oversized input; bodies may be empty and contain ordinary newlines but reject oversized input.

- [ ] **Step 5: Implement task DTOs**

Create `task_provider.rs` with:

```rust
pub enum ProviderTaskStatus { Todo, InProgress, Completed, Cancelled }
pub enum ProviderTaskPriority { Low, Normal, High, Urgent }
pub struct TaskSchedulingMetadata {
    due_at: Option<DateTime<Utc>>,
    deadline_at: Option<DateTime<Utc>>,
    duration_minutes: Option<NonZeroU32>,
    earliest_start: Option<DateTime<Utc>>,
    split: Option<bool>,
    project: Option<String>,
    context: Option<String>,
}
pub struct ProviderTask { provenance: ProviderProvenance, task_id: TaskId, title: String, body: String, status: ProviderTaskStatus, priority: ProviderTaskPriority, scheduling: TaskSchedulingMetadata }
pub struct TaskQuery { workspace_id: WorkspaceId, text: Option<String>, limit: NonZeroUsize }
pub struct TaskCreate { workspace_id: WorkspaceId, operation_id: OperationId, task_id: TaskId, title: String, body: String, priority: ProviderTaskPriority, scheduling: TaskSchedulingMetadata }
pub struct TaskUpdate { resource: ProviderResourceRef, operation_id: OperationId, expected_revision: ObservedRevision, title: String, body: String, status: ProviderTaskStatus, priority: ProviderTaskPriority, scheduling: TaskSchedulingMetadata }
pub struct TaskComplete { resource: ProviderResourceRef, operation_id: OperationId, expected_revision: ObservedRevision }
pub struct TaskDelete { resource: ProviderResourceRef, operation_id: OperationId, expected_revision: ObservedRevision }
```

Use the same title/body/query/result bounds, including support for an empty task body. Project/context accept `None` or non-blank/control-free strings up to 256 bytes. Reject wrong resource kinds in update/complete/delete constructors. Do not define Markdown keys, filenames, paths, or sync metadata.

- [ ] **Step 6: Export DTOs and run focused tests GREEN**

Export the shared, knowledge, and task types from `lib.rs`.

Run: `cargo test -p cortex-application --test provider_dtos`

Expected: every DTO, bound, freshness, mutation-provenance, and redacted-error test passes.

- [ ] **Step 7: Run application tests**

Run: `cargo test -p cortex-application`

Expected: all application tests pass.

- [ ] **Step 8: Commit Task 2**

```powershell
git add crates/cortex-application/src/provider.rs crates/cortex-application/src/knowledge.rs crates/cortex-application/src/task_provider.rs crates/cortex-application/src/lib.rs crates/cortex-application/tests/provider_dtos.rs
git commit -m "feat: define provider knowledge and task DTOs" -m "SCRUM-102"
```

---

### Task 3: Complete provider ports proven by an in-memory fake

**Files:**
- Modify: `crates/cortex-application/src/knowledge.rs`
- Modify: `crates/cortex-application/src/task_provider.rs`
- Create: `crates/cortex-application/tests/provider_contracts.rs`

**Interfaces:**
- Consumes: all Task 1 and Task 2 resource/DTO/outcome types.
- Produces: complete `KnowledgeProvider` and `TaskProvider` traits with substitutability proven by a stateful fake.

- [ ] **Step 1: Write the failing fake-provider contract tests**

Create `provider_contracts.rs`. Define `FakeProvider` only in this test using `tokio::sync::Mutex` or `std::sync::Mutex` over maps keyed by `ProviderResourceRef`; implement no production test helpers.

Write separate Tokio tests that exercise the desired port through generic functions:

```rust
async fn read_knowledge<P: KnowledgeProvider>(provider: &P, resource: &ProviderResourceRef) -> Result<Option<ProviderRead<KnowledgeDocument>>, ProviderError> {
    provider.get(resource).await
}

async fn read_task<P: TaskProvider>(provider: &P, resource: &ProviderResourceRef) -> Result<Option<ProviderRead<ProviderTask>>, ProviderError> {
    provider.get(resource).await
}
```

Contract cases:

1. knowledge create returns a created mutation with current provenance, then get and bounded search return the same document;
2. knowledge update with the returned revision changes content and returns previous/current revision/hash evidence;
3. stale knowledge update/delete returns `ProviderError::Conflict { current }` and preserves the current document; successful delete returns previous provenance and no current provenance;
4. task create/get/list/update/complete/delete cover the complete task port, preserve `TaskId` across mutations, and return the correct created/updated/deleted mutation shape;
5. stale task update/complete/delete returns conflict without changing state;
6. a fake marked unavailable returns `ProviderError::Unavailable` for reads and mutations;
7. knowledge/task resources of the wrong kind are rejected before state access;
8. searches and lists never return more than the requested limit and carry explicit freshness.

Expected values must be literals or state read back through the real trait methods; do not assert on mock call counts.

- [ ] **Step 2: Run contract tests and verify RED**

Run: `cargo test -p cortex-application --test provider_contracts`

Expected: compilation fails because `KnowledgeProvider` and `TaskProvider` do not exist.

- [ ] **Step 3: Define the knowledge port**

Append this exact port surface to `knowledge.rs`:

```rust
#[allow(async_fn_in_trait)]
pub trait KnowledgeProvider: Send + Sync {
    async fn get(&self, resource: &ProviderResourceRef) -> Result<Option<ProviderRead<KnowledgeDocument>>, ProviderError>;
    async fn search(&self, query: &KnowledgeQuery) -> Result<ProviderPage<KnowledgeDocument>, ProviderError>;
    async fn create(&self, input: KnowledgeCreate) -> Result<ProviderMutation, ProviderError>;
    async fn update(&self, input: KnowledgeUpdate) -> Result<ProviderMutation, ProviderError>;
    async fn delete(&self, input: KnowledgeDelete) -> Result<ProviderMutation, ProviderError>;
}
```

- [ ] **Step 4: Define the task port**

Append this exact port surface to `task_provider.rs`:

```rust
#[allow(async_fn_in_trait)]
pub trait TaskProvider: Send + Sync {
    async fn get(&self, resource: &ProviderResourceRef) -> Result<Option<ProviderRead<ProviderTask>>, ProviderError>;
    async fn search(&self, query: &TaskQuery) -> Result<ProviderPage<ProviderTask>, ProviderError>;
    async fn create(&self, input: TaskCreate) -> Result<ProviderMutation, ProviderError>;
    async fn update(&self, input: TaskUpdate) -> Result<ProviderMutation, ProviderError>;
    async fn complete(&self, input: TaskComplete) -> Result<ProviderMutation, ProviderError>;
    async fn delete(&self, input: TaskDelete) -> Result<ProviderMutation, ProviderError>;
}
```

Export both traits from `lib.rs`.

- [ ] **Step 5: Implement the minimal fake and verify GREEN**

Implement the in-test fake with deterministic resource IDs, revisions, and hashes. Each successful mutation increments a numeric revision string; stale expected revisions return the current typed provenance; deletes remove the resource only after revision validation. Search/list filter by workspace and query, sort by resource ID, truncate to the requested limit, and preserve configured `ProviderFreshness`.

Run: `cargo test -p cortex-application --test provider_contracts`

Expected: all eight contract behaviors pass with no warnings.

- [ ] **Step 6: Mutation-check the contract tests**

Temporarily change the fake's revision comparison to always accept stale values and run:

`cargo test -p cortex-application --test provider_contracts stale`

Expected: stale mutation tests fail. Restore the correct comparison and rerun the same command; expected: pass.

- [ ] **Step 7: Run package and story gates**

Run in order:

```powershell
cargo fmt --all --check
cargo clippy -p cortex-domain -p cortex-application --all-targets --locked
cargo test -p cortex-domain
cargo test -p cortex-application
git diff --check
```

Expected: every command exits 0 with no warnings.

- [ ] **Step 8: Commit Task 3**

```powershell
git add crates/cortex-application/src/knowledge.rs crates/cortex-application/src/task_provider.rs crates/cortex-application/src/lib.rs crates/cortex-application/tests/provider_contracts.rs
git commit -m "feat: add replaceable knowledge and task ports" -m "SCRUM-102"
```

---

### Task 4: Final verification, review, and PR evidence

**Files:**
- Modify only if review finds a concrete defect in the files above.

**Interfaces:**
- Consumes: completed SCRUM-102 implementation and commits.
- Produces: reviewed, verified branch and a PR against `main` with Jira evidence.

- [ ] **Step 1: Verify no forbidden coupling or legacy adaptation**

Run:

```powershell
rg -n -i "obsidian|icloud|headless|sync|std::path|pathbuf|note(repository)?|taskrepository" crates/cortex-domain/src/provider.rs crates/cortex-application/src/provider.rs crates/cortex-application/src/knowledge.rs crates/cortex-application/src/task_provider.rs
```

Expected: no matches for vendor/sync/path types and no legacy repository references. Type names such as `ProviderTask` are allowed; inspect every reported line rather than accepting a raw count.

- [ ] **Step 2: Run full repository gates**

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked
cargo test --workspace
git diff --check
git status --short --branch
```

Expected: all gates exit 0 and the branch is clean.

- [ ] **Step 3: Request independent review**

Review `main..HEAD` against SCRUM-102, the merged design spec, and this plan. Repair all Critical and Important findings with a failing regression test first, then rerun focused and full gates.

- [ ] **Step 4: Push and create the PR**

Push `feat/scrum-102-provider-resource-contracts` and create a PR against `main`. The description lists changed paths, TDD red/green evidence, focused/full verification, review outcome, TUI non-impact, and Jira keys SCRUM-90/SCRUM-102.

- [ ] **Step 5: Wait for green CI and update Jira**

Wait for every required GitHub check. When green, add commit/PR/verification evidence to SCRUM-102 and transition it to Done. Keep SCRUM-90 active for its remaining subtasks and preserve the worktree until the user merges the PR.
