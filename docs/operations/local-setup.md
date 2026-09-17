# Local setup

## Scope and prerequisites

This guide starts the two v0.1 local processes from a checked-out Cortex
workspace: `cortexd` and the `brain` client. It does not install a model server,
create an Internet-facing service, or enroll a remote MCP identity. Use the
pinned Rust toolchain selected by the workspace.

Cortex keeps all local state in one **data directory**: `cortex.db`, its
discovery/pairing artifacts, and the non-secret `cortexd.toml` settings file.
By default it is `%LOCALAPPDATA%\cortex` on Windows and
`$XDG_DATA_HOME/cortex` (or `~/.local/share/cortex`) elsewhere, so normal
usage needs no environment variable at all. The single documented override is
`CORTEX_DATABASE`, which points at the database file and thereby selects the
data directory. The following examples use Windows PowerShell and a
deliberately ordinary path; replace it with a directory private to the same
user:

```powershell
New-Item -ItemType Directory -Force -Path 'C:\CortexData' | Out-Null
$env:CORTEX_DATABASE = 'C:\CortexData\cortex.db'
cargo run -p cortexd
```

On its first successful start, `cortexd` creates and migrates the SQLite file,
then creates a discovery record and a separately protected local pairing
enrollment beside that database. The discovery record identifies the initial
workspace and **owner** principal; the private enrollment file proves local
client possession. Treat both artifacts as private local state. Do not copy the
enrollment file to another account or commit it to source control.

Stop the daemon with `Ctrl+C`. It listens only on platform-local IPC (a
same-user Windows named pipe on Windows), not on a network address.

## Use the authenticated CLI

Keep the same `CORTEX_DATABASE` value in a second shell. The CLI reads the
daemon's discovery and protected local enrollment artifacts; it does not accept
a user-supplied principal or workspace ID.

The CLI provides status, note-creation, task-addition/listing, and memory-search
operations. From an uninstalled checkout, run the complete forms below through
`cargo run -p brain --`.

```powershell
$env:CORTEX_DATABASE = 'C:\CortexData\cortex.db'
cargo run -p brain -- status
cargo run -p brain -- note create 'Ideas' 'Cortex keeps canonical local state.'
cargo run -p brain -- task add 'Review the local setup guide' --due 2026-09-30
cargo run -p brain -- task list --limit 20
cargo run -p brain -- memory search 'canonical local state' --limit 20
```

For stable automation output, place the global output option before the command:

```powershell
cargo run -p brain -- --output json status
cargo run -p brain -- --output json note search 'Cortex' --limit 20
```

Commands return a redacted category and a nonzero exit status when the daemon,
enrollment, input, or request is unavailable. They do not print database paths,
pairing keys, or raw daemon/storage errors.

## Interactive terminal session

Running `brain` with no subcommand opens one interactive full-screen session:

```powershell
cargo run -p brain
```

The default tab is agent chat; prompts reuse the daemon's bounded,
policy-checked agent loop (`cortex_agent_run`). Switch tabs with `Tab` or
`Shift+Tab`, send with `Enter`, and quit with `Ctrl+C`. `Esc` goes back to
Chat or cancels a settings prompt. Text fields accept ordinary characters,
including `q` and digits; `1`-`4` select tabs only outside text fields.
Settings prompts keep focus until confirmed or cancelled.

- **Tasks** lists canonical tasks through the same authorized IPC path as the
  CLI. Press `r` to refresh.
- **Notes** searches notes and memories. Type at least two characters and
  press `Enter`.
- **Settings** shows the local `cortexd.toml` values and the daemon's model
  status. Press `Enter` or `e` to edit, then `n` to add a profile and endpoint.
  Press `a` to add an official provider preset instead: NVIDIA NIM is
  preconfigured with the hosted `https://integrate.api.nvidia.com/v1`
  endpoint, so the API key from build.nvidia.com is the only input — the
  profile becomes the default and the key goes straight into the keyring.
  Use Up/Down to select a row: `Enter` edits the default profile or endpoint,
  `i` imports a masked token into the system keyring, `t` toggles a provider,
  and `d` requests deletion. Confirming an API key or endpoint URL saves
  immediately; press `w` to save other changes. Restart the daemon to apply.
  `Esc` asks before discarding unsaved settings; imported keyring tokens
  remain stored even if the settings draft is discarded.

Cyan marks the current selection, green indicates success, amber marks
warnings or unsaved changes, and red indicates errors. Text labels accompany
these colors. The footer shows shortcuts for the current screen.

When no eligible model is configured or the provider is unavailable, the chat
tab shows an explicit degraded state while tasks, notes, and settings keep
working; there is no silent provider fallback. The TUI is only a view over
the daemon's typed capabilities and holds no direct database access. All
one-shot CLI subcommands continue to work unchanged for scripting.

## Local settings: `cortexd.toml`

Non-secret settings live in `cortexd.toml` in the data directory. When the
file is absent, `cortexd` starts with documented defaults. Write the
commented template (it documents every supported key and never contains
credentials) with:

```powershell
cargo run -p brain -- config init
```

The template supports a `[daemon]` section (`database` file name and `endpoint`
override), a `[models] default_profile` selector, and one
`[models.profiles.<id>]` table per provider. Each profile accepts `base_url`,
`enabled`, a non-secret `secret_ref` locator, and typed SCRUM-82 keys:
`api_mode` (`openai_completions`), `auth_type` (`none` or `secret_ref`; when
omitted it is inferred from the presence of `secret_ref`), per-phase timeouts
(`connect_timeout_ms`, `request_timeout_ms`, `stale_stream_timeout_ms`), a
declared model allowlist (`models`), and typed provider `quirks`
(`omit_tool_choice`). Unknown keys — including anything that looks like a raw
credential such as `api_key` — are rejected at startup, so secrets can never
enter the config file, environment variables, or tracked files. A profile
whose auth strategy contradicts its `secret_ref` (or a zero timeout) fails
startup with a clear configuration error. Routing decisions are persisted as
typed `{profile_id, model_id}` selections; profiles with fresh capability
evidence are discovered across every enabled profile, and the router selects
deterministically with no silent fallback. Environment variables are not a
configuration channel.

## Provider credentials: keyring-backed `SecretRef`s

Provider API keys are imported into the OS secret store (Windows Credential
Manager, macOS Keychain, or a Linux Secret Service-compatible backend such as
GNOME Keyring) and referenced by non-secret `keyring:` locators:

```powershell
cargo run -p brain -- secret import --profile nim
# paste the provider API key on stdin, then press Enter
```

This stores the credential under `cortexd/nim` and prints the `SecretRef`
`keyring:cortexd/nim` to reference from a model profile:

```toml
[models]
default_profile = "nim"

[models.profiles.nim]
base_url = "http://127.0.0.1:8000/v1/"
enabled = true
secret_ref = "keyring:cortexd/nim"
```

Rotate by re-running `brain secret import --profile nim` for the same profile. Raw secrets
are never accepted on the command line, in the config file, in logs, or in
diagnostics; `cortexd` resolves `SecretRef`s through the platform secret store
at startup only.

## Model routing

On startup, `cortexd` resolves `[models] default_profile` through the
capability-aware runtime router: it discovers the profile endpoint's models,
probes their capabilities, and selects an eligible model for the agent role.
Endpoints for the chat provider must be loopback OpenAI-compatible deployments
(such as a self-hosted NIM container on `127.0.0.1`). If no profile is
configured or no eligible model is available, the daemon starts in an explicit
degraded state: deterministic capabilities (notes, tasks, memories, lexical
search) keep working and the provider failure is reported — there is no silent
provider fallback.

## Recoverable deletion and restore

## Recoverable deletion and restore

v0.1 deletion is recoverable: normal reads omit a deleted record, and restore
is an audited capability that requires the deleted record's expected revision.
There is no irreversible purge in v0.1.

The shipped `brain` CLI currently exposes note creation/search, task creation,
listing/completion, memory creation/search, and diagnostics; it deliberately
does **not** expose lifecycle delete/restore subcommands. A paired MCP client
can invoke the separately authorized `cortex_knowledge_delete`,
`cortex_task_delete`/`cortex_task_restore`, and `cortex_memory_delete`/
`cortex_memory_restore` capabilities with canonical resource ID and revision. Do not simulate deletion by editing SQLite directly.
