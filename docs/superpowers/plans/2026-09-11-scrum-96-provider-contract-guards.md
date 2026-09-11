# SCRUM-96 Provider Contract Guards Implementation Plan

**Goal:** Prove the provider contracts are substitutable and prevent new legacy coupling: contract tests prove a fake provider can replace the future Markdown adapter, and dependency/architecture guards keep Obsidian, iCloud, sync, filesystem, and transport types — plus the legacy SQLite note/task abstractions — out of the new boundary.

**Architecture:** Three pure test additions, no production changes. (1) Dependency allowlist inspections of `cortex-domain` and `cortex-application` manifests: any new dependency (filesystem watchers, transport, vendor SDKs) fails CI. (2) Forbidden-token source scans of the domain sources and the provider/port modules, and a legacy-isolation scan proving the provider modules never reference the legacy `Note`/`Task` aggregates or repositories and the legacy repository traits never reference the provider ports. (3) A generic consumer scenario over `KnowledgeProvider` + `TaskProvider` run against two independently implemented in-memory fakes with different internals, asserting identical outcome shapes — the substitution proof required by ADR-026.

**Spec:** `docs/superpowers/specs/2026-09-11-scrum-90-provider-contracts-design.md` ("Legacy isolation and migration", "Verification strategy")

## Global Constraints

- Test-only change: no production source or manifest edits.
- No test requires Obsidian, a sync subscription, a network service, or the user's vault.
- Before PR: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, `cargo test --workspace`, `git diff --check`.

## Tasks

- [ ] Task 1: `cortex-domain` dependency allowlist + forbidden-token guard tests.
- [ ] Task 2: `cortex-application` dependency allowlist, provider-module forbidden tokens, and legacy-isolation guard tests.
- [ ] Task 3: Substitutability consumer scenario against two independent fakes.
- [ ] Task 4: Full DoD verification.
