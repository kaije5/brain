# Diagnostics and safe incident handling

## Safe local checks

With `cortexd` running and `CORTEX_DATABASE` set to the same database path,
these commands use the authenticated local client:

The executable forms are shown in full below; the examples use `cargo run` so
they work from a checkout.

```powershell
cargo run -p brain -- status
cargo run -p brain -- doctor
cargo run -p brain -- logs
cargo run -p brain -- --output json status
```

Today the daemon returns a bounded diagnostic envelope for `status`, `doctor`,
and `logs`: requested capability, correlation ID, workspace ID, authenticated
principal ID, and whether migrations were applied. The logs command is not a
raw log-file tailer. It is intentionally safe to run remotely because it does not
return filesystem paths, pairing material, SQLite errors, prompt text, notes,
memory text, credentials, or raw trace data.

Record the correlation ID and the redacted result category when seeking help.
Do not attach the discovery record, pairing/enrollment file, gateway JSON,
OIDC token, client private key, or a database copy to a public issue.

## Common redacted outcomes

- `discovery_unavailable` or `enrollment_unavailable`: verify that the CLI uses
  the same `CORTEX_DATABASE` path and OS account that initialized the daemon.
- `transport_unavailable` or `timeout`: start `cortexd`, then retry the bounded
  request; do not bypass IPC by opening SQLite directly.
- `invalid_input`: correct the CLI's command arguments or its bounded limit.
- `permission_denied`: the principal lacks the capability; inspect the trusted
  provisioning/policy decision rather than forging a principal ID.
- Gateway `cortex_unauthorized`, rate-limit, or tunnel-unavailable outcomes:
  verify the paired OIDC subject, relay trust, and protected local files without
  logging their contents.

## Model-offline operation

The local OpenAI-compatible provider is optional. If it is absent or offline,
the agent command may return `unavailable`, and semantic embeddings cannot be
refreshed. Lexical search remains available; for example:

```powershell
cargo run -p brain -- --output json memory search 'Cortex' --limit 20
```

Search output marks a semantic leg as degraded where applicable. Diagnose the
provider by checking the configured base URL/model pair and opaque secret
reference presence in the daemon environment, never by printing a secret.

## Vault health in doctor and status

`brain doctor` and `brain status` include a secret-free `vault` block
(SCRUM-133). It reports configuration facts and derived-index state only —
never vault content, principal identities, or credentials:

- `configured`, `provider_id`, `root`, `mode`, `scopes`: the resolved vault
  configuration exactly as the daemon opened it.
- `root_accessible`: the configured root is present and usable right now.
  `false` usually means a sync client owns a stale lock or the mount is gone.
- `fresh`: the outcome of the last derived-index refresh. `false` means the
  last rebuild failed — the previous index stays in place and remote state
  must be treated as unknown until a refresh succeeds (see
  `docs/operations/linux-vault-topology.md` for the sync-outage runbook).
- `index`: counters from the last refresh — `refreshed_at`,
  `indexed`/`unchanged`/`removed` (reconciler output) and `skipped`
  (documents that failed bounded parsing; always a typed count, never a
  crash).
- `semantic`: `available` when a model embedding provider is configured,
  `degraded` when retrieval runs lexical-only.

## Release evidence

Run the documented-command contract and the wider release checks from the
workspace root:

```text
cargo test -p cortex-e2e --test documented_commands
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git diff --check
```

The tests use local fakes for model, OIDC, and relay integration. They do not
contact a live model, identity provider, public relay, or ChatGPT account.
