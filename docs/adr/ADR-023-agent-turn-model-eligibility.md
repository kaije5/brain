# ADR-023: Agent-turn eligibility requires tool calling and structured output

## Decision

In Sprint 1, a discovered model may execute agent turns only when its catalog entry holds current probing evidence for both tool/function calling and structured output. The runtime router enforces this at selection ([ADR-021](ADR-021-runtime-model-router.md)); ineligible models are never selected for the agent role, regardless of availability.

## Context

The bounded agent loop ([ADR-020](ADR-020-provider-neutral-inference-contract.md)) is safe because the model emits typed tool calls whose arguments validate against capability schemas before execution. A model that cannot reliably produce schema-constrained output turns every turn into malformed-output failures: wasted cost, degraded user experience, and constant exercise of failure paths. SCRUM-42 and SCRUM-6 therefore make this eligibility rule an acceptance criterion.

## Rules

- Eligibility is a property of normalized capability evidence (`ToolCalling` and `StructuredOutput`, each with outcome and timestamp), never of model names, vendor metadata claims alone, or availability alone.
- Evidence expires per the role policy's staleness window; expired evidence is not eligibility. Selection with only stale evidence yields `NoSuitableModel` ([ADR-024](ADR-024-no-silent-model-fallback.md)) until an explicit refresh re-probes.
- Models lacking either capability remain visible in the catalog and may serve future non-agent roles with different requirements; Sprint 1 defines only the `agent` role.
- The agent loop keeps all existing defenses regardless of eligibility: unknown or unauthorized tool names, malformed arguments, oversized messages, duplicate calls, and limit exhaustion fail as typed errors without widening access.

## Rationale

Gating on evidence keeps the safety argument of the bounded loop intact (schema-valid, policy-checked tool calls only), makes routing deterministic, and turns a model-quality problem into an explicit degraded state instead of runtime noise.

## Consequences and alternatives considered

Selecting any available model and relying on post-hoc validation was rejected: it is still safe but violates the Sprint 1 acceptance rule and produces unbounded malformed-turn churn. Name or family heuristics (for example, "Nemotron-family models are tool-capable") were rejected: they are not evidence and silently rot. The cost is probe traffic and the possibility of `NoSuitableModel` when evidence is stale, which the explicit refresh path bounds.
