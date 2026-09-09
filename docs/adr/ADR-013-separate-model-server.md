# ADR-013: Separate local model server

## Decision

The local model server is separate from `cortexd`.

## Rationale

This isolates model lifecycle and provider payloads from the state-owning daemon.
