# OpenAI-Compatible Model Connector Design

## Goal and scope

Replace the NIM-named inference discovery path with one connector for any configured OpenAI-compatible Chat Completions endpoint. A provider profile supplies an API base URL, authentication strategy, model candidate source, timeouts, and typed compatibility options. The connector lists or accepts candidate model IDs, probes capabilities, and passes normalized evidence to the existing router. NVIDIA's hosted API is one possible endpoint; this design does not target self-hosted NIM.

The knowledge and task provider contracts are outside this change. Other wire protocols, such as Anthropic Messages or OpenAI Responses, need separate connectors.

## Why this approach

The current `NimDiscovery` performs generic HTTP model listing and Chat Completions probes, while `OpenAiCompatibleProvider` handles actual inference. Keeping both as separate vendor-named transports duplicates request and security behavior. Renaming `nim.rs` alone would preserve that duplication. Removing discovery would lose dynamic routing. A single connector is the smallest design that retains both features.

## Profile contract

- `base_url` is the **API base**, not merely the host. Join `models`, `chat/completions`, and existing embedding paths to this base. For example, `https://api.openai.com/v1` yields `/v1/models` and `/v1/chat/completions`; `https://example.test/custom/api` yields `/custom/api/models` and `/custom/api/chat/completions`. Do not add or strip `/v1` implicitly. Existing host-only URLs must be updated to the intended API base.
- `api_mode = "openai_completions"` remains the only supported mode. Unknown modes remain invalid.
- `model_source = "list"` calls `GET {base_url}/models` and parses bounded `data[*].id` entries. This is the default for existing profiles. A profile's existing `models` array filters listed IDs when present.
- `model_source = "configured"` uses the profile's nonempty `models` array as candidate IDs and does not call `/models`. This is an explicit operator choice for endpoints without a documented list API; it is not an automatic fallback after a failed GET.
- The existing `SecretRef` and `auth_type` remain profile-scoped. The daemon resolves each enabled profile's credential independently. A missing required credential excludes that profile before any request. The selected route carries only its own credential into runtime inference.
- Typed quirks include `omit_tool_choice` and a probe token-limit field of `max_tokens` or `max_completion_tokens`. Existing profiles default to `max_tokens`; a profile requiring the newer field opts in explicitly. No arbitrary JSON escape hatch is added.

## Discovery, probing, and routing

The connector validates candidate IDs and caps their count before probing. A model list is a candidate source, not evidence of chat or tool support. Both candidate sources use the same bounded probes against `{base_url}/chat/completions`.

The tool probe supplies only a harmless `cortex_probe` function. It requests that function when the endpoint accepts `tool_choice`; a profile with `omit_tool_choice` omits that parameter. A positive result requires a complete response with a matching, parseable tool call. The separate structured-output probe supplies no tools, requests a small strict `json_schema`, and checks that the returned object matches it. `json_object` proves JSON mode only and does not count as `StructuredOutput`. The output-token budget is capped at 128; a response ending for length or content filtering is inconclusive, not positive evidence.

The router keeps its existing deterministic selection, profile provenance, declared-model filter, fresh-evidence requirement, and explicit degraded state. Listing failure in `list` mode does not switch to configured candidates. A provider that supports only JSON mode or rejects a probe remains ineligible for the current agent role; this may reduce availability compared with the old HTTP-success-only probe.

## Safety and compatibility

- Preserve HTTPS for remote endpoints, loopback HTTP support, no ambient proxy, no redirect following, bounded responses and probes, typed redacted errors, and no silent route fallback.
- Treat HTTP 202 or any other non-final response as pending or unavailable, never as positive capability evidence. Polling is a separate feature.
- Keep credentials and provider response bodies out of settings, logs, persisted route records, IPC, and `Debug` output.
- Existing settings syntax remains readable. The new `model_source` defaults to `list`; `models` retains allowlist semantics there and becomes the explicit candidate list in `configured` mode. Host-only base URLs need a documented migration to an API base.
- Remove production `NimConfig`, `NimDiscovery`, `NimTransport`, and `ReqwestNimTransport` types and their tests after migration. Keep an optional NVIDIA hosted API preset as a convenience for entering its URL; it uses exactly the same connector and must ask for a model ID if `configured` mode is selected.
- Do not claim that NVIDIA's hosted API supports `GET /v1/models` without verifying that contract. A configured model ID plus probing works without that GET endpoint.
- The current agent routing policy requires both `ToolCalling` and `StructuredOutput`. Before rollout, verify the chosen hosted model can pass the strict-schema probe; if it cannot, revisit that policy in a separate design rather than claiming capability from JSON mode.

## Acceptance criteria

1. A profile using `https://api.openai.com/v1` and a profile using a different OpenAI-compatible API base both use the same connector, with no brand-specific branch in routing or inference.
2. `list` mode issues a bounded GET and probes listed candidates; `configured` mode issues no GET and probes declared candidates. A failed GET does not silently change modes.
3. A 200 with `{}`, an unrelated assistant message, invalid tool arguments, invalid schema output, an incomplete finish, or a 202 response grants no capability evidence.
4. Two authenticated profiles never exchange credentials during listing, probes, or selected inference. A missing key causes no request to its endpoint.
5. Existing profile configuration parses, a versioned API base joins endpoints exactly once, and the optional NVIDIA preset uses the generic flow.
6. Focused tests pass, followed by `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, and `cargo nextest run --workspace` before a PR.

## Official API references checked through Context7 (2026-09-16)

- [OpenAI Models list](https://developers.openai.com/api/reference/resources/models/methods/list) documents `GET /v1/models` and `data[*].id`. [Chat Completions](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create) requires a model ID and documents tool-call responses. These are separate operations; the connector therefore does not require listing when IDs are configured explicitly.
- [OpenAI Structured Outputs](https://developers.openai.com/api/docs/guides/structured-outputs) distinguishes strict `json_schema` from `json_object` JSON mode. [Chat Completions parameters](https://developers.openai.com/api/reference/python/resources/chat/subresources/completions/methods/create) document `max_completion_tokens` and deprecate `max_tokens` for newer models.
- [NVIDIA hosted API reference](https://docs.api.nvidia.com/nim/reference/nvidia-nemotron-3-ultra-550b-a55b-infer) shows the OpenAI client pointed at `https://integrate.api.nvidia.com/v1` with a bearer key, model ID, and Chat Completions. The official hosted references returned by Context7 did not establish `GET /v1/models`; configured IDs avoid relying on that unverified endpoint.
