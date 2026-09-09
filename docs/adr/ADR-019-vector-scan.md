# ADR-019: Bounded SQLite vector scan behind a future index port

## Decision

v0.1 uses a bounded SQLite vector scan behind a replaceable index port.

## Rationale

Explicit record and time limits protect the local daemon while allowing a future index.
