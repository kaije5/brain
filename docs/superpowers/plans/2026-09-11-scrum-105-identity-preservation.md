# SCRUM-105 Identity and Unknown-Content Preservation Implementation Plan

**Goal:** Guarantee across renames and edits that task identity comes only from `brain_id` and that unknown frontmatter properties and body prose survive Brain rewrites byte-for-byte.

**Architecture:** New `cortex-vault` API `rewrite_task_file(existing, task) -> String`: recomposes the full file from the canonical task frontmatter (managed keys only) plus the existing document's unknown properties and verbatim body. A brain_id present in the existing file that differs from the rewritten task's identity is a typed `InvalidIdentity` error — Brain never overwrites another task's identity. Rename invariance is proven at the parse level (identity derives from `brain_id`, not the file stem or path).

**Spec:** `docs/vault/markdown-vault-format.md` §3, §6.2, §7

## Tasks

- [ ] Task 1: `rewrite_task_file` with identity-conflict detection.
- [ ] Task 2: Fixtures — rename invariance, status/priority edits preserving unknowns and body, external unknown edits preserved by the next rewrite, identity-conflict rejection, brain_id spelling normalization on rewrite.
