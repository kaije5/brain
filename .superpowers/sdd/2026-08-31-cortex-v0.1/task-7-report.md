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

## Review fix round 1 (2026-09-02)

The four independent-review findings were addressed without starting Task 8:

- `search_document` now stores the canonical SHA-256 digest of the exact UTF-8
  searchable text. The document upsert computes the digest internally, and a
  same-statement SQLite trigger deletes all entity embeddings when that digest
  changes. Embedding writes select the current document hash inside the insert,
  so callers cannot supply a mismatched hash; semantic reads additionally require
  `embedding.content_hash = search_document.content_hash`.
- Both lexical and semantic memory predicates now require an active
  `memory_source -> source` relation. Candidate citation loading continues to
  return only active sources.
- The persistence boundary now decodes the exact declared little-endian `f32`
  payload through the validated embedding contract before setting `ready`.
  Empty/dimension-mismatched, NaN, infinity, and zero-norm payloads are rejected;
  valid payloads round-trip unchanged.
- `Embedding`, `EmbeddingProvider`, `EntityKind`, `SearchCandidate`,
  `IndexedVector`, and `SearchIndex` are application-owned contracts.
  `SqliteRepositories` implements the port in `cortex-storage`; the production
  `cortex-search -> cortex-storage` edge is gone. Storage remains only a search
  dev-dependency for SQLite integration tests.

### Review-fix TDD evidence

RED was observed for the inward contract before production changes:

```text
cargo test -p cortex-application --test search_contract
FAIL E0432: unresolved import `cortex_application::Embedding`
```

The storage-backed regression tests were written before their storage changes
and execution was attempted at RED. Cargo did not reach project compilation:

```text
cargo test -p cortex-storage --lib canonical_search_hash_is_sha256_of_exact_utf8_text
BLOCKED compiling sqlx-core 0.8.6:
E0463: can't find crate for `thiserror`

cargo test -p cortex-search --test fts
BLOCKED compiling sqlx-core 0.8.6:
E0463: can't find crate for `thiserror`
```

The added storage-backed regressions cover:

- content A -> ready A vector -> content B -> no semantic record -> ready B
  vector and B snippet returned;
- lexical and semantic memory candidates with zero active sources and with one
  active source among multiple links;
- malformed length, NaN, infinity, zero norm, and a valid little-endian
  persistence round trip.

Available GREEN evidence after implementation:

```text
cargo test -p cortex-application --test search_contract                 PASS (2 tests)
cargo test -p cortex-application                                        PASS (25 tests)
cargo clippy -p cortex-application --all-targets -- -D warnings         PASS
cargo check -p cortex-search --lib                                      PASS
cargo clippy -p cortex-search --lib -- -D warnings                      PASS
cargo metadata --no-deps --format-version 1                             PASS
  cortex-storage is a dev-dependency, not a production dependency, of cortex-search
cargo fmt --check                                                       PASS
git diff --check                                                        PASS
SQLite in-memory execution of 0001_initial.sql                          PASS
SQLite schema A -> B embedding-invalidation check                       PASS
```

`cargo test -p cortex-domain` ran 9 tests successfully before WDAC blocked the
freshly linked `persistence_rehydration` executable with `os error 4551`; this is
partial evidence, not a package pass.

### Remaining environment gate

The shared target cache still cannot compile SQLx because its `thiserror`
artifact is missing. A non-destructive isolated-target retry confirmed that a
fresh cache cannot replace the missing evidence on this host:

```text
$env:CARGO_TARGET_DIR='target-task7-fix'; cargo check -p cortex-storage
BLOCKED while compiling icu_normalizer_data 2.3.0:
could not execute ...\target-task7-fix\debug\build\icu_normalizer_data-...\build-script-build
Dit bestand is geblokkeerd door een beleid voor toepassingsbeheer. (os error 4551)
```

No shared-cache deletion and no Code Integrity policy change was attempted.
Consequently the new storage code and storage-backed regressions could not be
compiled or run on this host. Acceptance still requires these clean-runner gates:

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Review-fix changed paths:

- `Cargo.lock`
- `crates/cortex-application/src/{lib,search}.rs`
- `crates/cortex-application/tests/search_contract.rs`
- `crates/cortex-search/Cargo.toml`
- `crates/cortex-search/src/{embedding,lib}.rs` (`fts.rs` removed)
- `crates/cortex-search/tests/{degraded,fts,vector}.rs`
- `crates/cortex-storage/Cargo.toml`
- `crates/cortex-storage/migrations/0001_initial.sql`
- `crates/cortex-storage/src/{lib,repositories}.rs`
- `.superpowers/sdd/2026-08-31-cortex-v0.1/task-7-report.md`

## Review fix round 1 verification continuation (2026-09-02)

The SQLx cache blocker was traced to stale local build artifacts for
`thiserror`/`thiserror-impl`. After those scoped artifacts were cleaned outside
the source change, a fresh strict workspace Clippy run reached Cortex code and
reported one source lint:

```text
cargo clippy --workspace --all-targets -- -D warnings
FAIL clippy::items-after-test-module
crates/cortex-storage/src/repositories.rs:474
```

Root cause: the SHA-256 unit-test module added in the first review-fix commit
preceded the existing repository port implementations and decode helpers. The
test module was moved unchanged to the end of `repositories.rs`. No production
behavior changed and no lint suppression was added.

Fresh verification after the move:

```text
cargo fmt --check                                                       PASS
cargo clippy --workspace --all-targets -- -D warnings                  PASS
cargo test --workspace                                                  PARTIAL
  all crates and SQLx compiled successfully
  11 application tests passed
  newly linked application `notes` executable blocked before main:
  Dit bestand is geblokkeerd door een beleid voor toepassingsbeheer. (os error 4551)
cargo test -p cortex-search                                             PARTIAL
  degraded: 4 passed
  fts: 3 passed, including both active-provenance regressions
  newly linked rank executable blocked before main with os error 4551
cargo test -p cortex-search --test vector                              BLOCKED
  newly linked vector executable blocked before main with os error 4551
cargo test -p cortex-storage --lib canonical_search_hash_is_sha256_of_exact_utf8_text
                                                                          PASS (1 test)
```

The stale-cache failure is resolved: the full workspace now compiles and strict
workspace Clippy passes. The remaining incomplete runtime gate is solely the
previously documented Windows application-control restriction on selected newly
linked test executables. No Code Integrity policy was changed.

An immediate final rerun after the selected executables became runnable
supersedes that partial runtime result:

```text
cargo test --workspace                                                  PASS (77 tests)
  cortex-application: 25 passed
  cortex-domain: 22 passed
  cortex-search: 13 passed
  cortex-storage: 17 passed
```

All Task 7 acceptance gates now pass on the committed source plus this narrow
test-module-ordering fix: formatting, strict workspace Clippy, the full workspace
test suite, and diff checks.
