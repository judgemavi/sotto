# T008 — RAG crate: sqlite-vec + fastembed local retrieval

**Status:** todo (unblocked — T001 approved)

**Wave:** 1 — fully parallel

**Depends on:** T001 (`Retriever`, `Chunk`, `RagError`)

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
   - `accounts` / `calls` — so account notes and past transcripts can scope retrieval
   `kind` covers battlecard, product doc, account note, transcript. Battlecards are the
   highest-value kind and may warrant retrieval boosting; leave a hook for it.

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

`rag::Store::open(path)` → `Retriever` impl, plus `ingest(path_or_text, kind, metadata)`.
T013 assembles prompts from `Chunk`s; `Chunk` carries enough metadata to cite a source
in `Suggestion::citations`.

## Acceptance

- Query embed + hybrid search under ~50 ms on the fixture corpus.
- Hybrid retrieval measurably better than dense-only on competitor-name queries.
- Re-ingesting unchanged documents does no embedding work.
- Migration test proves an existing DB survives a schema bump.

## Out of scope

Prompt assembly (T013), MCP-sourced context (Phase 4), cloud sync (explicit non-goal),
the ingestion UI (Phase 3).
