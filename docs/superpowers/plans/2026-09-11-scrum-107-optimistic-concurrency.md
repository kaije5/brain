# SCRUM-107 Observed Revisions and Optimistic Concurrency Implementation Plan

**Goal:** Give the vault provider its revision scheme — content-derived observed revisions with SHA-256 content hashes — and the optimistic-concurrency primitives that SCRUM-108's atomic mutations compose: verify-before-write with explicit conflict data sufficient to re-read, and no blind last-writer-wins anywhere.

**Architecture:** The observed revision of a file is `rev-<first 16 hex bytes of the SHA-256 content digest>` — opaque, bounded, and deterministic, so identical content yields identical revisions and any external edit yields a different one. New provider primitives: `read_confined` (bytes + revision + hash in one pass) and `verify_revision(expected)` which re-reads the current file and returns `ProviderError::Conflict { current }` carrying the freshly observed provenance for reconciliation. Reads detect a file changing mid-read by re-verifying once. `Storage plan §7` optimistic-concurrency steps 2–6 are covered by these primitives; step 7 (audit) is SCRUM-109.

**Spec:** `docs/vault/markdown-vault-format.md`; SCRUM-90 design ("Error and degraded-state behavior")

## Global Constraints

- Conflict errors carry the current observed provenance only — never file contents or paths.
- No blind overwrite: mutation authors (SCRUM-108) must call `verify_revision`; the primitive is the single enforcement point.
- No test requires Obsidian, sync, network, or the user's vault.
- Before PR: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, `cargo test --workspace`, `git diff --check`.

## Tasks

- [ ] Task 1: `read_confined` + `current_revision` + `verify_revision` primitives with explicit conflict data.
- [ ] Task 2: Mid-read change detection in reads (bounded single re-verify).
- [ ] Task 3: Tests — identical content identical revision, external change → conflict with current provenance, re-read reconcile, verify-then-change race detection, unchanged files pass verification.
