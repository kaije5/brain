# SCRUM-114 Periodic Reconciliation and Full Index Rebuild Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Recover missed vault filesystem events through deterministic periodic reconciliation and provide an explicit full rebuild that reconstructs equivalent searchable derived state from authoritative Markdown.

**Architecture:** Extend the existing pure vault-watcher layer with a `VaultReconciler` whose clock input is caller-supplied milliseconds. A provider-owned deterministic scan returns confined Markdown paths in stable order; reconciliation feeds each live path through the existing bounded/hash-gated event application and removes indexed resources no longer present, while full rebuild constructs a replacement `DerivedVaultIndex` off to the side before swapping it into use.

**Tech Stack:** Rust, `cortexd`, `cortex-search`, `cortex-vault`, `tempfile`, Cargo Nextest

**Spec:** [SCRUM-114](https://kaijehenstra.atlassian.net/browse/SCRUM-114), [Brain v0.2 Obsidian Knowledge and Task Storage Plan](https://kaijehenstra.atlassian.net/wiki/spaces/SCRUM/pages/1376257), and parent story [SCRUM-92](https://kaijehenstra.atlassian.net/browse/SCRUM-92)

## Global Constraints

- Work inline in `D:\brain` on `feat/scrum-114-periodic-reconciliation`; do not create a linked worktree or dispatch subagents.
- Treat vault content and paths as untrusted input; every indexed path must pass the existing confinement, exclusion, UTF-8, size, and Markdown parsing gates.
- Keep time deterministic: callers supply millisecond timestamps; tests do not sleep or start background threads.
- Reconciliation must converge after missed, reordered, or duplicated filesystem events.
- Full rebuild must construct replacement state before swapping it into the caller-owned index.
- Do not require Obsidian desktop, a sync subscription, or network access.
- Preserve the pre-existing uncommitted `AGENTS.md` change and exclude it from commits.
- Before PR run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, `cargo nextest run --workspace --all-targets --locked`, and `git diff --check`.

---

### Task 1: Deterministic confined vault scan

**Files:**
- Modify: `apps/cortexd/src/vault_provider.rs`
- Test: `apps/cortexd/tests/vault_watcher.rs`

**Interfaces:**
- Consumes: `MarkdownVaultProvider::enumerate_paths`, `VaultProviderConfig::allows_kind`, and the existing confinement/exclusion rules.
- Produces: `MarkdownVaultProvider::indexable_paths(&self) -> Vec<String>`, returning normalized `.md` paths in lexical order with task paths included once and excluded/unconfined paths omitted.

- [ ] **Step 1: Write the failing deterministic-scan test**

  Add a test that creates `z.md`, `a.md`, `Tasks/<brain-id>.md`, a non-Markdown file, and an excluded Markdown file; assert the literal result is `a.md`, the task path, then `z.md`, with no duplicates.

- [ ] **Step 2: Run the focused test and verify RED**

  Run `cargo nextest run -p cortexd --test vault_watcher deterministic_scan_lists_each_confined_markdown_path_once_in_lexical_order` and confirm compilation fails because `indexable_paths` does not exist.

- [ ] **Step 3: Implement the minimal scan API**

  Enumerate allowed knowledge paths and allowed task paths with the existing hard limit, combine them, sort, and deduplicate. Do not expose absolute paths.

- [ ] **Step 4: Run the focused test and verify GREEN**

  Re-run the exact focused command and require one passing test.

### Task 2: Reconcile live files and stale derived resources

**Files:**
- Modify: `apps/cortexd/src/vault_watcher.rs`
- Modify: `apps/cortexd/src/lib.rs`
- Test: `apps/cortexd/tests/vault_watcher.rs`

**Interfaces:**
- Consumes: `MarkdownVaultProvider::indexable_paths`, `apply_event`, and `DerivedVaultIndex::{documents,remove_resource}`.
- Produces: `ReconciliationReport { indexed, unchanged, removed, skipped }` and `reconcile_vault(provider: &MarkdownVaultProvider, index: &mut DerivedVaultIndex) -> ReconciliationReport`.

- [ ] **Step 1: Write failing convergence tests**

  Add real-filesystem tests proving that a scan recovers an unobserved addition and update, removes an indexed resource whose file disappeared or moved, leaves unchanged content untouched, and ignores a foreign provider/workspace resource already present in the same index.

- [ ] **Step 2: Run the reconciliation tests and verify RED**

  Run `cargo nextest run -p cortexd --test vault_watcher reconciliation` and confirm failure because the reconciliation API is absent.

- [ ] **Step 3: Implement minimal reconciliation**

  Build the live owned-resource set while applying a synthetic `Updated` event to each sorted path. Then snapshot only indexed resources owned by the provider's workspace/provider identity and remove those absent from the live set. Count outcomes in the report; do not remove foreign derived state.

- [ ] **Step 4: Run the reconciliation tests and verify GREEN**

  Re-run the focused command and require all matching tests to pass.

### Task 3: Periodic coordinator and atomic full rebuild

**Files:**
- Modify: `apps/cortexd/src/vault_watcher.rs`
- Modify: `apps/cortexd/src/lib.rs`
- Test: `apps/cortexd/tests/vault_watcher.rs`

**Interfaces:**
- Consumes: `reconcile_vault` and `DerivedVaultIndex` ownership.
- Produces: `VaultReconciler::new(interval_millis: NonZeroU64)`, `VaultReconciler::reconcile_if_due(provider, index, now_millis) -> Option<ReconciliationReport>`, and `rebuild_vault(provider, index) -> ReconciliationReport`.

- [ ] **Step 1: Write failing schedule and rebuild tests**

  Add tests proving the first periodic call runs, calls strictly before the interval do not run, the boundary call runs, and an explicit rebuild discards stale/foreign derived data and recreates the same ordered document/chunk snapshots as a clean rebuild from the same vault.

- [ ] **Step 2: Run the focused tests and verify RED**

  Run `cargo nextest run -p cortexd --test vault_watcher periodic` and `cargo nextest run -p cortexd --test vault_watcher rebuild`; confirm both fail because the APIs are absent.

- [ ] **Step 3: Implement the minimal coordinator and rebuild operation**

  Store only the interval and last-run timestamp. Use saturating elapsed-time comparison and update the watermark only when a run occurs. For rebuild, reconcile into a new empty index and assign it to the caller's index after the scan finishes.

- [ ] **Step 4: Run focused and package verification**

  Run `cargo nextest run -p cortexd --test vault_watcher` and `cargo clippy -p cortexd --all-targets --locked`; require zero failures and zero warnings.

### Task 4: Repository verification and delivery evidence

**Files:**
- Modify: `docs/superpowers/plans/2026-09-15-scrum-114-periodic-reconciliation.md` only to check completed boxes if useful.

**Interfaces:**
- Consumes: all implementation and tests above.
- Produces: a conventional commit and PR evidence tied to SCRUM-114.

- [ ] **Step 1: Run all required gates**

  Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, `cargo nextest run --workspace --all-targets --locked`, and `git diff --check` from the feature branch.

- [ ] **Step 2: Review the exact diff and status**

  Run `git diff -- apps/cortexd/src/vault_provider.rs apps/cortexd/src/vault_watcher.rs apps/cortexd/src/lib.rs apps/cortexd/tests/vault_watcher.rs docs/superpowers/plans/2026-09-15-scrum-114-periodic-reconciliation.md` and `git status --short --branch`. Confirm `AGENTS.md` is not staged.

- [ ] **Step 3: Commit and open the PR**

  Commit the scoped files with subject `feat: add deterministic vault reconciliation` and body `SCRUM-114`, push `feat/scrum-114-periodic-reconciliation`, and open a PR to `main` describing acceptance coverage and exact verification commands.

- [ ] **Step 4: Track CI and Jira truthfully**

  Wait for required PR checks. Move SCRUM-114 to `Gereed` only after the PR is open with green CI or merged, and add a Jira comment with changed paths, focused verification, commit/PR evidence, and final Git status.
