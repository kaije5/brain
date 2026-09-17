# Linux vault topology and Obsidian Headless Sync runbook

This runbook describes the supported Linux deployment for Brain v0.2 and the
preferred sync topology. It is a release artifact: the e2e
`required_operation_guides_are_tracked_as_release_artifacts` gate fails if it
is removed.

Scope: the runbook describes the *deployment wrapper* only. Core correctness —
parsing, confinement, concurrency, retrieval — never depends on it, and all
core provider tests run without Obsidian, systemd, or any sync subscription.

## Process topology

Exactly one `cortexd` daemon owns the vault and the SQLite runtime database
(single-owner rule, ADR-017). Everything else is a client over the local IPC
endpoint.

```text
Brain CLI/TUI ──┐
MCP gateway ─────┼─ local IPC ─> cortexd.service ──> vault root (Markdown)
Obsidian ────────┘                                     (single owner: cortexd)
```

The daemon is replaceable: stop it, swap the binary, start it again. The
derived vault index is rebuildable from Markdown alone, so a new process
reconstructs all derived state at startup (`refresh_vault_index`).

## systemd units

`/etc/systemd/system/cortexd.service` (user services are equivalent with
`systemctl --user`; keep the unit type and restart policy):

```ini
[Unit]
Description=Brain Cortex daemon (single vault owner)
After=network-online.target

[Service]
Type=simple
# Replace with the deployed binary path and the configured workspace name.
ExecStart=/usr/local/bin/cortexd
Restart=on-failure
RestartSec=2
# The daemon stores pairing keys and the runtime database under the user
# profile; give it a private home so parallel logins cannot race it.
PrivateTmp=true
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
```

Operate with:

```sh
systemctl daemon-reload
systemctl enable --now cortexd.service
systemctl status cortexd.service   # process health
brain --output json doctor
```

## Preferred sync: Obsidian Headless Sync

The preferred topology keeps sync entirely outside the daemon: a second,
independent unit runs Obsidian's headless sync client against the same vault
directory. The daemon never talks to the sync transport — it watches the
filesystem and reconciles (normalized change events + periodic reconciliation),
which keeps the sync product replaceable.

`/etc/systemd/system/obsidian-headless-sync.service`:

```ini
[Unit]
Description=Obsidian headless sync client for the Brain vault
# Start after the vault owner so the first sync pass sees a settled root.
After=cortexd.service

[Service]
Type=simple
# Replace with the headless sync invocation for your subscription; it must
# target the same vault root that cortexd was configured with.
ExecStart=/usr/local/bin/obsidian-headless-sync --vault /srv/brain/vault
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

### Sync outage behavior (required semantics)

- A sync outage never blocks local operation: reads, writes, search, and
  planning keep working against the local Markdown.
- After the outage the next reconciliation pass folds missed transitions
  (debounced events + `reconcile_vault`); the derived index rebuilds from
  Markdown alone, so missed events cannot corrupt it.
- Freshness is explicit: `brain doctor` / `brain status` report
  `vault.fresh`, `vault.root_accessible` (live probe at diagnostics time),
  `vault.index.refreshed_at` and the per-refresh counters
  (`indexed`/`unchanged`/`removed`/`skipped`). `fresh: false` means the last
  refresh failed or the root is currently inaccessible — treat remote state
  as unknown until a refresh succeeds.
- Conflicts are never merged silently: writers perform revision checks and
  conflicting updates fail with typed `conflict` errors; sync copies that
  duplicate a `brain_id` surface as typed duplicate-identity errors (see
  `docs/operations/backup-restore.md` and the adversarial vault tests).

## Backup and rebuild quick reference

- Backup: quiesce the vault owner first (`systemctl stop cortexd`), copy the
  vault root and the SQLite database file, then start the daemon again. Full
  procedure: `docs/operations/backup-restore.md`.
- Index rebuild: `brain doctor` shows index state; a full rebuild is one
  daemon operation (`refresh_vault_index`), exercised by the
  `index_rebuild_restores_retrieval_after_full_index_loss` test.
- Conflict handling: concurrent edits resolve by revision checks with typed
  `conflict` errors; recovery is documented in `docs/operations/diagnostics.md`.
