# SCRUM-113 Debounced Idempotent Vault Watching Implementation Plan

**Goal:** Implement the vault watching layer: bounded, debounced coalescing of raw filesystem events (create/update/delete/rename) into idempotent index operations, honoring confinement so only allowed paths are ever indexed.

**Architecture:** New `apps/cortexd/src/vault_watcher.rs` (pure, deterministic — time is caller-supplied `u64` millis, no threads or OS callbacks, so the OS-specific watch mechanism can attach later and tests are reproducible). `VaultEvent` models the four filesystem transitions. `EventQueue::push(event, at)` accumulates raw events; `drain(ready_at)` coalesces per-path bursts within the debounce window into one latest-state event per path (last-writer kind wins, renames collapse to their final form) and returns them in first-touched order. `apply_event` maps a coalesced event to idempotent `DerivedVaultIndex` operations through the confined provider reads: upsert only when the content hash differs from the indexed provenance, delete only when a chunk existed — applying the same event twice is a no-op. Oversized/unparseable files are skipped, never fatal (storage plan §8: partial/missed/duplicated events tolerated; the reconciliation scan is SCRUM-114).

**Spec:** storage plan §8 (file watching and indexing); SCRUM-92 acceptance (debounced idempotent watcher)

## Global Constraints

- Deterministic: no real sleeping, no OS callbacks in tests; debounce windows are virtual.
- Only confined paths are indexed; oversized/unparseable files are skipped, never fatal.
- Idempotency: re-applying an event changes nothing (hash-gated).
- Before PR: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, `cargo test --workspace`, `git diff --check`.

## Tasks

## Task 1: `VaultEvent` + `EventQueue` debounce/coalescing with virtual time

Implement a bounded event queue whose per-path debounce timer resets on each
new event. Coalesce ready events in first-touched order without emitting an
older transition while a newer transition for the same path is still pending.

## Task 2: `apply_event` idempotent index operations through the confined provider

Apply create/update/delete/rename hints only after confinement. Read and hash
the current file state before indexing, and never load an oversized file into
memory. Reapplying any coalesced event must converge without duplicate index
state.

## Task 3: Tests — debouncing, bounded indexing, and convergence

Cover burst coalescing, a newer same-path event delaying the whole burst,
rename collapse, idempotent re-application, confined/excluded skips,
oversized/binary skips, delete of missing, and duplicate/reordered event
convergence.
