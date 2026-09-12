# SCRUM-109 Vault Mutations × Policy, Operations and Audit Implementation Plan

**Goal:** Route every vault provider mutation through Cortex policy before dispatch and record redacted audit evidence — principal, operation id, provider resource target, destructive classification, and before/after revision metadata — so destructive provider operations keep the same guarantees as SQLite mutations.

**Architecture:** New `GovernedVaultProvider<P: PolicyPort, A: AuditPort>` wrapper in `apps/cortexd/src/governed.rs` decorating `MarkdownVaultProvider`. Every operation evaluates `PolicyPort::evaluate(context, capability, target)` (SCRUM-99's target dimension) with `ResourceTarget::ProviderResource`/`ProviderScope`; a deny appends a `Rejected` audit event and returns `ProviderError::Unauthorized` before any provider dispatch. Allowed mutations append a `Succeeded` event carrying `ProviderAuditMetadata` (before/after revisions from the mutation result). Capability mapping uses the existing grant model, extended with the storage-plan §11 knowledge capabilities (`cortex_knowledge_create/update/delete`) as first-class `Capability` variants.

**Spec:** SCRUM-90 design ("Policy, audit, and operation identity"); storage plan §7/§9/§11

## Global Constraints

- Provider dispatch happens only after policy allows principal + workspace + capability + target.
- Audit evidence is redacted: identifiers, classifications, revisions — never content or paths.
- Destructive deletes keep `AuditClassification::Mutation` with `destructive: true`.
- No test requires Obsidian, sync, network, or the user's vault.
- Before PR: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, `cargo test --workspace`, `git diff --check`.

## Tasks

- [ ] Task 1: `KnowledgeCreate/Update/Delete` capability variants + metadata + catalog + storage whitelist.
- [ ] Task 2: `GovernedVaultProvider` gating + audit wrapper for both ports.
- [ ] Task 3: Tests — allowed mutation with audit evidence, denied mutation (no dispatch, Rejected audit), destructive delete classification, read gating.
