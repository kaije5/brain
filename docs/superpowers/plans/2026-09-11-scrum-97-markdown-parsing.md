# SCRUM-97 Safe Markdown Document Parsing Implementation Plan

**Goal:** Implement safe, bounded, total parsing and serialization of vault knowledge documents exactly per `docs/vault/markdown-vault-format.md`, without filesystem access and without interpreting document content.

**Architecture:** New pure crate `crates/cortex-vault` (std + `thiserror` only; no markdown/parser libraries so every bound in format spec §8 is enforced at the single scan). `FrontmatterBlock` preserves the raw block byte-for-byte and offers typed managed-property access; `ParsedDocument` exposes title resolution (frontmatter → first ATX heading → caller-supplied file-stem fallback), headings, tags (frontmatter + inline, code spans/fences skipped), wikilinks, block IDs, and the untouched body. Serialization preserves unmodified regions byte-for-byte (spec §7), with CRLF→LF only on rewritten files. The concrete vault provider (SCRUM-91) consumes this crate; the domain/application contracts stay untouched.

**Spec:** `docs/vault/markdown-vault-format.md`

## Global Constraints

- Total parsing: no panic on any input; every failure is a typed, value-free `VaultFormatError`.
- All §8 bounds enforced during the scan (frontmatter 8 KiB, file 1 MiB, per-item and per-collection bounds).
- Round-trip: `parse(serialize(doc)) == doc`; `serialize(parse(file)) == file` for unmutated files.
- Inline scanning ignores fenced code blocks and inline code spans (Obsidian-compatible).
- No test requires Obsidian, a sync subscription, a network service, or the user's vault.
- Before PR: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, `cargo test --workspace`, `git diff --check`.

## Tasks

- [ ] Task 1: Crate skeleton + typed error taxonomy (§8) with redacted, value-free messages.
- [ ] Task 2: Frontmatter splitting and property scanning (§5 subset: scalars, sequences, duplicates, bounds) with verbatim block preservation.
- [ ] Task 3: Document model extraction (title resolution, headings, tags, wikilinks, block IDs, inline links) with fence/code-span awareness.
- [ ] Task 4: Serialization + preservation guarantees + round-trip and adversarial fixture tests.
