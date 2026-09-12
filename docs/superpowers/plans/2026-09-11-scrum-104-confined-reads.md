# SCRUM-104 Confined Enumeration and Read Operations Implementation Plan

**Goal:** Implement the read half of the `KnowledgeProvider`/`TaskProvider` ports over the confined vault: bounded enumeration, `get`, and text-containment `search`, all through the SCRUM-106 confinement gate and the `cortex-vault` format layer.

**Architecture:** `MarkdownVaultProvider::open` gains the daemon workspace identity (composition passes `DaemonConfig::workspace_id`); every constructed `ProviderResourceRef` is workspace-scoped through it. Enumeration walks the canonical root recursively, admitting only `.md` files that pass `confine`, bounded by the port's 100-result page limit. **Identity scheme (documented limitation):** task identity is the canonical `brain_id` (format spec §6.2, rename-stable); documents have no on-disk stable id in the format spec, so their resource id is `path:<relative>` — rename-stable document identity is deferred to the derived index (SCRUM-92). Reads parse via `cortex-vault` and build contract DTOs with provenance (SHA-256 content hash; observed revision = hash-derived opaque token until SCRUM-107 adds optimistic concurrency). Mutation port methods return `ProviderError::Unavailable` until SCRUM-108.

**Spec:** `docs/vault/markdown-vault-format.md`; SCRUM-90 design ("Application ports")

## Global Constraints

- Every path passes `confine`; enumeration honors exclusions and scopes.
- All collections bounded (port page limit); files beyond the 1 MiB format bound are skipped during enumeration, not a crash.
- Typed redacted errors only; no local paths in `ProviderError`.
- No test requires Obsidian, sync, network, or the user's vault.
- Before PR: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, `cargo test --workspace`, `git diff --check`.

## Tasks

- [ ] Task 1: Workspace-scoped construction + document/task identity mapping helpers.
- [ ] Task 2: Bounded recursive enumeration for knowledge and tasks.
- [ ] Task 3: `get` and containment `search` for both ports; `Unavailable` mutations.
- [ ] Task 4: Fixtures via tempdir (seeded vault: nested docs, Tasks/ dir, excluded dirs, oversized file, empty vault) asserting bounded enumeration, parse-to-DTO values, not-found, and wrong-workspace rejection.
