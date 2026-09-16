# OpenAI-Compatible Model Connector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the NIM-specific inference path with a general OpenAI-compatible connector that supports explicit model candidates or API listing, capability probes, and secure model routing.

**Architecture:** A validated API base and one JSON transport serve model listing, probes, and the existing Chat Completions inference adapter. Each profile chooses `list` or `configured` candidates explicitly. The daemon resolves credentials per profile and carries the selected route's credential into inference.

**Tech Stack:** Rust, Tokio, Reqwest, Serde JSON, Cargo nextest.

**Spec:** `docs/superpowers/specs/2026-09-16-openai-compatible-discovery-design.md`

## Global Constraints

- `api_mode = "openai_completions"` is the only supported wire mode; unknown modes are rejected.
- Use the supplied API base path exactly: join `models`, `chat/completions`, and `embeddings`; never invent `/v1`.
- Default `model_source` to `list`; configured candidates require a nonempty `models` array and never trigger GET.
- Preserve HTTPS for remote endpoints, loopback HTTP, bounded responses, no proxy, no redirects, redacted errors, and explicit degradation.
- Never send one profile's secret to another profile's endpoint.
- Require observed tool calls and strict-schema output for their respective capability evidence; HTTP success alone is insufficient.
- Before a PR, run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, and `cargo nextest run --workspace`.

---

## File map

- `crates/cortex-inference/src/openai_compatible.rs`: validated API base, shared GET/POST transport, existing inference adapter.
- `crates/cortex-inference/src/discovery.rs`: candidate listing and bounded capability probes; replaces `nim.rs`.
- `crates/cortex-inference/src/lib.rs`: generic exports and removal of NIM exports.
- `apps/cortexd/src/settings.rs`: typed `model_source`, profile quirks, candidate selection and route result.
- `apps/cortexd/src/main.rs`, `apps/cortexd/src/lib.rs`: per-profile secret resolution and selected bearer installation.
- `apps/brain/src/tui/mod.rs`, `apps/brain/src/tui/view.rs`, `apps/brain/src/tui/runner.rs`: generic provider setup and optional hosted API preset.
- `crates/cortex-inference/tests/discovery.rs`, `apps/cortexd/tests/local_model_resolution.rs`, `apps/cortexd/tests/local_settings.rs`, `apps/brain/tests/settings_editor.rs`: focused contracts.
- `docs/operations/local-setup.md`, `docs/adr/ADR-022-nvidia-nim-first-provider.md`: current instructions and historical note.

Do not change knowledge/task provider contracts or vault code. Assign a Jira story key before implementation and use its feature branch/worktree; this documentation branch is not the implementation branch.

### Task 1: Consolidate API base and HTTP transport

**Files:** Modify `crates/cortex-inference/src/openai_compatible.rs`; test `crates/cortex-inference/tests/provider.rs` and create `crates/cortex-inference/tests/discovery.rs`.

**Interface:** Extend `OpenAiTransport` with `get_json` matching its `post_json` security and size contract. Add a shared validated API-base type or helper for relative endpoint joining; `OpenAiCompatibleConfig` and discovery consume the same helper.

- [ ] **Step 1: Write failing endpoint/GET tests.** Verify `/v1`, a custom nested API path, no implicit `/v1`, remote HTTP rejection, keyless and bearer GET, no redirect follow, bounded body, and typed redacted failures.

  ```rust
  assert_eq!(api_base("https://api.openai.com/v1").models(),
             "https://api.openai.com/v1/models");
  assert_eq!(api_base("https://example.test/custom/api").chat(),
             "https://example.test/custom/api/chat/completions");
  // A loopback HTTP fake must receive GET /models and only its own bearer.
  ```

- [ ] **Step 2: Run `cargo nextest run -p cortex-inference --test provider --test discovery`; verify the new assertions fail.**
- [ ] **Step 3: Implement the shared helper and GET transport.** Reuse the existing Reqwest client with `.redirect(Policy::none()).no_proxy()`, bearer-header handling, bounded body reader, and error classifier. Preserve current inference and embedding request encoding.

  ```rust
  async fn get_json(&self, endpoint: &str, bearer: Option<&str>,
      timeout: Duration, max_response_bytes: usize) -> Result<Vec<u8>, ProviderError>;
  ```

- [ ] **Step 4: Rerun the focused tests and commit with a Conventional Commit referencing the assigned story key.**

### Task 2: Implement candidate sources and capability evidence

**Files:** Create `crates/cortex-inference/src/discovery.rs` and `crates/cortex-inference/tests/discovery.rs`; modify `crates/cortex-inference/src/lib.rs`; remove `crates/cortex-inference/src/nim.rs` and `crates/cortex-inference/tests/nim.rs` after caller migration.

**Interface:** `OpenAiCompatibleDiscovery<T: OpenAiTransport>` accepts a validated API base, timeout, typed quirks, and `ModelCandidates` (`List { allowlist }` or `Configured(Vec<ModelId>)`). `refresh(bearer: Option<&str>) -> Result<ModelCatalog, ApplicationError>` returns normalized evidence and never silently changes candidate source.

- [ ] **Step 1: Write failing candidate tests.** In list mode parse bounded `data[*].id`, reject malformed or oversized responses, and apply the configured allowlist before probing. In configured mode probe only declared IDs and assert zero GET requests. Assert a failed list GET does not use configured IDs.

  ```rust
  assert_eq!(list_fake.get_count(), 1);
  assert_eq!(configured_fake.get_count(), 0);
  assert_eq!(configured_fake.probed_models(), ["model-a"]);
  ```

- [ ] **Step 2: Write failing evidence tests.** A 200 with `{}`, wrong tool name, malformed arguments, schema mismatch, `finish_reason = "length"` or `content_filter`, and a 202 pending response grant no capability. A complete `cortex_probe` call grants `ToolCalling`; a separate no-tools strict `json_schema` response matching the schema grants `StructuredOutput`. Check request payload and response budgets, 128-model cap, probe concurrency 8, and at most 128 output tokens.

  ```rust
  assert!(!probe_tool_calling(br#"{}"#).await?.demonstrated());
  assert!(probe_tool_calling(valid_cortex_probe_call()).await?.demonstrated());
  assert!(!probe_structured_output(json_object_only_response()).await?.demonstrated());
  ```

- [ ] **Step 3: Run `cargo nextest run -p cortex-inference --test discovery`; verify the new tests fail against the old implementation.**
- [ ] **Step 4: Move generic list parsing and probes into `discovery.rs`.** Reuse Task 1's transport, add `ModelCandidates`, parse final response content before recording evidence, and keep typed failures. Implement typed `ProbeTokenLimitField::{MaxTokens, MaxCompletionTokens}`; each request sends exactly one field, with no automatic retry under another name.

  ```rust
  const MAX_DISCOVERED_MODELS: usize = 128;
  const PROBE_CONCURRENCY: usize = 8;
  const PROBE_MAX_TOKENS: u32 = 128;
  // Only a matching final response records timestamped capability evidence.
  ```

- [ ] **Step 5: Rerun focused tests; migrate exports and remove NIM-specific production types and tests; commit.**

### Task 3: Wire typed profiles and isolate credentials

**Files:** Modify `apps/cortexd/src/settings.rs`, `apps/cortexd/src/main.rs`, `apps/cortexd/src/lib.rs`, `apps/cortexd/tests/local_settings.rs`, and `apps/cortexd/tests/local_model_resolution.rs`. Inspect `apps/cortexd/src/platform_secret_store.rs` for the existing zeroizing secret reader.

**Interface:** Parse `model_source = "list" | "configured"` and `probe_token_limit_field = "max_tokens" | "max_completion_tokens"`. Replace `resolve_default_model(..., bearer: Option<&str>)` with an injected per-`SecretRef` lookup. `ModelResolution::Configured` carries a redacted, zeroizing bearer for the routed profile alongside its config and route.

- [ ] **Step 1: Write failing settings tests.** Existing files default to list mode. Configured mode without `models` is invalid; unknown source and token field are invalid. Explicit API bases retain their path. Existing `models` remains a list-mode allowlist.
- [ ] **Step 2: Write failing two-profile tests.** Distinct authenticated endpoints receive only their own credentials for GET and probes; the second profile's selected route installs only its credential. Missing credentials cause zero requests to that profile. Keep a keyless loopback test.

  ```rust
  assert_eq!(alpha_requests.bearers(), ["alpha-token"]);
  assert_eq!(beta_requests.bearers(), ["beta-token"]);
  assert_eq!(selected_route.profile_id.as_str(), "beta");
  assert_eq!(installed_bearer, "beta-token");
  ```

- [ ] **Step 3: Run `cargo nextest run -p cortexd --test local_settings --test local_model_resolution`; verify failures.**
- [ ] **Step 4: Implement profile parsing and per-profile secret lookup.** Use `PlatformSecretStore.resolve_value` in production and an injected fake in tests. Wrap owned secret material in a zeroizing type with custom redacted `Debug`; carry only the selected profile's value into `install_resolved_model`. Never persist or log it.

  ```rust
  pub struct ResolvedBearer(zeroize::Zeroizing<String>);
  // Debug prints ResolvedBearer([REDACTED]).
  // lookup: Fn(&SecretRef) -> Result<ResolvedBearer, ApplicationError>
  ```

- [ ] **Step 5: Rerun focused daemon tests, including explicit Disabled/Degraded outcomes; commit.**

### Task 4: Make setup generic and remove vendor coupling

**Files:** Modify `apps/brain/src/tui/mod.rs`, `apps/brain/src/tui/view.rs`, `apps/brain/src/tui/runner.rs`, `apps/brain/tests/settings_editor.rs`, `docs/operations/local-setup.md`, and `docs/adr/ADR-022-nvidia-nim-first-provider.md`.

- [ ] **Step 1: Write failing setup tests.** A user can enter any OpenAI-compatible API base, choose list or configured candidates, and enter model IDs when configured. A hosted API preset only pre-fills URL and ordinary profile fields; it invokes no vendor-specific runtime code and does not promise that a key alone discovers models.
- [ ] **Step 2: Run `cargo nextest run -p brain --test settings_editor`; verify failures.**
- [ ] **Step 3: Update settings UI and current setup docs.** Explain API base URLs, the two candidate sources, capability probing, typed token field, and the need for a model ID when listing is unavailable. Add a superseding note to ADR-022 without rewriting historical text.
- [ ] **Step 4: Search production source for `NimConfig|NimDiscovery|NimTransport|ReqwestNimTransport`; require zero matches.** Retain a brand name only for an optional endpoint preset and historical docs.
- [ ] **Step 5: Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, and `cargo nextest run --workspace`. Fix actual failures, commit, then report the exact Git and CI state before a PR.**
- [ ] **Step 6: Before rollout to a hosted endpoint, verify the chosen model passes both probes using that profile's documented parameters.** If it lacks strict-schema output, report explicit degradation and seek a separate routing-policy decision; do not mark JSON mode as schema support.

## Plan self-review

- Both candidate sources end in the same probes and router, with no implicit fallback.
- Explicit API base paths, typed field quirks, credential isolation, and degraded states are covered by tests.
- Current profile files remain parseable; host-only API bases are called out for migration.
- The optional hosted API preset uses the same connector; no self-hosted NIM behavior is part of the plan.
- The hosted-model compatibility gate protects the current agent policy, which requires tool calling and structured output.
