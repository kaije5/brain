# ADR-004: SQLite as the initial persistent store

## Decision

SQLite is the initial durable store, opened for mutation only by `cortexd`.

## Rationale

It supports local durability and transactional state-plus-audit writes.
