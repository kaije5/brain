# Backup and restore

## What to copy

Cortex v0.1 has no `brain export` command and no cloud backup service. A backup
is a local file-level copy of the complete private data directory after the
daemon has stopped. Copying the directory preserves the SQLite database,
migration state, discovery record, owner pairing enrollment, and any remote
principal enrollment/manifest sidecars that belong together.

Do not treat a single live `SQLite` database file as a complete backup. While
`cortexd` is running, SQLite may have WAL sidecars and the enrollment artifacts
also remain necessary for the same local identity. Do not copy a live database
or edit it with an arbitrary SQLite tool.

## Create a consistent local backup

1. Stop `cortexd` with `Ctrl+C` and wait for it to exit.
2. Copy the entire private directory to a separately protected backup location.
3. Keep the backup readable only by the intended owner and protect it like an
   enrollment key: it contains personal data and private pairing material.

For the paths used in the local setup guide:

```powershell
Copy-Item -Path 'C:\CortexData' -Destination 'D:\CortexBackups\CortexData-2026-09-08' -Recurse
```

Do not put the backup in a public repository or send pairing/enrollment files
through chat, tickets, or email. A copied model secret reference is only an
opaque locator; restore access still depends on the platform secret store.

## Restore without overwriting a live instance

Restoration is safest into a new, empty private directory. First stop any daemon
using the target database. Then copy the backed-up directory and point
`CORTEX_DATABASE` at the restored `cortex.db` file before starting `cortexd`.

```powershell
New-Item -ItemType Directory -Force -Path 'C:\CortexRestore' | Out-Null
Copy-Item -Path 'D:\CortexBackups\CortexData-2026-09-08\CortexData\*' -Destination 'C:\CortexRestore' -Recurse
$env:CORTEX_DATABASE = 'C:\CortexRestore\cortex.db'
cargo run -p cortexd
```

In another shell with the same `CORTEX_DATABASE`, run `brain status` and a
non-mutating search before using the restored state. The daemon applies its
ordered migrations before serving; do not manually alter migration tables.

If an existing deployment must be replaced, preserve the old directory as a
separate rollback copy first. This guide intentionally does not include a
destructive delete command.

## Recycle-bin behavior

Normal entity deletion is recoverable. Deleted notes, tasks, and memories are
omitted from ordinary queries; an authorized restore returns the canonical
record to active lifecycle and is audited. Deletion and restore are not backup
substitutes, and v0.1 has no irreversible purge. Keep a backup before broad
lifecycle changes or before testing an MCP client's destructive grants.
