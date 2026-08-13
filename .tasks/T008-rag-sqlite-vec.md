# T008 — RAG crate: sqlite-vec + fastembed local retrieval

**Status:** done

**Wave:** 1 — fully parallel

**Depends on:** T001 (`Retriever`, `Chunk`, `RagError`) · T014 (timeline SQLite schema)

**Owns:** `crates/rag/**`

## Goal

Battlecards, product docs, account notes and past transcripts in one local SQLite file,
retrieved in-process with zero IPC hops on the hot path (`AGENTS.md`). Retrieval runs
*in parallel with* the trigger classification, so its latency budget is roughly "faster
than the watcher model" — a few tens of milliseconds, not hundreds.

## Plan

1. Deps: `rusqlite` (bundled SQLite so we don't depend on the system copy),
   `sqlite-vec`, `fastembed`. Note the on-disk footprint fastembed's default model adds
   and whether it is downloaded or bundled — footprint is a product claim, and unlike
   Whisper weights this one is small enough that bundling may be right.

2. **Schema** (`crates/rag/src/schema.rs`) with migrations from version 0 — users will
   have real data in this file and we will change the schema:
   - `documents` (id, kind, title, source_path, ingested_at, content_hash)
   - `chunks` (id, doc_id, ordinal, text, token_count, metadata JSON)
   - `vec_chunks` — sqlite-vec virtual table over the embedding
   - `accounts` — so account notes and past calls can scope retrieval
   - `sessions` / `events` — **the timeline tables, defined by T014.** Implement them
     exactly as specified there; core owns the shape, you own the I/O.
   `kind` covers battlecard, product doc, account note, and past-call recap. Battlecards
   are the highest-value kind and may warrant retrieval boosting; leave a hook for it.

2b. **Timeline persistence.** You own writing timeline events to SQLite and reading them
   back. Two hard requirements from `AGENTS.md`:
   - **Append-only on disk too.** Events are inserted, never updated; a correction is a
     new row with `supersedes` set. No `UPDATE` on the events table.
   - **Writes must never stall the live pipeline.** A call is in progress while you are
     writing. Batch inserts on a background task off the hot path, and make a slow or
     failed write degrade the recording, never the call.
   Provide `load_session(session_id) -> Vec<TimelineEvent>` in timeline order — T016's
   board replays it and asserts it reproduces the live board exactly.

2c. **Past timelines become account memory.** `AGENTS.md`: *"every recorded call makes
   future advising smarter about that account."* Ingest finished sessions into the vector
   index — chunk by topic window rather than by raw utterance, since a single line out of
   context retrieves badly. Scope chunks by account so *"what did they object to last
   time?"* is answerable. T017's recaps ingest the same way and are probably the better
   retrieval unit; support both and note which retrieves better.

3. **Ingestion.** Plain text and Markdown first; PDF and DOCX only if a light pure-Rust
   crate exists — do not pull a heavyweight parser into a binary that claims to be
   small. Chunking: structure-aware (split on headings, keep a battlecard's
   objection→response pair intact) with overlap, targeting ~300–500 tokens. A
   battlecard split down the middle retrieves as two useless halves — chunk quality
   dominates retrieval quality here.
   `content_hash` drives re-ingestion so unchanged docs are not re-embedded.

4. **Embedding** via `fastembed`. Batch on ingest. Cache the model handle; load lazily
   and unload when idle, same discipline as Whisper. Query embedding is on the hot path
   — measure it separately from search and report both numbers.

5. **Retrieval.** Implement `Retriever`. Vector KNN via sqlite-vec plus SQLite FTS5
   keyword search, fused (reciprocal rank fusion is fine). Hybrid matters
   disproportionately here: competitor and product names are exactly the terms a
   dense embedder blurs and a keyword index nails. Support metadata filters
   (kind, account) so retrieval can be scoped to the current call's account.

6. **Storage location.** One SQLite file under the platform app-support dir, path
   injectable for tests. WAL mode. Ingestion must not block reads — retrieval is on the
   hot path and a background ingest must never stall a live call.

7. **Seed data.** Commit a small realistic fixture set (a handful of battlecards, a
   product doc, an account note) under `crates/rag/tests/fixtures/`. Downstream prompt
   and watcher work (T013) needs something to retrieve against, and inventing it there
   would duplicate this effort.

8. Tests: ingest → retrieve round trip, hybrid beating either method alone on a
   competitor-name query, migration from an older schema, and a bench for
   embed-query + search latency.

## Contract for downstream tasks

`rag::Store::open(path)` → `Retriever` impl, plus `ingest(path_or_text, kind, metadata)`,
`append_events(&[TimelineEvent])` and `load_session(session_id)`. T013 assembles prompts
from `Chunk`s; `Chunk` carries enough metadata to cite a source in
`Suggestion::citations`. T016 and T017 both read timelines back through you.

## Acceptance

- Query embed + hybrid search under ~50 ms on the fixture corpus.
- Hybrid retrieval measurably better than dense-only on competitor-name queries.
- Re-ingesting unchanged documents does no embedding work.
- Migration test proves an existing DB survives a schema bump.
- A persisted session round-trips: `append_events` then `load_session` returns an
  identical, identically-ordered event log including supersede chains.
- Sustained event writes during a simulated live call do not stall the pipeline.

## Out of scope

Prompt assembly (T013), MCP-sourced context (Phase 5), cloud sync (explicit non-goal),
the ingestion UI (Phase 4).

## Review round 1 — changes requested

The persistence half is right. `SQLITE_SCHEMA` is applied exactly as `core` defines it,
`PRAGMA foreign_keys = ON` and WAL are set per connection, `append_events` is INSERT-only
in a single transaction so duplicate identities fail rather than mutating the log, and both
tests are well chosen — `every_connection_enforces_foreign_keys` in particular guards the
exact footgun the schema comment warns about.

Two things to fix.

### R1. One mutex serializes reads against writes, on the hot path

```rust
pub struct Store {
    connection: Mutex<Connection>,
    ...
}
```

Every operation takes the same lock — `append_events`, `ingest`, `search`, `load_session`.
WAL mode exists precisely so readers and a writer can proceed concurrently, and this mutex
throws that away.

The failure is concrete and only appears live: during a call, T011 appends timeline events
continuously while the advisor runs retrieval against a ~50 ms budget. A batched append
transaction — or worse, an ingest holding the lock through an embedding batch — blocks
retrieval for its full duration. Two requirements in this brief say that must not happen:
*"Ingestion must not block reads — retrieval is on the hot path and a background ingest must
never stall a live call"* and *"Writes must never stall the live pipeline."*

Separate the read path from the write path — a dedicated write connection plus one or more
read connections, or a small pool. SQLite in WAL mode supports exactly this; the Rust side
just has to stop preventing it.

Also document on `append_events` that it blocks and must be called off the live path.
Nothing in the current signature or docs tells T011 that, and the natural reading of a
plain synchronous method is that it is cheap.

### R2. Four of six acceptance criteria are untested

Two tests for 560 lines, both covering timeline persistence. The retrieval half — which is
most of the crate — has none. Still unverified:

- hybrid retrieval measurably better than dense-only on competitor-name queries (the
  specific reason hybrid was required at all);
- re-ingesting unchanged documents does no embedding work (`content_hash` path);
- migration from an older schema preserves existing data;
- query embed + hybrid search under ~50 ms on the fixture corpus.

The first two are cheap and catch real regressions. The migration test matters most in the
long run: users will have real timelines in this file and the schema *will* change.

### Note

`crates/rag/tests/persistence.rs` wraps its tests in `#[cfg(test)]`. That works, but the
documented house idiom for integration files is the `#![expect(clippy::tests_outside_test_module, reason = "...")]`
header in `.tasks/README.md` — worth matching so the codebase reads consistently.

### Re-review

R1 and R2 addressed, existing tests still green.

## Review round 2 — approved

Both items fixed, and one trap avoided that I did not flag.

**R1.** `Store` now holds separate `writer` and `reader` connections, and the split is
actually used: `save_session`, `append_events` and `ingest_text` take the writer;
`load_session`, `load_session_record` and `search_filtered` take the reader. Retrieval no
longer waits behind an append or an embedding batch, which is what WAL was there to allow.
`append_events` now documents that it blocks and must be called from a background task,
never the live pipeline — so T011 cannot mistake it for cheap.

**Avoided trap:** `open_in_memory` uses `file:sotto-rag-{n}?mode=memory&cache=shared` with
`SQLITE_OPEN_URI` and a per-store counter. Two plain `:memory:` connections would each have
got a private database, so the reader would never have seen the writer's data — tests would
have failed confusingly, or worse, passed by only exercising one side. Catching that
unprompted is the good kind of care.

**R2.** Hybrid-vs-dense, unchanged-content, and v1→v2 migration are all tested, including
`migrating_a_version_one_database_preserves_existing_data` — the one that matters most,
since users will have real timelines in this file when the schema next moves.

The latency check is `#[ignore]`d behind the fastembed model cache, with an explicit reason
string. That is the honest way to do it, and better than a test that quietly passes without
measuring. It does mean the **~50 ms retrieval budget is still unverified** — carry it as
open until the fixture corpus exists and it can run for real. Note it in the crate docs so
the next person does not read a green suite as proof of latency.

## Follow-up — schema v3 (2026-07-29)

T019's derived-view table took the schema to v3. I traced all four version paths and the
migration is correct: fresh installs get `derived_views` from the base schema, `version == 1`
adds its indexes then falls through, and `version <= 2 && version != 0` catches both that
fall-through and a database already at v2. No gap.

Two things to tighten while this is fresh:

- **There is no v2→v3 migration test.** Only `migrating_a_version_one_database_preserves_existing_data`
  exists — but v2 is what every existing user of the previous build actually has, so the v2→v3
  path is the migration that will really run, and it is the one untested. Given how often in this
  project an untested path has looked fine, please add it.
- **`derived_views` is now defined twice** — once in the base schema for fresh installs, once as
  `CREATE TABLE IF NOT EXISTS` in the migration block. They must stay byte-compatible forever, and
  nothing enforces that. Either derive the migration from the same constant, or add a test that
  opens a fresh database and a migrated one and asserts identical `PRAGMA table_info` output.

## Follow-up implementation (2026-07-30)

Added a v2→v3 migration test that starts from the schema existing users have, preserves a session,
and exercises derived-view save/load after migration. Fresh creation and migration now execute the
same `DERIVED_VIEWS_SCHEMA` constant, eliminating the duplicated table definition rather than
merely testing two copies for equality.

## Review of the follow-up — both gaps closed

The v2 fixture is constructed correctly: v2 is `SQLITE_SCHEMA` + `RAG_SCHEMA` + the two v1→v2
indexes, which is exactly what dropping `derived_views` and its index from a v3 database leaves
behind. And the test does more than assert the table exists — preserving the session record and
then round-tripping a derived view through the migrated database exercises the foreign key into
`sessions`, which is the part that would actually break.

Deriving both paths from one `DERIVED_VIEWS_SCHEMA` is better than the `PRAGMA table_info`
comparison I suggested as the alternative: it removes the drift instead of detecting it. The
`IF NOT EXISTS` now in the fresh-install path is marginally weaker than a bare `CREATE TABLE`,
but on a fresh database there is nothing for it to skip over.
