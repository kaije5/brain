# ADR-020: Provider-neutral inference contract for discovered remote providers

## Decision

All provider interaction - discovery, capability probing, chat with tools, and structured output - crosses a provider-neutral contract owned by the Cortex application/inference boundary. NVIDIA-specific types remain inside the NIM adapter; `cortex-domain` and `cortex-application` never reference provider payload types. This extends [ADR-007](ADR-007-local-inference.md) from a fixed configured model to dynamic discovery and per-role selection.

## Context

v0.1 inference targeted one loopback server with a model fixed in configuration. Sprint 1 must discover available models at runtime, record normalized capability metadata, and route roles to models - all without letting a vendor SDK or wire format shape Cortex contracts.

## Contract

- Normalized types: `ProviderProfileId`, `ModelId`, `InferenceRole`, `ModelDescriptor`, `ModelCapability` (`ToolCalling`, `StructuredOutput`), `CapabilityEvidence`, `DiscoveredModel`; chat stays `InferenceRequest`/`InferenceResponse`/`InferenceTool`, with an optional normalized structured-output constraint.
- New discovery/probing port beside [InferenceProvider](../../../crates/cortex-inference/src/provider.rs); the router consumes only normalized entries.
- Adapters normalize or reject at the edge: model identifiers are validated and bounded, payload sizes are limited as in v0.1, and provider failures map to typed application errors.
- Provider responses are external data: they can never alter configuration, capability definitions, grants, or policy.

## Rationale

Providers can be added or replaced without touching domain or application logic; fakes substitute cleanly in tests; and normalizing at the adapter edge keeps unbounded or hostile provider payloads out of routing and persistence.

## Consequences and alternatives considered

Adopting a vendor SDK's types in shared contracts was rejected: it couples Cortex state and tests to one vendor and repeats the mistake ADR-007 avoided. Per-provider branches in routing logic were rejected: they multiply test surface and leak provider semantics into policy. The cost is a normalization layer per adapter, which is small and independently testable.
