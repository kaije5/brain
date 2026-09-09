# Cortex Sprint 1 Model Routing Design (v0.2)

**Status:** Proposed; written as the SCRUM-40 design prerequisite for SCRUM-41, SCRUM-42, SCRUM-43, and SCRUM-62
**Date:** 2026-09-09
**Scope:** Sprint 1 of v0.2 under parent story SCRUM-6 (canonical requirements REQ-2, REQ-9, REQ-10, REQ-13, REQ-18, REQ-19, REQ-21): the provider-neutral inference contract, the runtime model router, NVIDIA NIM as the first concrete provider, agent-turn model eligibility, and explicit degraded behavior, plus the security boundary for remote inference. Native iOS and unrelated later v0.2 architecture are out of scope.

## 1. Purpose and preserved invariants

Sprint 1 proves that Brain can choose a compatible configured model at runtime, with NVIDIA NIM working end to end, without coupling Cortex to one vendor and without weakening the v0.1 security architecture. The implementation tasks (SCRUM-41 through SCRUM-62) build on the delivered v0.1 codebase; this design defines their contracts before code.

Every Sprint 1 decision preserves the v0.1 invariants that this slice touches:

1. `cortexd` remains the final local policy authority. Routing, discovery, probing, capability resolution, and every tool execution happen inside `cortexd`'s application boundary and keep their policy and audit semantics. Clients never select providers or models for agent turns.
2. Domain and application contracts stay provider-neutral. No NVIDIA-specific type, endpoint shape, or payload enters `cortex-domain` or `cortex-application`.
3. There is no silent fallback. When no eligible model exists, Cortex reports an explicit degraded state instead of substituting a provider. Offline and degraded operation remain first-class states, consistent with local-first architecture.
4. Credentials exist only as opaque `SecretRef` values resolved through the platform secret store. They never appear in domain data, configuration files, logs, tracing, audit payloads, model context, or documentation.
5. Agent turns stay bounded and schema-validated. The v0.1 `AgentRunner` limits, typed validation, duplicate-call detection, and per-tool policy checks are preserved, not reimplemented.
6. External content is data, never privileged instruction. This now explicitly includes provider discovery responses and model metadata.

New Sprint 1 invariants introduced by this design:

7. A model executes agent turns only when its capability record contains probing evidence for both tool/function calling and structured output.
8. Eligibility derives from recorded evidence, never from model names, vendor metadata claims alone, or availability alone.
9. Authorization of agent tools is derived only from Cortex policy and typed capability definitions, never from provider output.

## 2. What changes and what does not

| Area | v0.1 state | Sprint 1 change |
| --- | --- | --- |
| Inference ports | `InferenceProvider::complete` over provider-neutral messages/tools; loopback OpenAI-compatible adapter with a fixed configured model | Add a discovery/probing port and normalized model capability metadata; extend the chat contract with structured-output constraints. `InferenceProvider::complete` keeps its shape. |
| Providers | One local loopback adapter (Nemotron default profile) | Add the NVIDIA NIM adapter as the first concrete remote provider. Base URL is configuration; hosted and self-hosted NIM both fit. |
| Model selection | Fixed model in provider configuration | New capability-aware runtime model router in the `cortexd` application layer, backed by durable provider/model profiles. |
| Capability metadata | None (single configured model) | Discovered models, probed capabilities, eligibility, and evidence timestamps stored as a refreshable catalog. |
| Error taxonomy | `InferenceUnavailable`, `InferenceTimeout`, `MalformedModelOutput` | Add `NoSuitableModel { role, reason }` for the explicit degraded state. |
| Endpoints | Loopback-only enforced by `OpenAiCompatibleConfig` | Remote NIM endpoints permitted over TLS as explicit provider profile configuration; the local adapter keeps its loopback restriction. |
| Agent loop | Bounded `AgentRunner` with `AuthorizedCapabilities` | Unchanged in shape; its provider now comes from the router, and eligibility gates which provider it may receive. |

Unchanged: capability catalog and policy/audit flow, SQLite ownership and migrations, MCP gateway and pairing, CLI/IPC surface, the embedding provider used by hybrid search.

## 3. Architecture placement

All Sprint 1 behavior lives inside `cortexd`'s existing dependency direction. Nothing new faces the network except the provider adapter itself.

```text
brain CLI / MCP gateway / agent entry point
                 |
        authenticated local IPC
                 |
             cortexd application layer
                 |
   ModelRouter  <- ModelCatalog (refreshable capability metadata)
      |              ^  ModelDiscoveryPort + probing (adapter)
      v
   AgentRunner (bounded loop, AuthorizedCapabilities, per-tool policy + audit)
      |
   InferenceProvider port
      |-- NimProvider adapter   (GET /v1/models, POST /v1/chat/completions)
      |-- OpenAI-compatible local adapter (unchanged, loopback)
      |
   SecretStore port (unchanged; resolves SecretRef values)
```

The router, catalog, and NIM adapter are new infrastructure behind existing application contracts. The NIM adapter performs outbound HTTPS to the configured endpoint; it stores nothing and decides nothing about authorization.

## 4. Provider-neutral inference contract (ADR-020)

The normalized contract is owned by the Cortex application/inference boundary:

- `ProviderProfileId`, `ModelId` (a validated, bounded model identifier string), `InferenceRole` (Sprint 1: `agent`; embeddings keep their existing dedicated port).
- `ModelDescriptor` { `model_id`, provider attribution, optional metadata hints }.
- `ModelCapability` { `ToolCalling`, `StructuredOutput` } - the closed set Sprint 1 reasons about.
- `CapabilityEvidence` { `probed_at`, probe method, outcome } and a normalized catalog entry `DiscoveredModel` { descriptor, capabilities, eligible, reason }.
- Chat remains `InferenceRequest`/`InferenceResponse` with `InferenceTool` schemas; a structured-output constraint is added as an optional, normalized request field that adapters map to provider mechanisms.
- The new `ModelDiscovery` port yields discovered descriptors; probing produces normalized capability evidence. Adapters normalize or reject at the edge: payload size limits, model-identifier validation, and typed errors are applied before any provider data becomes catalog state.

Rule: anything a provider sends is external data. It is validated, bounded, and normalized before routing or persistence; it can never alter Cortex configuration, capability definitions, or policy.

## 5. Runtime model router (ADR-021)

Durable model profiles are configuration records, stored without secret material:

- A provider profile: stable identifier, provider kind (`nim`, `local-openai-compatible`), base URL, optional `SecretRef`, timeouts, allocation limits, enabled flag.
- A role policy: for the `agent` role, required capabilities (`ToolCalling` + `StructuredOutput`), optional preference ordering over models, and the staleness window after which capability evidence must be refreshed before it can justify selection.

Selection is a pure, deterministic function of (profiles, catalog state, role): filter providers by enabled state, filter models by role requirements using catalog evidence, order by configured preference then stable model identifier, and select the first. Same inputs always produce the same model. Given no eligible model, the router returns `NoSuitableModel` rather than a substitute.

The catalog is a durable cache with evidence timestamps, refreshed explicitly (`brain models refresh`, `brain doctor`) and opportunistically per configured staleness policy. Refresh failures keep the last catalog but never fabricate or extend eligibility; stale evidence is not eligibility.

## 6. NVIDIA NIM as the first provider (ADR-022)

The NIM adapter uses the OpenAI-compatible surface only: `GET /v1/models` for discovery and `POST /v1/chat/completions` for agent turns. The base URL is configuration (NVIDIA hosted API catalog or a self-hosted NIM deployment). When the endpoint requires authentication, the adapter resolves the configured `SecretRef` at call time and sends it as a bearer credential; keyless self-hosted deployments are valid configuration. Non-loopback endpoints require TLS.

Discovery responses are normalized into `ModelDescriptor`s; model identifiers are validated (charset, length) before they can influence anything. Each discovered model is capability-probed with deterministic bounded requests:

- a tool-call probe (one trivial tool; eligible only if the response carries a well-formed call),
- a structured-output probe (a small JSON schema; eligible only if the response parses and validates).

Probe results are recorded per model as `CapabilityEvidence` with an outcome and reason, and drive eligibility. NVIDIA-specific request/response mapping (tool-choice nuances, response-format encoding, error shapes) stays inside the adapter.

## 7. Agent-turn eligibility (ADR-023)

A model may execute agent turns in Sprint 1 only when its catalog entry holds current evidence for both `ToolCalling` and `StructuredOutput`. The router enforces this at selection. The agent loop keeps its existing defenses regardless of the selected model: unknown tool names, unauthorized capabilities, malformed arguments, oversized messages, duplicates, and limit exhaustion all fail as typed errors without widening access. Non-agent roles may declare different requirements later; Sprint 1 defines only the `agent` role.

## 8. Degraded behavior: `NoSuitableModel` (ADR-024)

`NoSuitableModel { role, reason }` is added to the application error taxonomy and is distinct from `InferenceUnavailable` (selected provider unreachable or failing). It is returned when the router cannot find a model that satisfies the role's requirements from current evidence. Cortex then:

- fails the agent command with an explicit, safe, machine-readable degraded result;
- surfaces the degraded state in `brain status` and `brain doctor` with remediation hints (refresh models, check credential and endpoint);
- writes an audit event for the affected command outcome, consistent with v0.1 audit redaction;
- performs no cross-provider, cross-model, or unconfigured substitution. Any future fallback must be an explicit, stored, audited policy decision.

## 9. Dynamic capability resolution and the bounded loop (SCRUM-43, SCRUM-62)

Before each agent turn, `cortexd` resolves the exact capability subset authorized for the current principal and request from stored grants and typed capability definitions. Tool schemas supplied to the model are generated from those provider-neutral contracts, so unauthorized capabilities are absent from the request rather than rejected after selection. Provider output cannot expand the set: the existing `AgentRunner` checks every returned tool name against the authorized set and validates arguments against typed schemas before execution, with per-tool policy and audit unchanged. Tool results are bounded before re-entering the conversation, and a completed turn yields an auditable final result without persisting hidden reasoning.

## 10. Security boundary

The Sprint 1 threat-model extension (docs/threat-model/cortex-v0.2-sprint1-model-routing.md) covers, at minimum: content sent to the provider/model, credentials and SecretRefs, capability widening, malformed structured output, and prompt injection - including injection arriving through provider discovery metadata. The standing data boundary: a turn may carry the user prompt, the authorized capability subset (names, descriptions, schemas), and bounded tool results; it may never carry secret material, database contents outside those tool results, audit payloads, or local paths.

## 11. Testing requirements

No core test requires a live NIM service. Fakes cover discovery (empty, partial, malformed, oversized), probing (eligible, ineligible, timeout, non-conforming responses), router determinism and eligibility filtering, authentication-required versus keyless endpoints, and degraded `NoSuitableModel` behavior. Capability-resolution tests cover allowed, denied, mixed, and prompt-injection cases. Provider isolation is enforced structurally: no NVIDIA types compile into `cortex-domain` or `cortex-application`. Optional live integration stays explicitly isolated behind configuration, consistent with v0.1 test policy.

## 12. Explicit exclusions

Native iOS; additional providers beyond preserving the generic interface (OpenRouter and other local servers are later work); provider fallback policy; streaming responses; automatic model benchmarking beyond the two defined probes; embedding-model routing (embeddings keep their dedicated port and configuration); and all other v0.2 roadmap areas not required by SCRUM-6.

## 13. ADRs established by this design

1. ADR-020: provider-neutral inference contract for discovered remote providers.
2. ADR-021: runtime model router with durable model profiles.
3. ADR-022: NVIDIA NIM as the first concrete provider.
4. ADR-023: agent-turn eligibility requires tool calling and structured output.
5. ADR-024: explicit degraded state, no silent model fallback.

## 14. Sequencing constraint

Implementation is test-driven in small reviewable tasks: SCRUM-65 prepares the NIM development profile and SecretRef (no credential material in Jira, Confluence, or Git); SCRUM-41 builds profiles and the router against fakes; SCRUM-42 lands the NIM adapter, discovery, and probing for the real end-to-end path; SCRUM-43 and SCRUM-62 complete dynamic capability resolution and the bounded agent loop. Each task must keep `cortexd` as the final policy authority and preserve invariants 1-9 above.
