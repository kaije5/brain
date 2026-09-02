# Task 8 report: local OpenAI-compatible inference and bounded agent loop

## Status

Task 8 is complete and scoped to `cortex-inference`. No daemon, CLI, MCP, or
Task 9+ behavior was added.

## Implemented behavior

- Added native-async provider-neutral inference request, response, message,
  tool-call, and `InferenceProvider` contracts.
- Added a configured OpenAI-compatible adapter with:
  - loopback-only HTTP/HTTPS endpoint validation;
  - bounded model identifiers and non-zero request timeouts;
  - an opaque, redacted `SecretRef` retained in configuration without exposing
    secret material to model payloads or diagnostics;
  - reusable Reqwest transport with JSON requests and safe HTTP/network error
    classification;
  - OpenAI-compatible chat/tool-call response decoding;
  - `EmbeddingProvider` implementation for OpenAI-compatible embedding
    responses.
- Added `AgentRunner<P, S>` with static generic dispatch for both
  `InferenceProvider` and `AgentCapabilityExecutor`; no native-async trait
  objects are required.
- Added a single total timeout, iteration limit, per-response tool-call bound,
  prompt/tool argument/text size bounds, call-ID validation, duplicate-call
  rejection before a second side effect, and fresh trusted operation/correlation
  IDs for each accepted tool invocation.
- Generated the model-facing tool catalog from canonical application capability
  metadata. Tool arguments are parsed into capability-specific, unknown-field
  rejecting DTOs, validated for UUIDv7/revision/date/source semantics, and only
  then normalized back to the application executor's JSON boundary.
- Malformed or empty provider output maps to `MalformedModelOutput`; transport
  timeout/unavailability maps to `InferenceTimeout`/`InferenceUnavailable`.

## TDD evidence

1. The initial provider/agent tests were written before production modules and
   failed with unresolved Task 8 imports and types.
2. The source-evidence semantic test failed because an empty memory `sources`
   array reached the service path; the minimal semantic guard then made it pass
   before service invocation.
3. The embedding adapter test failed at compile time because
   `OpenAiCompatibleProvider` did not implement `EmbeddingProvider`; the port
   implementation then made it pass.

The final crate suite contains 11 tests using fake inference/HTTP boundaries;
no test requires a live model service.

## Verification evidence

Fresh final verification on the Task 8 source tree:

- `cargo fmt --check`: exit 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0.
- `cargo test --workspace`: exit 0; 88 tests passed, 0 failed.
- `git diff --check`: exit 0.
- Production scan found no `unsafe`, `unwrap`, or `expect`; the crate declares
  `#![forbid(unsafe_code)]`.

## Changed paths

- `Cargo.toml`
- `Cargo.lock`
- `crates/cortex-inference/Cargo.toml`
- `crates/cortex-inference/src/{lib,provider,openai_compatible,agent}.rs`
- `crates/cortex-inference/tests/{provider,agent}.rs`
- `.superpowers/sdd/2026-08-31-cortex-v0.1/task-8-report.md`

## Commit

The Task 8 implementation and this report are committed together with message
`feat(inference): add bounded local agent provider boundary`. The exact commit
ID is reported by the implementing agent after the commit is created.

## Concerns

- The adapter intentionally keeps only the opaque secret reference. Resolving a
  platform credential and composing it with process startup remains daemon
  composition work; no raw credential is accepted or transmitted by Task 8.
- The first-party agent schema DTOs are adapter-owned because the application
  executor contract accepts validated JSON and the application command structs
  do not expose transport deserialization. A later transport must reuse these
  canonical capability names rather than invent a second catalog.
