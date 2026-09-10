# ADR-024: Explicit degraded state; no silent model fallback

## Decision

Cortex never substitutes another provider or model without an explicit, stored, audited policy decision. When the router finds no model satisfying a role's requirements, `cortexd` returns the typed degraded outcome `NoSuitableModel { role, reason }`, surfaces it through the CLI and diagnostics, writes an audit event for the affected command outcome, and makes no substitution. Sprint 1 contains no fallback path of any kind.

## Context

A "just use whatever works" fallback looks convenient, but for Cortex it is a security and trust defect: a silent cross-provider switch sends personal data to an endpoint the user did not select, breaks the determinism that makes routing auditable ([ADR-021](ADR-021-runtime-model-router.md)), and hides degraded operation behind apparent success. SCRUM-6 names the explicit degraded state as a Sprint 1 acceptance criterion.

## Behavior

- `NoSuitableModel` is distinct from `InferenceUnavailable`: the former means no eligible model exists in current catalog evidence; the latter means the selected provider could not be reached or failed mid-request.
- The failing agent command returns a safe, machine-readable degraded result; `brain status` and `brain doctor` report the degraded routing state with remediation hints (refresh models, verify endpoint and credential).
- Retry is explicit: a user or operator refreshes the catalog or fixes configuration. The daemon performs no background substitution or unconfigured retry.
- Recovery honors local-first degradation, consistent with [ADR-003](ADR-003-local-first.md): deterministic Cortex capabilities keep working without any model, as retrieval already works lexically without embeddings.
- Any future fallback must be an explicit configuration entry, scoped per role, and recorded in audit when it fires; it may never cross to a provider the configuration does not name.

## Rationale

Predictable, auditable behavior is worth more than uptime for a personal, policy-governed system: the user must always know which system received their words. An explicit degraded state also gives tests a single, typed condition to assert.

## Consequences and alternatives considered

Silent cross-provider or cross-model fallback was rejected for the security and determinism reasons above. Automatic retry loops were rejected beyond transport-level retries: they multiply provider calls without new evidence. The cost is that a stale catalog or an outage stops agent turns until the operator acts, which the explicit refresh command and doctor diagnostics make a small, visible burden.

## Clarification: error classification and same-selection retries (SCRUM-84)

This record is clarified to state explicitly what the typed provider error taxonomy (`ProviderFailureCategory`, `RecoveryHint`) permits:

- **Prohibited:** silently substituting another model or provider when the selected one fails. A failure never changes which endpoint receives user data.
- **Allowed:** classifying provider/inference failures into typed categories (`Timeout`, `Unavailable`, `RateLimit`, `Overloaded`, `ServerError`, `Auth`, `QuotaOrBilling`, `ContextOverflow`, `InvalidRequest`, `MalformedResponse`) and retrying the **same resolved selection** when the category's recovery hint permits it (see SCRUM-85 for bounded retry scheduling). Classification and retry never weaken the no-silent-fallback rule.
- Any future cross-model or cross-provider fallback must be an explicit, user-visible configuration change; it may never fire automatically inside error recovery.

Consumers decide recovery from the typed category/recovery hint, never by parsing provider error text, so retry policy stays deterministic and auditable.
