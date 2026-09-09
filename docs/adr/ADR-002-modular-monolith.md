# ADR-002: Modular monolith architecture

## Decision

Cortex is a modular monolith with a thin, separately runnable MCP transport adapter.

## Rationale

One deployable state owner keeps policy and persistence boundaries explicit without microservice overhead.
