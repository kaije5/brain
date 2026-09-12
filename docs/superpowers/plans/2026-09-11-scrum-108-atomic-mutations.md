# SCRUM-108 Atomic Vault Mutations Implementation Plan

**Goal:** Replace the `Unavailable` mutation stubs with atomic create, update, complete, rename, and delete for both ports — every write gated by SCRUM-107's `verify_revision`, every file replacement through a same-filesystem temporary file plus atomic rename, so faults can never leave a partially written authoritative file.

**Architecture:** `write_atomic(confined, bytes)` writes `<name>.tmp-<uuid>` beside the target and renames over it (same filesystem by construction). Creates derive the path from the content: knowledge at `Documents/<slug>.md`, tasks at `Tasks/<brain_id>-<slug>.md` (spec §6.1), with `-2`/`-3` suffixes when a file already exists. Updates/deletes/rename call `verify_revision(expected)` first — a stale expectation returns `Conflict { current }`; nothing is ever overwritten blind. Knowledge updates preserve unknown frontmatter via `with_managed_properties` and rewrite only title/body; task mutations rewrite only managed keys via SCRUM-95's serializer. Rename confines both endpoints and re-locates the file by path — task identity lives in `brain_id` and is unaffected.

**Spec:** `docs/vault/markdown-vault-format.md` §6/§7; storage plan §7 (write and concurrency model)

## Global Constraints

- Faults cannot leave a partially written authoritative file (temp + rename).
- Conflict errors carry current provenance for re-read; verify happens before any mutation.
- Destructive operations keep destructive capability classification; audit wiring is SCRUM-109.
- No test requires Obsidian, sync, network, or the user's vault.
- Before PR: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, `cargo test --workspace`, `git diff --check`.

## Tasks

- [ ] Task 1: `write_atomic` + slug derivation + unique-path allocation.
- [ ] Task 2: Knowledge create/update/delete; task create/complete/update/delete through managed rewrites.
- [ ] Task 3: Rename (both kinds) with identity invariance for tasks.
- [ ] Task 4: Tests — create/update/delete happy paths, stale-revision conflicts, atomicity (temp cleanup), unique path allocation, rename invariance, destructive flows.
