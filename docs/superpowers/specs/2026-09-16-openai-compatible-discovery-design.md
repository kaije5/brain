# OpenAI-Compatible Discovery and Probing Design

## Purpose and scope

Replace the NIM-named discovery, probing, configuration, and transport path with an OpenAI-compatible path. Keep automatic model discovery, bounded capability probing, deterministic routing, explicit degradation, and the NVIDIA NIM settings preset. This is a change to the inference integration, not to the knowledge/task provider contracts.

All current model profiles use `api_mode = "openai_completions"`; unknown modes remain invalid. A profile is eligible only for its own discovered models and observed capabilities. No provider is selected by brand name.

## Decision

Use one OpenAI-compatible JSON transport for `GET /models` and `POST /chat/completions`, one discovery/probing component, and the existing `OpenAiCompatibleProvider` for runtime inference. The component is parameterized by a validated profile endpoint, timeout, typed quirks, and an injected transport. Keep the router and normalized model types unchanged.

Alternatives considered:

1. Rename `nim.rs` and its public types only. This leaves duplicate HTTP clients, different endpoint normalization, and the credential routing defect.
2. Delete discovery and require configured model IDs. This drops automatic discovery and capability evidence, breaking routing requirements.
3. Generalize the existing path and consolidate transport. This is the chosen approach because it preserves behavior while removing vendor coupling and duplicated HTTP code.

## Boundaries and data flow

1. `cortexd` reads validated provider profiles. The composition root resolves each enabled profile's `SecretRef` separately. A failed required resolution makes that profile unavailable; it never substitutes another profile's key or silently runs keyless.
2. For each enabled OpenAI-compatible profile, discovery calls the profile's `/models` endpoint and validates the bounded `data[*].id` response. A root URL and a `/v1` URL both resolve to one `/v1/models` endpoint; nested deployment prefixes stay intact. This makes the implicit API version in root URLs explicit. Existing custom root URLs that served `/chat/completions` directly require review before migration; `/v1` URLs retain their path.
3. Each admitted model receives bounded tool-calling and structured-output probes on that same profile's `/chat/completions` endpoint, using that profile's bearer and request timeout. The probe output budget is at most 128 tokens, enough to return a minimal valid tool call or JSON object. Profile quirks, including `omit_tool_choice`, apply to probes where relevant.
4. Capability evidence is recorded only after parsing a response that actually demonstrates the capability. A successful HTTP status alone is insufficient. Rejection, malformed response, or timeout creates no positive evidence; authentication and billing failures degrade that profile.
5. The existing router selects a `{profile_id, model_id}` pair from fresh evidence and the declared-model allowlist. The selected profile's endpoint and bearer are installed together for the agent turn. No other profile's credential reaches that endpoint.

## Safety and failure behavior

- Preserve response-size, model-count, model-ID, probe-payload, timeout, HTTPS-for-remote, no-proxy, no-redirect, redacted-error, and no-silent-fallback protections.
- Keep provider responses and raw credentials out of logs, SQLite, IPC, and `Debug` output. The route record stores IDs only.
- Preserve explicit `Disabled` and `Degraded` outcomes. The default profile must be valid and discoverable before routing; other enabled profiles may fail independently without hiding eligible alternatives. Authentication failure on any profile must not cause its credential to be reused elsewhere.
- A provider that lacks `/models` cannot participate in automatic routing under this refactor. Supporting declared-only catalogs would require a separate design.

## Compatibility and cleanup

- Existing `cortexd.toml` profiles, including `[models.profiles.nim]`, remain valid. `nim` is a profile ID and UI preset, not an API mode.
- Profiles with root-only `base_url` may need an explicit versioned API base to preserve a nonstandard unversioned endpoint. Document this migration in local setup instructions rather than guessing a different endpoint or silently retrying another path.
- Remove public `NimConfig`, `NimDiscovery`, `NimTransport`, and `ReqwestNimTransport` after migrating in-repository callers and tests. No backward-compatibility aliases are needed for this clean cutover.
- Update current operational documentation and add a short superseding note to ADR-022. Preserve ADR-022 and the Sprint 1 threat model as historical records.

## Acceptance criteria

- NIM-hosted and a non-NVIDIA OpenAI-compatible test endpoint both discover and probe through the same implementation; root and `/v1` base URLs produce correct requests.
- An endpoint returning HTTP 200 with `{}` or unrelated assistant content cannot gain tool-calling or structured-output evidence.
- Two authenticated profiles with distinct secrets send only their own bearer on discovery, probes, and the selected inference route; a missing secret never sends a request or borrows another key.
- The existing router continues to filter by profile provenance, enabled state, declared models, and fresh capability evidence. Unavailable providers yield an explicit degraded state without fallback.
- No `Nim*` or `ReqwestNimTransport` production types remain. The NVIDIA preset and existing user configuration still work.
- Focused tests pass, followed by `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, and `cargo nextest run --workspace` before any PR.
