# ADR-004: SQLite as the initial persistent store

## Status

Superseded in part by
[ADR-025](ADR-025-markdown-vault-authority.md). SQLite remains the durable store
for Cortex-owned runtime state, but it does not remain authoritative for
user-authored personal knowledge or task records after the v0.2 cutover.

## Decision

SQLite is the initial durable Cortex runtime store, opened for mutation only by
`cortexd`.

## Rationale

It supports local durability and transactional Cortex-state-plus-audit writes.
Provider-backed user content follows its own revision-safe mutation boundary.
