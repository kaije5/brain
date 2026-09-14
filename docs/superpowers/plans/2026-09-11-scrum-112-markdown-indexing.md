# SCRUM-112 Markdown Indexing for Lexical and Semantic Retrieval Implementation Plan

**Goal:** Extract supported Markdown content (body, headings, tags, links, task metadata) into bounded indexed chunks, and provide deterministic lexical-only and hybrid retrieval over the derived index — returning stable provider-backed hits with chunk/source locations and explicit semantic degradation.

**Architecture:** New `vault_retrieval.rs` module in `cortex-search` (pure, no filesystem). `index_document(resource, text, provenance)` parses via `cortex-vault`, extracts title/headings/tags/links/task metadata into a `DocumentIndexEntry` and body into `chunk_body` chunks with `ChunkProvenance`, upserting into `DerivedVaultIndex`. Retrieval over the index: `lexical` (case-insensitive containment across chunk text/title/tags, deterministic ordering), `semantic` (cosine over chunks carrying embeddings), and `hybrid` (reciprocal-rank fusion keyed by `ChunkReference`, with an explicit `semantic_degraded` flag when embeddings are unavailable). All hits carry the chunk reference and a bounded snippet — stable provider-backed hits with source locations.

**Spec:** SCRUM-92 acceptance ("lexical-only and hybrid searches return stable provider-backed hits and source locations"); storage plan §5.2

## Global Constraints

- Hits are provider/chunk-backed; no `EntityId` in the retrieval surface.
- Deterministic ranking: ties broken by chunk reference ordering.
- Bounded: queries, snippets, and result counts obey limits.
- No filesystem, Obsidian, sync, or network in tests.
- Before PR: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked`, `cargo test --workspace`, `git diff --check`.

## Tasks

- [ ] Task 1: `index_document` extraction (parse → document entry + provenance-stamped chunks).
- [ ] Task 2: `VaultHit` + lexical/semantic/hybrid retrieval over `DerivedVaultIndex` with RRF fusion and explicit degradation.
- [ ] Task 3: Tests — extraction values, lexical-only, semantic, hybrid fusion, degradation, bounds, deterministic ordering.
