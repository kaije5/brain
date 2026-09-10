# hermes-agent vs cortex inference — comparison

Reference: [hermes-agent](https://github.com/NousResearch/hermes-agent) (Python, cloned at
`hermes-agent/` in this workspace) compared against our Rust workspace
(`crates/cortex-inference`, `apps/cortexd`). Written 2026-09-10 as the basis for epic
[SCRUM-78](https://www.atlassian.com/browse/SCRUM-78) "Priority improvements".

## 1. Request workflow, side by side

### Brain (cortex)

```
IPC request (named pipe / unix socket, authenticated)
  └─ LocalDaemon (apps/cortexd/src/ipc.rs:924)
       └─ AgentRunner::run_streaming (crates/cortex-inference/src/agent.rs:203)
            ├─ AuthorizedCapabilities::resolve (agent.rs:123)   policy-derived per turn
            ├─ ModelRouter::select (routing.rs:257)             deterministic, capability evidence
            └─ OpenAiCompatibleTransport (openai_compatible.rs) single shot
                 ├─ no proxy, no redirects (prompt-leak protection)
                 ├─ optional Bearer from keyring, resolved once
                 ├─ 5 s fixed timeout, 64 KiB cap
                 └─ "streaming" = chunk callback over JSON responses (not SSE)
```

Properties: bounded loop (`AgentLimits`: max iterations, deadline, duplicate-call-id cap,
byte caps), strict tool-call schema validation, no retries, no fallback (ADR-024),
redacted degradation reasons (`settings.rs:293`), fast startup via background model
resolution (SCRUM-76).

### hermes-agent

```
AIAgent turn pipeline (~40 agent/turn_*.py modules around run_agent.py:211)
  ├─ ProviderProfile (providers/base.py)      declarative: api_mode, auth_type, env vars,
  │                                           base_url, quirks — ~30 providers in a registry
  ├─ API call (agent/turn_api_call.py)
  ├─ error → ClassifiedError (agent/error_classifier.py, ~20 FailoverReasons)
  │        └─ recovery hints: retry / rotate-credential / fallback / compress / abort
  ├─ resilience ladder
  │    1. jittered retry, Retry-After aware (agent/retry_utils.py)
  │    2. same-provider credential pool rotation (agent/credential_pool.py)
  │    3. explicit cross-provider fallback chain + exponential cooldown,
  │       anti-oscillation dead-marking (agent/fallback_cooldown.py)
  ├─ deadlines: per-provider request + stale timeouts resolved by agent/deadline.py
  └─ streaming: real SSE, single-writer, stale-stream + empty-response guards
```

## 2. What brain does better (keep)

- **Security boundary.** `ReqwestOpenAiTransport` is hard-wired `.no_proxy()` +
  `redirect::Policy::none()` with tests (openai_compatible.rs:398); hermes uses standard
  reqwest defaults. Our transport also rejects credentials in URLs and cleartext remote
  HTTP (openai_compatible.rs:78-131).
- **Capability-evidence routing.** `ModelRouter::select` is a pure, deterministic function
  over probed capability evidence with freshness bounds (ADR-021/023). Hermes selects
  models by config + auto-detection with no capability proofs.
- **Bounded agent loop.** Hard iteration/deadline/byte caps; hermes relies on softer
  guards.
- **Secret hygiene.** `SecretRef` locators, keyring-only storage, zeroize-on-read;
  hermes stores OAuth/pool state in plaintext JSON (`~/.hermes/auth.json`).

## 3. Gaps → stories

| Gap in brain | hermes reference | Story |
|---|---|---|
| No system prompt / compaction; conversation accumulates inside one bounded run | three-tier prompt (`agent/system_prompt.py`), compaction (`context_compressor.py`, `turn_overflow.py`) | SCRUM-79 |
| "Streaming" is chunk callbacks over non-streaming HTTP | real SSE + stale-stream monitoring (`stream_single_writer.py`, `chat_completion_stream_monitor.py`) | SCRUM-80 |
| Keyring store is Windows-only; non-Windows returns `Internal` | cross-platform secrets (albeit weaker hygiene) | SCRUM-81 |
| One hardwired wire protocol (OpenAI-compatible) | declarative `ProviderProfile`s, pluggable adapters per `api_mode` | SCRUM-82 |
| No product-level design philosophy doc; TUI minimal | narrow-waist core, mid-session `/model`, single-writer rendering, visible degradation status | SCRUM-83 |
| Coarse error taxonomy, no recovery hints | `FailoverReason` → `ClassifiedError` pipeline | SCRUM-84 |
| Single-shot on transient failures | decorrelated jittered backoff + `Retry-After` | SCRUM-85 |

Also noted (no story yet): persisted capability evidence should be loaded at startup
instead of re-probing NIM discovery every boot (`SqliteModelRoutingStore` exists);
persistent sessions; per-provider timeout config replacing the fixed 5 s.

## 4. ADR-024 nuance

ADR-024 ("no silent model fallback") bans substituting a different model without telling
the user. It does **not** ban retrying the same model, nor error classification.
SCRUM-84/85 are compatible with it; an *explicit, user-visible* fallback chain (hermes'
third layer, minus the credential pool) could be added later behind loud UX.
