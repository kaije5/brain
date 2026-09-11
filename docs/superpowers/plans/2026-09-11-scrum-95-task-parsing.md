# SCRUM-95 Provider-Backed Task Parsing and Serialization Implementation Plan

**Goal:** Map Brain-managed task files (format spec §6) to the provider-neutral task contract fields and back, with stable `brain_id` identity, canonical §6.1 serialization, and unknown-property preservation.

**Architecture:** `ParsedTask` in `cortex-vault` is context-free — the vault knows nothing about workspace or provider at parse time, so it produces the contract's component types (`ProviderTaskStatus`, `ProviderTaskPriority`, `TaskSchedulingMetadata`, `TaskId`, `ProviderResourceId`) plus the original `brain_id` text; SCRUM-91's provider assembles the full `ProviderTask` with workspace/provider provenance. Managed-key rewrites reuse `FrontmatterBlock::with_managed_properties`, so unknown properties and body prose survive byte-for-byte. Validation is exactly spec §6: missing `brain_id`/`status`/`priority` are `MissingProperty`, invalid values/bounds are `InvalidProperty`, duplicates are `DuplicateProperty`.

**Spec:** `docs/vault/markdown-vault-format.md` §6

## Global Constraints

- No filesystem, Obsidian, or sync types; total parsing with typed value-free errors.
- `brain_id` is the only task identity; file names and paths never participate.
- Canonical §6.1 property order on Brain writes; unknowns preserved on rewrite.
- Before PR: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, `cargo test --workspace`, `git diff --check`.

## Tasks

- [ ] Task 1: `ParsedTask` + frontmatter→contract mapping (status, priority, dates, duration, split, project/context) with spec §6 validation.
- [ ] Task 2: Canonical serialization + rewrite of an existing block preserving unknowns.
- [ ] Task 3: Fixtures: full/minimal tasks, unknowns, defaults, missing/invalid/duplicate keys, date forms, duration bounds, identity from `brain_id` independent of filename, round-trips.
