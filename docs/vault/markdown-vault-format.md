# Brain Markdown Vault Format — Canonical Layouts and Frontmatter

**Status:** Canonical format specification for the Brain Markdown vault (SCRUM-89 / SCRUM-100).
**Supersedes:** nothing; first formal specification.
**Implements:** the Obsidian Knowledge & Task Storage Plan (§5 Knowledge model, §6 Task model) and ADR-025/026/027.

Later subtasks implement against this document: SCRUM-97 (document parsing/serialization), SCRUM-95 (task parsing/serialization), SCRUM-105 (identity/unknown-content preservation), SCRUM-103 (adversarial round-trip fixtures). Where this document and an implementation disagree, this document wins and the implementation is a bug.

## 1. Scope and authority

The Markdown vault is the authoritative, user-owned store for knowledge documents and personal task records. This specification defines the on-disk representation Cortex reads and writes. It is deliberately a strict subset of what Obsidian accepts: every Brain-written file is a valid Obsidian file, and ordinary Obsidian usage must never produce a file Brain cannot read.

The vault is untrusted user content: parsing must be total (no panics on any input), bounded (see §8), and must never interpret document text as instructions (ADR-025).

## 2. Encoding and file conventions

| Property | Rule |
| --- | --- |
| Encoding | UTF-8, no BOM. Files with invalid UTF-8 are a typed read error, never lossily repaired. |
| Line endings | LF (`\n`) on write. CRLF is accepted on read and normalized on the next Brain write of that file. |
| Frontmatter | YAML subset (§5), delimited by a leading `---` line and a closing `---` line. |
| Extension | `.md` only. Other files are attachments, never parsed as knowledge. |
| Case sensitivity | File names are compared byte-exactly. No case-insensitive deduplication is performed. |

## 3. Directory conventions

- Knowledge documents may live anywhere under the configured vault root, except excluded paths (SCRUM-98 `exclusions`) and the Brain-managed task directory.
- Tasks live under a single Brain-managed `Tasks/` directory at the vault root.
- **One substantial task per file.** List-item tasks inside documents are user content, not Brain-managed tasks, and are never mutated by Cortex.
- Task file names follow `<brain_id>-<slug>.md` (for example `01K4EXAMPLE-finish-anatomy-assignment.md`). The file name is **presentation only**: task identity comes exclusively from the `brain_id` frontmatter property (§6.2). Renaming or moving a task file never changes task identity.
- Directory renames and moves are user operations; Cortex follows identity, not paths. Cortex never rewrites paths as a side effect of reading.

## 4. Knowledge document model

A knowledge document is any `.md` file under the root that is not a Brain-managed task file. Cortex parses and exposes, at minimum:

| Element | Source | Rules |
| --- | --- | --- |
| Path | Vault-relative path | Vault-relative, `/`-separated, UTF-8. Never exposed as an absolute path in domain types. |
| Title | `title` frontmatter property; otherwise the first ATX heading (`# `); otherwise the file stem | Bounded (§8). |
| Headings | ATX headings (`#`–`######`) in body order | Text content only, no markup. |
| Body | Everything after frontmatter | Preserved byte-for-byte on round-trip when unmodified (§7). |
| Tags | Union of the `tags` frontmatter property and inline `#tag` occurrences in the body | Deduplicated, case-preserving; frontmatter form is authoritative for Brain writes. |
| Wikilinks | `[[target]]` and `[[target\|label]]` occurrences | Target and label bounded; resolution is an index concern (SCRUM-92), not a format concern. |
| Block IDs | Trailing `^block-id` markers | Bounded identifier charset `[A-Za-z0-9-]`. |
| Attachments/references | Markdown links and image references | Recorded as raw references where supported; SCRUM-92 normalizes them. |
| Content hash | SHA-256 of the exact file bytes | Computed by the provider; never stored in the file. |
| Observed revision | Provider-assigned opaque token | Derived from content hash + mtime at read time; never stored in the file. |

Frontmatter is **optional** for knowledge documents. A plain `.md` file with no frontmatter is a valid document.

### 4.1 Reserved frontmatter properties (documents)

Brain reads but does not require: `title`, `tags`. Unknown properties are preserved verbatim (§7). Brain reserves the `brain_id` property for task files and must never write a document that carries `brain_id` without `type: task`.

## 5. Frontmatter rules (both document kinds)

- Only the YAML block form (`---` … `---`) is recognized. A `---` line later in the body is body content, never frontmatter.
- Values are scalars, sequences of scalars, or nothing else. Nested mappings in Brain-read properties are a typed parse error for that property; unknown nested mappings are preserved as-is under the unknown-property rule.
- Dates are ISO 8601 (`YYYY-MM-DD`); instants add `HH:MM` and optional `Z`/offset. Anything else is a typed error for that property.
- Booleans are `true`/`false`. Numbers are integers within §8 bounds.
- Duplicate keys within one frontmatter block are a typed `DuplicateProperty` error for Brain-managed properties; for unknown properties the last occurrence wins on read and the block is preserved verbatim on write.
- All Brain-written frontmatter is emitted in a deterministic order: Brain-managed properties first (§6.1 order), then unknown properties in their original order with their original formatting preserved.

## 6. Task file model

### 6.1 Required frontmatter

A Brain-managed task file is a `.md` file whose frontmatter carries `type: task`. Canonical property order and meaning:

```markdown
---
type: task
brain_id: 01K4EXAMPLE2X7Q9R
status: todo
priority: normal
due: 2026-09-17
deadline: 2026-09-20
duration_minutes: 60
earliest_start: 2026-09-12
split: false
project: school
context: homework
tags: [school]
---
```

| Property | Required | Type / values | Contract field |
| --- | --- | --- | --- |
| `type` | yes (marks a task file) | literal `task` | — |
| `brain_id` | yes for Brain-managed tasks | UUIDv7 text (hyphenated or compact) | stable resource identity (`ProviderResourceId`) |
| `status` | yes | `todo` \| `in_progress` \| `done` \| `cancelled` | `ProviderTaskStatus` |
| `priority` | yes | `low` \| `normal` \| `high` \| `urgent` | `ProviderTaskPriority` |
| `due` | no | date/instal (§5) | `scheduling.due_at` |
| `deadline` | no | date/instal (§5) | `scheduling.deadline_at` |
| `duration_minutes` | no | positive integer ≤ 1440 | `scheduling.duration_minutes` |
| `earliest_start` | no | date/instal (§5) | `scheduling.earliest_start` |
| `split` | no | boolean | `scheduling.split` |
| `project` | no | bounded text (§8) | `scheduling.project` |
| `context` | no | bounded text (§8) | `scheduling.context` |
| `tags` | no | sequence of scalars | document tags |

Missing `brain_id`, `status`, or `priority` in a file under `Tasks/` is a typed `MissingProperty` error — the file is never silently adopted as a Brain-managed task. Missing optional properties fall back to the documented defaults (`status: todo`, `priority: normal` apply only when the property is absent from a Brain-*written* file; on read of a foreign file they are treated as absent, not defaulted, unless the file is being adopted).

### 6.2 Stable identity

- `brain_id` is the task's `ProviderResourceId`. It is assigned once at task creation and never changes.
- Rename, move, or edit of anything else in the file does not affect identity. Identity is how Cortex survives the rename/move races of a synchronized vault (storage plan §7.1).
- `brain_id` values must be unique across the vault. Two task files with the same `brain_id` produce a typed, bounded `DuplicateIdentity` error naming neither path to clients (redaction rule: identity and classification only).
- A `brain_id` that is not valid UUIDv7 text, or exceeds §8 bounds, is a typed `InvalidIdentity` error.

### 6.3 Task body

Everything after the frontmatter is human-readable context, links, and notes. Brain-owned status lives only in frontmatter; Brain never edits task body prose, and never parses the body for scheduling data.

## 7. Preservation promises

1. **Unknown frontmatter properties are preserved.** Properties Brain does not manage are carried through create/update byte-for-byte, including original key order, quoting style, and comments within the frontmatter block. This is a hard guarantee; losing an unknown property is a release-blocking bug.
2. **Unmodified bodies are preserved byte-for-byte.** If a mutation changes only frontmatter, the body (including its exact whitespace) is untouched, and vice versa.
3. **No normalization on write of unchanged regions.** Brain never reflows, re-quotes, re-orders, or re-formats user content it did not mean to change. The only normalization is CRLF → LF on a file Brain rewrites anyway (§2).
4. **No invented content.** Brain never adds headings, separators, generated markers, or signatures to user files. If provenance must be recorded, it lives in Cortex audit state, not the file.
5. **Round-trip identity:** parse(serialize(doc)) == doc for every field in §4/§6, and serialize(parse(file)) == file for any file Brain reads without mutating.

## 8. Bounds and typed errors

All bounds are enforced before any provider dispatch; violations produce typed, value-free errors (no content, no paths in client-visible messages).

| Item | Bound |
| --- | --- |
| Frontmatter block | ≤ 8 KiB |
| Single property value | ≤ 1 KiB |
| Title / project / context | ≤ 256 bytes |
| Tag | ≤ 64 bytes; ≤ 64 tags per file |
| Heading text | ≤ 512 bytes; ≤ 512 headings per file |
| Wikilink target / label | ≤ 512 / ≤ 256 bytes; ≤ 1024 links per file |
| Block ID | ≤ 64 bytes; ≤ 1024 per file |
| `duration_minutes` | 1 – 1440 |
| Whole file | ≤ 1 MiB for parse; larger files are a typed `TooLarge` read result, not a crash |

Error taxonomy (typed, redacted): `MalformedFrontmatter`, `InvalidEncoding`, `DuplicateProperty`, `MissingProperty`, `InvalidProperty { field }`, `InvalidIdentity`, `DuplicateIdentity`, `TooLarge`. Every error names only the field/classification — never file contents, local paths, or provider diagnostics.

## 9. Round-trip fixture requirements

SCRUM-103 must cover, at minimum: an ordinary document; a document with only a body (no frontmatter); a fully-populated task; a minimal task; Unicode content (CJK, emoji, combining marks); CRLF input; frontmatter with unknown properties interleaved with known ones; malformed frontmatter (unterminated block, duplicate `brain_id`, non-ISO date, oversized value); a file with duplicate `brain_id` across two files; a task file renamed (identity stable); and a document exceeding each §8 bound. Every fixture asserts both directions of §7 promise 5.

## 10. Alternatives rejected

- **Filename-derived identity** — rejected: renames are routine in Obsidian and synchronized vaults; identity must survive them.
- **A single tasks.md list file** — rejected by the storage plan (§6.1): concurrent edits to different tasks would conflict constantly.
- **JSON sidecar metadata files** — rejected: metadata must live in the user-visible file so external editors see one source of truth; sidecars drift and are lost by sync.
- **Full YAML feature support** — rejected: anchors, multiple documents, and arbitrary nesting expand the trusted-parsing surface without user value; the §5 subset covers real frontmatter usage.
