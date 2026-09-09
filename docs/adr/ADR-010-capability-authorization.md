# ADR-010: Capability-based authorization

## Decision

`cortexd` makes final authorization decisions from a principal and capability.

## Rationale

Authorization is independent of untrusted transport-provided workspace or principal fields.
