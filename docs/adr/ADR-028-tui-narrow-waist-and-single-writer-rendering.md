# ADR-028: TUI narrow-waist boundary and single-writer rendering

## Decision

Cortex's terminal product (Brain TUI) and every future front end are
event-driven adapters over one shared application/inference core — the
"narrow waist". Three rules hold:

1. **Shared core, thin edges.** The CLI, TUI, agent loop, and MCP surface
   all reach knowledge/task/inference behavior through the same daemon
   capabilities and typed application services. UI modules (`apps/brain/src/tui/`)
   never open the database or vault, never implement routing or inference
   logic, and never parse provider payloads.
2. **Single-writer rendering.** All terminal output for an active
   conversation flows through one serialized render path: the TUI runner's
   effect loop. Provider SSE deltas, status changes, errors, and vault
   updates are delivered as ordered events on that loop's channel; no
   concurrent producer writes the terminal directly. This guarantees
   event-order rendering with no interleaved or corrupted output.
3. **UI state never becomes model context.** Spinners, progress text,
   status lines, freshness banners, model pickers, and other render/UX
   state exist only in the TUI. They are never added to a provider request
   payload or to model context merely because they are displayed. Only
   actual user/application context (the prompt, conversation history,
   retrieved content, tool results) reaches the inference core.

## Context

hermes-agent's product layer demonstrates useful interaction patterns:
explicit model selection, corruption-free incremental streaming, and
visible provider/model state. Adopting those patterns in Cortex must not
couple product/UI state to the inference core. SCRUM-79 established the
three-tier system prompt whose Stable tier is byte-stable for prompt-cache
reuse; SCRUM-80 delivered true SSE deltas; SCRUM-82 profile-aware routing
resolves `{profile, model}`; SCRUM-84/85 provide typed errors and bounded
retries. The TUI consumes the resulting events; it does not re-implement
them.

## Behavior

- **Model selection** (`/model` interaction): shows the active model and
  the models offered by routing; a successful selection applies to
  **subsequent turns only** and preserves conversation history. The daemon
  swaps the resolved provider atomically; an in-flight turn keeps the
  provider snapshot taken at turn start, so a switch never mutates an
  in-flight request and cannot produce mixed-model output.
- **Invalid/unavailable selections** leave the active model unchanged and
  surface a typed, actionable error (`not_found` / `invalid_request` /
  `invalid_configuration`). There is no silent fallback (ADR-024).
- **Degradation visibility**: the active model, provider profile, and
  resolution/streaming/failure states are displayed without credentials,
  headers, or raw provider payloads.
- **Refresh semantics** follow SCRUM-147: prompt and model configuration
  changes affect new turns, never in-flight requests.

## Rationale

A single application core with thin event-driven edges keeps security
policy, audit, and prompt composition in exactly one place. A single
writer for terminal output eliminates a whole class of race-corruption
bugs without locking in the UI layer. Keeping UI state out of model
context protects the prompt cache (SCRUM-79) and prevents display-only
text from gaining instruction-level influence over the model.

## Consequences and alternatives considered

Embedding inference or routing logic in `apps/brain` was rejected: it
would duplicate policy-governed behavior outside the audited core. Direct
terminal writes from streaming producers were rejected: interleaved writes
corrupt the display and can leak ordering. The cost is a small event-plumbing
layer in the TUI runner; the benefit is that every surface (CLI, TUI, MCP)
stays a thin adapter over the same tested core.
