# SCRUM-106 First-Party Vault Adapter and Root-Scope Model Implementation Plan

**Goal:** Create the first-party `MarkdownVaultProvider` adapter shell: an opened, canonicalized vault root plus the confined-path model that guarantees every later operation stays inside the configured root, respects exclusions and resource scopes, and rejects traversal and symlink escape.

**Architecture:** New infrastructure module `apps/cortexd/src/vault_provider.rs` (filesystem path types stay in cortexd per the approved design). `MarkdownVaultProvider::open(VaultProviderConfig)` canonicalizes the root. `confine(relative, kind)` is the single gate every later read/mutation goes through: relative-path validation (no absolute, no `..`, no backslash, bounded), per-component exclusion matching, scope check via `allows_kind`, and a deepest-existing-ancestor `canonicalize` check that rejects symlinks escaping the root. A `ConfinedPath` value is the only way later subtasks obtain an absolute path. Read/mutation operations, revisions, and audit wiring belong to SCRUM-104/107/108/109.

**Spec:** `docs/vault/markdown-vault-format.md`; SCRUM-90 design ("Configuration and composition"); storage plan §9 (security model).

## Global Constraints

- Client-visible errors are typed, value-free, and never contain local paths.
- No Obsidian, sync, or credential types; configuration already excludes them.
- No test requires Obsidian, a sync subscription, a network service, or the user's vault.
- Before PR: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, `cargo test --workspace`, `git diff --check`.

## Tasks

- [ ] Task 1: `MarkdownVaultProvider::open` + canonical root; typed `VaultPathError`.
- [ ] Task 2: `confine(relative, kind)` — validation, exclusion matching, scope check, symlink-escape rejection — returning `ConfinedPath`.
- [ ] Task 3: Adversarial confinement tests (traversal, absolute, backslash, symlink escape on unix, exclusions, out-of-scope kinds, canonicalization of a moved root).
