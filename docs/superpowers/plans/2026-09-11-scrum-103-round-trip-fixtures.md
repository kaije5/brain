# SCRUM-103 Adversarial Round-Trip Fixture Coverage Implementation Plan

**Goal:** Cover every fixture class the format spec §9 requires, asserting both directions of §7 promise 5 (parse(serialize(doc)) == doc and serialize(parse(file)) == file) and the typed-error taxonomy for adversarial inputs.

**Architecture:** On-disk fixtures under `crates/cortex-vault/tests/fixtures/{valid,invalid}/` walked by a table-driven suite (`include_str!`), plus generated inline fixtures for size bounds (avoiding megabyte files in git). New `DuplicateIdentity` error variant and a cross-file `assert_unique_identities` helper cover the duplicate-`brain_id`-across-two-files class; rename invariance and full-task round-trips reuse SCRUM-105's `rewrite_task_file`.

**Spec:** `docs/vault/markdown-vault-format.md` §9

## Tasks

- [ ] Task 1: `DuplicateIdentity` variant + cross-file identity uniqueness helper.
- [ ] Task 2: Valid fixture set (ordinary/body-only/full-task/minimal/Unicode/CRLF/unknown-interleaved/wikilinks/foreign/renamed) with dual-direction round-trip assertions.
- [ ] Task 3: Invalid fixture set with exact typed-error mapping + generated bound-exceeding fixtures (title, tag, heading, wikilink, block ID, tags count, file size, values).
