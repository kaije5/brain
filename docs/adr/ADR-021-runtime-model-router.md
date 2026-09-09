# ADR-021: Runtime model router with durable model profiles

## Decision

`cortexd`'s application layer owns a deterministic, capability-aware model router that selects the model for each inference role from durable provider/model profiles plus the refreshable capability catalog. Clients never name providers or models for agent turns; selection is a pure function of configured state and recorded evidence.

## Context

Sprint 1 must choose among discovered NIM models at runtime (SCRUM-41) while keeping `cortexd` the final local policy authority. Routing is policy: deciding which model may execute agent turns and where personal data is sent belongs inside the daemon, not in CLI or gateway code.

## Behavior

- Durable profiles store, without secret material: provider kind, base URL, optional `SecretRef`, timeouts, allocation limits, and an enabled flag. Role policy states required capabilities (agent: tool calling + structured output per [ADR-023](ADR-023-agent-turn-model-eligibility.md)), optional preference ordering, and a staleness window for evidence.
- Selection filters by enabled providers, filters models by role requirements using catalog evidence, orders by configured preference then stable model identifier, and selects the first. Identical inputs always select the same model.
- The catalog is a durable cache with evidence timestamps. Refresh is explicit (`brain models refresh`, `brain doctor`) or per configured staleness policy. Refresh failure keeps the last catalog but never fabricates or extends eligibility; stale evidence is not eligibility.
- No eligible model yields `NoSuitableModel` per [ADR-024](ADR-024-no-silent-model-fallback.md), never a substitute.

## Rationale

Determinism makes routing auditable and testable; keeping it in the application layer preserves the single policy authority; and profiles plus normalized metadata let a second provider land as a new adapter and profile with no domain or application rewrite.

## Consequences and alternatives considered

Client-side selection was rejected: it fragments policy authority and breaks auditability. Availability-ordered "first working model" routing was rejected: it is nondeterministic and enables silent fallback. The cost is a catalog that can be stale, which the explicit staleness window and degraded behavior bound.
