# SCRUM-110 Filesystem and Concurrency Adversarial Hardening Implementation Plan

**Goal:** Prove and harden the vault provider against adversarial filesystem and concurrency conditions: rename-overwrite races, partial writes, permission failures, external concurrent edits, sync-conflict copies, and readers observing writes.

**Architecture:** Two hardening fixes plus a dedicated adversarial suite. (1) `rename` refuses to overwrite an existing target (`VaultPathError::TargetExists`) — a rename must never clobber another writer's file. (2) Enumeration ignores Brain temporary files (`*.tmp-*`) so a crashed write's leftovers never enter the vault model. The adversarial suite then covers: concurrent update races (one winner, one conflict), concurrent create of the same slug (unique allocation), external delete between read and write (typed NotFound, no resurrection), permission failures (unix read-only, original intact), simulated crashed write (temp leftover ignored, authoritative file intact), sync-conflict duplicate brain_id copies (typed DuplicateIdentity via `assert_unique_identities`), and readers-never-observe-partial-writes under concurrent read/write loops (tokio).

**Spec:** SCRUM-89 acceptance ("Tests cover create/update/delete/rename races, permission failures, partial writes and external concurrent edits"); storage plan §7.1

## Global Constraints

- Every failure is typed and value-free; originals stay intact on any fault.
- No test requires Obsidian, sync, network, or the user's vault.
- Before PR: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, `cargo test --workspace`, `git diff --check`.

## Tasks

- [ ] Task 1: `VaultPathError::TargetExists` + rename-overwrite guard; enumeration skips `*.tmp-*`.
- [ ] Task 2: Adversarial suite `vault_provider_adversarial.rs` covering the SCRUM-89 matrix.
