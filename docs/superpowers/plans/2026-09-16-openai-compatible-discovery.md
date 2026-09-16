# OpenAI-Compatible Discovery and Probing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the NIM-specific inference discovery path while preserving automatic discovery, evidence-based capability probing, and secure routing for OpenAI-compatible profiles.

**Architecture:** A shared JSON transport handles bounded GET and POST calls. A generic discovery component normalizes `/models` responses and probes model capabilities. The daemon resolves credentials per profile and carries the selected profile's credential with its route into the existing OpenAI-compatible inference provider.

**Tech Stack:** Rust, Tokio, Reqwest, Serde JSON, Cargo nextest.

**Spec:** `docs/superpowers/specs/2026-09-16-openai-compatible-discovery-design.md`

## Global Constraints

- Keep existing `cortexd.toml` profile syntax and the NVIDIA NIM UI preset.
- Normalize root and `/v1` bases to one `/v1` API root; document custom unversioned root endpoint migration.
- Retain the HTTPS requirement for remote endpoints; allow loopback HTTP.
- Never send one profile's bearer to another profile's endpoint.
- Never infer capability from an HTTP success status alone.
- Preserve bounded responses and probes, no ambient proxy, no redirects, typed redacted failures, profile provenance, and no silent fallback.
- Keep production NIM type names out of the final path; no compatibility aliases.
- Before a PR, run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, and `cargo nextest run --workspace`.

---

## File map and dependency order

1. `crates/cortex-inference/src/openai_compatible.rs`: shared validated endpoints, `OpenAiTransport` GET/POST, Reqwest implementation, and existing inference requests.
2. `crates/cortex-inference/src/discovery.rs`: model-list parsing, bounded probes, evidence, refresh. This absorbs `nim.rs`.
3. `crates/cortex-inference/src/lib.rs`: generic exports; remove NIM exports.
4. `apps/cortexd/src/settings.rs`: generic discovery composition, selected-route credential result.
5. `apps/cortexd/src/main.rs`, `apps/cortexd/src/lib.rs`: per-profile secret resolution, install selected bearer, remove NIM names.
6. `crates/cortex-inference/tests/discovery.rs`, `apps/cortexd/tests/local_model_resolution.rs`: behavioral and credential-isolation tests; remove `tests/nim.rs` after migration.
7. `docs/operations/local-setup.md`, `docs/adr/ADR-022-nvidia-nim-first-provider.md`: current instructions and historical decision note.

Do not modify knowledge/task provider traits or vault code. Assign a Jira story key before implementing this feature branch; the current `chore/plan-openai-compatible-discovery` branch contains documentation only.

### Task 1: Share OpenAI-compatible endpoints and transport

**Files:** Modify `crates/cortex-inference/src/openai_compatible.rs`; test `crates/cortex-inference/tests/provider.rs` and the new `crates/cortex-inference/tests/discovery.rs`.

**Interfaces:** Extend `OpenAiTransport` with `async fn get_json(&self, endpoint: &str, bearer: Option<&str>, timeout: Duration, max_response_bytes: usize) -> Result<Vec<u8>, ProviderError>`. Reuse `ReqwestOpenAiTransport` for both verbs. Add a validated endpoint helper used by `OpenAiCompatibleConfig` and discovery, producing `/v1/models` and `/v1/chat/completions` for root or `/v1` bases without doubling `/v1`.

- [ ] **Step 1: Add failing endpoint and GET transport tests.** Assert root and `/v1` bases, a nested deployment prefix, remote HTTP rejection, loopback HTTP acceptance, bearer present or absent, no redirect follow, bounded body, and safe error classification. Use a loopback `TcpListener` for HTTP assertions; do not call a live model provider.

  ```rust
  assert_eq!(endpoints("https://example.test").models(), "https://example.test/v1/models");
  assert_eq!(endpoints("https://example.test/v1").chat(), "https://example.test/v1/chat/completions");
  assert_eq!(endpoints("https://example.test/api/v1").models(), "https://example.test/api/v1/models");
  // A fake HTTP server must see GET /v1/models and only its own Bearer header.
  ```
- [ ] **Step 2: Run `cargo nextest run -p cortex-inference --test provider --test discovery`; confirm the new GET/endpoint assertions fail.**
- [ ] **Step 3: Implement the smallest shared endpoint helper and `get_json`.** Build one Reqwest client with `.redirect(Policy::none()).no_proxy()`; share bearer construction and bounded error mapping with POST. Keep `OpenAiCompatibleProvider::complete` and embedding behavior unchanged.

  ```rust
  // Inside the existing OpenAiTransport trait:
  async fn get_json(&self, endpoint: &str, bearer: Option<&str>,
      timeout: Duration, max_response_bytes: usize) -> Result<Vec<u8>, ProviderError>;
  // ReqwestOpenAiTransport uses the same client and response classifier for GET and POST.
  ```
- [ ] **Step 4: Rerun the two focused test binaries and `cargo fmt --all --check`; commit with a Conventional Commit message referencing the assigned story key.**

### Task 2: Generalize discovery and prove capabilities

**Files:** Create `crates/cortex-inference/src/discovery.rs`; modify `crates/cortex-inference/src/lib.rs`; migrate `crates/cortex-inference/tests/nim.rs` to `crates/cortex-inference/tests/discovery.rs`; delete `crates/cortex-inference/src/nim.rs` and `tests/nim.rs` after all callers move.

**Interfaces:** Export `OpenAiCompatibleDiscovery<T: OpenAiTransport>` and a validated `OpenAiDiscoveryConfig` containing the shared endpoint helper, timeout, and `ProviderQuirks`. `discover(&self, bearer: Option<&str>) -> Result<Vec<DiscoveredModel>, ApplicationError>` and `refresh(&self, bearer: Option<&str>) -> Result<ModelCatalog, ApplicationError>` retain their present contracts.

- [ ] **Step 1: Port model-list tests to neutral hosts and names.** Assert `data[*].id` validation, no duplicate `/v1`, maximum 128 entries, bounded response bytes, keyless and bearer requests, and typed failures. Add a non-NVIDIA fixture beside the NIM-hosted fixture; both must use the same component.
- [ ] **Step 2: Add failing probe-evidence tests.** A 200 response with `{}`, plain content, malformed tool arguments, or invalid structured JSON must not produce evidence. A valid tool call for the requested `cortex_probe` tool grants only `ToolCalling`; valid JSON matching the requested object constraint grants only `StructuredOutput`. A refused probe remains negative; authentication and billing errors abort that profile's refresh. Assert probe body is under 2 KiB, requests at most 128 output tokens, and `omit_tool_choice` is honored.

  ```rust
  let empty_ok = br#"{}"#.to_vec();
  let tool_ok = serde_json::json!({"choices":[{"message":{"tool_calls":[{
      "id":"probe-1","type":"function","function":{"name":"cortex_probe","arguments":"{}"}
  }]}}]}).to_string().into_bytes();
  assert!(!probe_tool_calling(empty_ok).await?.demonstrated());
  assert!(probe_tool_calling(tool_ok).await?.demonstrated());
  ```
- [ ] **Step 3: Run `cargo nextest run -p cortex-inference --test discovery`; confirm the new assertions fail against the old success-on-200 behavior.**
- [ ] **Step 4: Move parsing and bounded refresh from `nim.rs` into `discovery.rs`.** Parse the returned chat message before recording evidence. Keep model-count 128, concurrency 8, typed redacted errors, response budget, and timestamped evidence. Reuse Task 1's transport and endpoint helper; do not add vendor branches.

  ```rust
  // Positive tool evidence requires a parseable cortex_probe call with object arguments.
  // Positive structured evidence requires response content parseable as a JSON object.
  // A 2xx response without the corresponding content returns Ok(false).
  const MAX_DISCOVERED_MODELS: usize = 128;
  const PROBE_CONCURRENCY: usize = 8;
  const PROBE_MAX_TOKENS: u32 = 128;
  ```
- [ ] **Step 5: Rerun the focused tests. Remove the old NIM module, tests, and exports only after all new tests pass; commit.**

### Task 3: Resolve credentials per profile and carry the selected credential

**Files:** Modify `apps/cortexd/src/settings.rs`, `apps/cortexd/src/main.rs`, `apps/cortexd/src/lib.rs`, `apps/cortexd/tests/local_model_resolution.rs`; inspect `apps/cortexd/src/platform_secret_store.rs` and `crates/cortex-application/src/secrets.rs` for the existing resolution API.

**Interfaces:** Replace the single `bearer: Option<&str>` input to `resolve_default_model` with a lookup scoped by `ProviderProfileId` (an injected closure or trait returning an owned, redacted credential result). Make `ModelResolution::Configured` carry the selected credential alongside `{config, route, models}` in a field that has redacted `Debug`; do not derive `Debug` for raw secret values. The daemon installs only this credential. The default profile's missing secret yields explicit degradation; another profile's failure excludes that profile without sending a request.

```rust
// Suggested boundary in apps/cortexd/src/settings.rs; exact ownership may be refined in review.
pub struct ResolvedBearer(zeroize::Zeroizing<String>);
impl std::fmt::Debug for ResolvedBearer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResolvedBearer([REDACTED])")
    }
}
// resolve_default_model takes a lookup: Fn(&SecretRef) -> Result<ResolvedBearer, ApplicationError>.
// Configured returns the routed profile's ResolvedBearer, never the default's by assumption.
```

- [ ] **Step 1: Add failing two-profile tests.** Configure two distinct HTTPS endpoints and two distinct `SecretRef`s. Assert GET and both probes at each endpoint receive only that profile's bearer. Make the router select the second profile and assert the installed inference transport receives its bearer. With the second secret missing, assert zero requests to its endpoint and no fallback to the first secret. Keep a keyless loopback profile test.

  ```rust
  // Fake secret lookup: keyring:cortexd/alpha -> alpha-token,
  // keyring:cortexd/beta -> beta-token.
  assert_eq!(requests_to_alpha.bearers(), ["alpha-token"]);
  assert_eq!(requests_to_beta.bearers(), ["beta-token"]);
  assert_eq!(selected_route.profile_id.as_str(), "beta");
  assert_eq!(installed_inference_bearer, "beta-token");
  ```
- [ ] **Step 2: Run `cargo nextest run -p cortexd --test local_model_resolution`; confirm the credential-isolation assertions fail.**
- [ ] **Step 3: Resolve secrets in the daemon composition root per enabled profile.** Inject a fake credential lookup in tests, use `PlatformSecretStore` in production, and avoid storing or printing raw values in settings, logs, route records, or `Debug`. Pass each profile's credential only to its own generic discovery/probe instance. Carry the routed profile's credential into `install_resolved_model`.

  ```rust
  // In main.rs: pass the real lookup, instead of resolving only default_profile_secret.
  let resolution = resolve_default_model(settings.as_ref(), transport, |reference| {
      PlatformSecretStore.resolve_value(reference).map(ResolvedBearer::from)
  }).await;
  // On Configured, install the credential returned with that exact route.
  ```
- [ ] **Step 4: Rerun `local_model_resolution`, `local_settings`, and affected daemon IPC tests. Verify `ModelResolution::Disabled` and degraded outcomes still surface explicitly. Commit.**

### Task 4: Remove vendor coupling and update current docs

**Files:** Modify `apps/brain/src/tui/mod.rs`, `apps/brain/src/tui/view.rs` only where text describes a NIM-only adapter; modify `docs/operations/local-setup.md`, `docs/adr/ADR-022-nvidia-nim-first-provider.md`; check `apps/cortexd/src/settings.rs` template. Leave historical Sprint 1 design and threat-model text intact.

- [ ] **Step 1: Search production source for `NimConfig|NimDiscovery|NimTransport|ReqwestNimTransport` and NIM-only discovery claims.** The type-name search must return no production matches after Tasks 1–3. Preserve `nim` as the preset/profile ID and its official endpoint.
- [ ] **Step 2: Update current setup wording.** Describe generic OpenAI-compatible model discovery and probing for custom profiles and the NVIDIA preset. Add an ADR-022 note that its NIM-specific adapter description was superseded by this generic implementation, with a link to the design spec. Do not rewrite the historical decision as if it never happened.
- [ ] **Step 3: Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, and `cargo nextest run --workspace`. Investigate and fix actual failures before a PR. Commit docs/cleanup and report the exact Git state and verification results.**

## Plan self-review

- Discovery remains automatic: Tasks 1–2 preserve GET `/models`, probes, and refresh.
- Credential routing is profile-scoped: Task 3 covers discovery, probes, and selected inference.
- Existing profile syntax and NIM preset remain: Task 4 verifies them.
- Model eligibility requires parsed evidence: Task 2 tests false positives and valid demonstrations.
- Provider-neutral production names and docs are cleaned: Tasks 2 and 4.
