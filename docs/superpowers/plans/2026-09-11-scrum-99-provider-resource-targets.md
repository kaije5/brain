# SCRUM-99 Provider Resource Targets Implementation Plan

**Goal:** Generalize authorization, operation identity, and audit mutation targets from canonical SQLite entity assumptions to the provider-neutral `ResourceTarget` while retaining principal, workspace, capability, operation ID, and redaction invariants.

**Architecture:** `cortex-domain` owns the `ResourceTarget` enum (`CortexEntity`, `ProviderResource`, `ProviderScope`) and redacted before/after provider revision/hash audit metadata. `cortex-application` carries the target through `OperationIdentity`, `AtomicMutation`, and `PolicyPort::evaluate`; `GrantPolicy` denies provider targets outside the authenticated workspace. `cortex-storage` persists targets as tagged JSON in the existing columns and decodes legacy plain-UUID rows as `CortexEntity`. No legacy repository implements a provider port.

**Spec:** `docs/superpowers/specs/2026-09-11-scrum-90-provider-contracts-design.md` ("Policy, audit, and operation identity")

## Global Constraints

- Audit evidence carries identifiers, revisions, hashes, and classifications only — never title, body, paths, credentials, or provider diagnostics.
- Replaying an operation ID against a different target is a typed conflict.
- Policy deny happens before any provider or repository dispatch.
- Existing note/task behavior is preserved; legacy repositories are not adapted to provider ports.
- Before PR: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, `cargo test --workspace`, `git diff --check`.

## Tasks

- [ ] Task 1: `ResourceTarget` + `ProviderAuditMetadata` in `cortex-domain` with redaction/validation tests.
- [ ] Task 2: `AuditEvent.target: Option<ResourceTarget>` + `provider_metadata`; storage roundtrip for all variants; legacy plain-UUID row decode; `redacted_metadata` column carries revision/hash JSON.
- [ ] Task 3: `OperationIdentity.target: Option<ResourceTarget>`; operation outcome JSON v3 (decode v2 legacy); replay-against-different-target conflict tests (entity → provider, provider → scope).
- [ ] Task 4: `PolicyPort::evaluate` gains the target dimension; `GrantPolicy` denies cross-workspace provider resources/scopes (`PolicyDeny::TargetOutsideWorkspace`); allow/deny/destructive tests prove deny precedes dispatch.
- [ ] Task 5: Workspace-wide fixture updates (`ipc.rs`, e2e support, search/application tests) and full DoD verification.
