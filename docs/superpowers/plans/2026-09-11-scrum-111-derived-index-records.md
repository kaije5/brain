# SCRUM-111 Derived Index Records Redesign Implementation Plan

**Goal:** Redesign the derived search index records around provider resource references and chunk provenance, replacing canonical note/task table ownership: every indexed record is keyed by `ProviderResourceRef` + chunk number and carries the file's content hash and observed revision as last-indexed provenance.

**Architecture:** New `index.rs` module in `cortex-search` (pure, no filesystem). Types: `ChunkReference { resource, chunk }` (bounded), `ChunkProvenance { content_hash, observed_revision }` (both from `cortex-domain`), `IndexedChunk { reference, text (bounded), provenance, embedding: Option<Embedding> }` — embedding optional so lexical retrieval works without embeddings (spec: explicit degradation). A deterministic paragraph-boundary chunker (`chunk_body`) splits bounded bodies into ≤1200-char chunks. `DerivedVaultIndex` is an in-memory `BTreeMap<ChunkReference, IndexedChunk>` with `upsert` (hash-gated: same hash + revision → no-op), `remove_resource`, `get`, `len`. `DocumentIndexEntry` records document-level metadata (title, tags, links, task status/priority) keyed by resource. The legacy `EntityId`-keyed `SearchIndex` port stays untouched for the migration window; this is the parallel provider-backed boundary per the approved design.

**Spec:** SCRUM-92 acceptance (derived records use provider resource/chunk references; hash checks avoid unnecessary re-indexing); storage plan §5.2

## Global Constraints

- No `EntityId` in the new record types — provider references only.
- Everything bounded: chunk text, tags, chunk counts.
- Deterministic: same input → same chunking and ordering.
- No filesystem, Obsidian, sync, or network in tests.
- Before PR: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, `cargo test --workspace`, `git diff --check`.

## Tasks

- [ ] Task 1: `ChunkReference`, `ChunkProvenance`, `IndexedChunk`, `DocumentIndexEntry` types.
- [ ] Task 2: Deterministic `chunk_body` paragraph chunker.
- [ ] Task 3: `DerivedVaultIndex` map with hash-gated upsert, removal, retrieval.
- [ ] Task 4: Tests — provider-keyed identity, chunk provenance, hash-gating, removal, bounds.
