# ADR-022: NVIDIA NIM as the first concrete provider

## Decision

NVIDIA NIM is the first concrete provider adapter behind the neutral contract of [ADR-020](ADR-020-provider-neutral-inference-contract.md). It uses the OpenAI-compatible surface only - `GET /v1/models` for automatic discovery and `POST /v1/chat/completions` for agent turns - with a configurable base URL, `SecretRef`-based or keyless authentication as endpoint configuration, and normalized capability metadata produced by deterministic probing. NVIDIA-specific request and response types remain inside the adapter.

## Context

SCRUM-6 makes NIM the Sprint 1 proof that the provider abstraction works end to end, while SCRUM-42 forbids hard-coded model identifiers and requires refreshable discovery with capability probing. NIM serves both the NVIDIA hosted API catalog and self-hosted deployments, so endpoint location must not be an architectural assumption.

## Behavior

- **Endpoint:** base URL is provider-profile configuration. Non-loopback endpoints require TLS. The adapter validates and bounds every response before it becomes catalog state.
- **Discovery:** `GET /v1/models` is the only discovery source. Returned model identifiers are validated (charset, length) as external data, then normalized into `ModelDescriptor`s.
- **Authentication:** when the configured endpoint requires it, the adapter resolves the profile's `SecretRef` through the secret store at call time. Keyless self-hosted endpoints are valid. Credential values never enter configuration, logs, tracing, audit payloads, or model context.
- **Capability probing:** each discovered model is probed with deterministic bounded requests - a tool-call probe (one trivial tool; success requires a well-formed call) and a structured-output probe (a small JSON schema; success requires a parsing, validating response). Results are recorded as `CapabilityEvidence` with outcome, reason, and timestamp; they are the only source of eligibility under [ADR-023](ADR-023-agent-turn-model-eligibility.md).
- **Refresh:** discovery and probing rerun on demand because availability and capabilities change. Probe cost is bounded (small prompts, per-model and per-refresh caps); probe failures mark a model ineligible with a reason rather than guessing.
- **Mapping:** NIM tool-choice nuances, response-format encoding, and error shapes map to normalized types or typed errors inside the adapter.

## Rationale

Proving the generic interface against a real hosted catalog - with discovery, probing, routing, and a bounded agent turn - is the smallest credible demonstration that a second provider can follow without rewriting domain or application logic. Probing yields evidence instead of trusting vendor metadata claims.

## Consequences and alternatives considered

A hard-coded NIM model list was rejected: it violates the discovery and refresh requirements and rots as the catalog changes. Trusting metadata fields alone was rejected: they are neither bounded nor evidence of the structured behavior the agent loop requires. Other providers first were rejected: SCRUM-6 fixes NIM as the Sprint 1 target. The cost is probe traffic against the endpoint, bounded by per-refresh caps.
