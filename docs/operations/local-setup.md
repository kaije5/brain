# Local setup

## Scope and prerequisites

This guide starts the two v0.1 local processes from a checked-out Cortex
workspace: `cortexd` and the `brain` client. It does not install a model server,
create an Internet-facing service, or enroll a remote MCP identity. Use the
pinned Rust toolchain selected by the workspace.

Choose a private directory owned by the current OS user. The following examples
use Windows PowerShell and a deliberately ordinary path; replace it with an
absolute path that is private to the same user:

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

The command forms are `brain status`, `brain note create`, `brain task add`,
`brain task list`, and `brain memory search`. From an uninstalled checkout, run
each form through `cargo run -p brain --` as shown below.

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

## Optional local model configuration

The deterministic CLI and lexical search work without a configured model. To
enable the supported local OpenAI-compatible provider, configure all of the
following in the daemon process environment before starting `cortexd`:

- `CORTEX_MODEL_BASE_URL` — local provider base URL.
- `CORTEX_MODEL_NAME` — provider model name.
- `CORTEX_MODEL_SECRET_REF` — optional opaque platform-secret-store reference;
  this is a locator, never a credential value.

`CORTEX_MODEL_BASE_URL` and `CORTEX_MODEL_NAME` are an all-or-nothing pair.
Never put an API key in `CORTEX_MODEL_SECRET_REF`, a command line, this file,
or an exported configuration. If the provider is unreachable, retrieval remains
available through its lexical leg and reports the semantic leg as degraded.

## Recoverable deletion and restore

v0.1 deletion is recoverable: normal reads omit a deleted record, and restore
is an audited capability that requires the deleted record's expected revision.
There is no irreversible purge in v0.1.

The shipped `brain` CLI currently exposes note creation/search, task creation,
listing/completion, memory creation/search, and diagnostics; it deliberately
does **not** expose lifecycle delete/restore subcommands. A paired MCP client
can invoke the separately authorized `cortex_note_delete`/
`cortex_note_restore`, `cortex_task_delete`/`cortex_task_restore`, and
`cortex_memory_delete`/`cortex_memory_restore` capabilities with canonical
entity ID and revision. Do not simulate deletion by editing SQLite directly.
