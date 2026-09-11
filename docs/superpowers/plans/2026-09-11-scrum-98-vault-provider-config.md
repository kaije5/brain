# SCRUM-98 Vault Provider Configuration Implementation Plan

**Goal:** Define validated daemon-owned configuration for the Markdown vault provider (local root, allowed logical scopes, bounded exclusions, explicit provider mode) and an injectable provider composition seam, without sync-product, UI, credential, or transport dependencies.

**Architecture:** New `apps/cortexd/src/vault.rs` owns `VaultProviderConfig` (filesystem `PathBuf` stays in cortexd, never a domain resource) with typed coarse validation errors and a redacted `Debug` that never prints the local root. `LocalSettings` gains a `[vault]` section parsed into the typed config; `DaemonConfig` carries `Option<VaultProviderConfig>`, validates root accessibility at composition, and exposes a builder. An in-memory `InMemoryVaultProvider` implementing `KnowledgeProvider` + `TaskProvider` is the composition test seam so tests start a composed daemon without a filesystem, Obsidian, or a sync process. No adapter or path-confinement enforcement here (SCRUM-91).

**Spec:** `docs/superpowers/specs/2026-09-11-scrum-90-provider-contracts-design.md` ("Configuration and composition")

## Global Constraints

- Configuration holds no credentials and no sync-transport state; unknown keys are rejected.
- One local vault root; scopes are a bounded non-empty set of logical resource kinds; exclusions are bounded and vault-relative (no absolute paths, no `..`).
- `Debug` output never contains the local root path or user content.
- Missing/invalid/inaccessible/contradictory values fail startup with typed, value-free diagnostics.
- Tests need no Obsidian, sync subscription, network, or the user's vault.
- Before PR: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, `cargo test --workspace`, `git diff --check`.

## Tasks

- [ ] Task 1: `VaultProviderConfig`, `VaultProviderMode`, scope/exclusion validation, redacted `Debug`, typed `VaultConfigError` in `apps/cortexd/src/vault.rs` + unit/integration tests.
- [ ] Task 2: `[vault]` section in `LocalSettings` (`provider_id`, `root`, `mode`, `scopes`, `exclusions`), template update, parse/validate tests.
- [ ] Task 3: `DaemonConfig.vault` composition: load from settings, accessibility check, `with_vault_provider` builder, startup diagnostics tests.
- [ ] Task 4: `InMemoryVaultProvider` seam implementing both provider ports; daemon composition startup test with the fake provider.
