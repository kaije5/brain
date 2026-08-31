# ADR-015: Authenticated CLI-to-daemon local IPC

## Decision

The CLI is an authenticated local IPC client of `cortexd`.

## Rationale

Only the daemon owns mutable SQLite state and final policy enforcement.
