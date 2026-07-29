# T008 — RAG crate: sqlite-vec + fastembed local retrieval

**Status:** todo (unblocked — T014 frozen)

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
