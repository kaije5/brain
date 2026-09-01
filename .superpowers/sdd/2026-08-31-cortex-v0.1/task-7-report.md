# Task 7 report: FTS, embedding storage, and deterministic hybrid search

## Status

Implementation is complete and scoped to Task 7. `cortex-search` now provides the
approved search request/result contract and embedding provider port. The SQLite
adapter provides the minimal FTS, embedding, provenance, workspace, lifecycle,
and capability-filtered repository operations required by that service. Task 8
inference and agent behavior was not started.

The current Windows host has a verification gap: Windows application control
(`os error 4551`) blocks selected freshly linked Rust executables and native build
helpers before they run. The implementation is committed for review, but the
full runtime gate must be rerun on an approved runner before Task 7 is accepted.

## Implemented behavior

- Added the `cortex-search` workspace crate with:
  - `SearchRequest`, `SearchHit`, and generic `HybridSearchService`;
  - the provider-neutral async `EmbeddingProvider` port;
  - finite, non-empty, model-tagged embeddings with exact little-endian `f32`
    blob encoding/decoding and dimension validation;
  - deterministic cosine ordering with entity-ID tie breaking;
  - deterministic reciprocal-rank fusion using `k = 60`, one-based ranks, and
    entity-ID tie breaking;
  - bounded semantic scans and an end-to-end semantic timeout;
  - lexical degraded mode for embedding unavailability and timeout;
  - authorization before any lexical/provider work.
- Extended SQLite storage with:
  - canonical `search_document` metadata and an external-content FTS5 table;
  - insert/update/delete triggers that keep FTS content synchronized with search
    metadata;
  - a schema-level `vector length == dimensions * 4` constraint;
  - a composite embedding foreign key to indexed content;
  - typed search entity kinds and candidate records;
  - exact knowledge-capability grant lookup;
  - canonical-entity validation before indexing;
  - FTS and embedding queries that filter by workspace, principal grant,
    lifecycle, active memory status, and active provenance sources before
    returning candidates;
  - deterministic SQL ordering and SQL `LIMIT` bounds.
- Preserved provenance in lexical, semantic, and fused hits. Memory candidates
  cite active `memory_source` rows; source candidates cite themselves.
- Kept SQLx confined to `cortex-storage`; `cortex-search` depends only on the
  typed storage adapter boundary and does not add an inference implementation.

## TDD evidence

Every production behavior began with an observed failing test:

1. `tests/rank.rs` failed on the missing RRF symbol, then passed 1/1 after the
   minimal deterministic implementation.
2. `tests/vector.rs` failed on missing embedding/vector symbols, then passed 3/3
   for little-endian blobs, dimension rejection, cosine ordering, and scan
   bounds.
3. `tests/fts.rs` failed on missing FTS/search-index symbols, then passed 1/1
   against a real temporary SQLite database for workspace, grant, lifecycle,
   and provenance filtering.
4. The SQLite embedding integration test failed on missing repository methods,
   then `tests/vector.rs` passed 4/4 with real dimension-checked storage and
   bounded authorized loading.
5. `tests/degraded.rs` failed on missing service/provider symbols, then passed
   4/4 for unavailable-provider fallback, semantic timeout fallback, hybrid
   fusion including semantic-only hits, and authorization before provider use.

Tests exercise real ranking and real SQLite behavior. Provider/index fakes are
used only at the service ports to deterministically exercise unavailable,
timeout, allowed, and denied branches; assertions are on service results rather
than fake call counts.

## Verification evidence

Successful evidence captured on this worktree:

- Baseline before Task 7: `cargo test --workspace` passed 61 tests.
- `cargo fmt --check`: exit 0 on the final source tree.
- `cargo clippy -p cortex-search --all-targets -- -D warnings`: exit 0.
- `cargo clippy --workspace --all-targets -- -D warnings`: exit 0 before the
  later WDAC-triggered target-cache rebuild.
- `cargo test -p cortex-search --test rank`: 1 passed.
- `cargo test -p cortex-search --test fts`: 1 passed.
- `cargo test -p cortex-search --test vector`: 4 passed.
- `cargo test -p cortex-search --test degraded`: 4 passed immediately after its
  red/green cycle and before the final test-only FTS helper refactor.
- `cargo test -p cortex-search --no-run`: all five search test executables built.
- `git diff --check`: exit 0.
- Source scan found no production `.unwrap()`, `.expect()`, or unsafe block;
  `cortex-search` also declares `#![forbid(unsafe_code)]`.

## Environment gap

The final combined Task 7 runtime command could not complete because WDAC blocked
`tests/degraded.rs` before `main` with:

```text
Dit bestand is geblokkeerd door een beleid voor toepassingsbeheer. (os error 4551)
```

The other Task 7 binaries continued to run successfully. An isolated release
retry then failed earlier when WDAC blocked the `libsqlite3-sys` build helper with
the same error. Targeted Cargo cache rebuilding subsequently exposed missing
dependency artifacts (`E0463` for SQLx/thiserror), so no further cache or policy
changes were attempted. No Code Integrity setting was weakened.

Required acceptance rerun on an approved runner:

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Changed paths

- `Cargo.toml`
- `Cargo.lock`
- `crates/cortex-search/Cargo.toml`
- `crates/cortex-search/src/{lib,embedding,fts,vector,rank,service}.rs`
- `crates/cortex-search/tests/{fts,vector,rank,degraded}.rs`
- `crates/cortex-storage/migrations/0001_initial.sql`
- `crates/cortex-storage/src/{database,lib,repositories}.rs`
- `.superpowers/sdd/2026-08-31-cortex-v0.1/task-7-report.md`

## Concerns and follow-up

- The branch is not acceptance-ready until the full workspace format, strict
  Clippy, and runtime test gates pass on a host permitted to execute the newly
  built Rust/native artifacts.
- Search indexing is an explicit repository operation in Task 7. Later daemon
  composition must invoke it after canonical mutations; that orchestration is
  outside this task and must not bypass the same policy/audit boundary.
