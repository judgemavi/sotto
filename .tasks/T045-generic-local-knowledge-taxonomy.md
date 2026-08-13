# T045 — Generic local knowledge and prior-meeting taxonomy

**Status:** done

**Wave:** C3 — local context alignment

**Depends on:** T038; T041

**Owns:** `crates/rag/**`, `crates/cli/src/pipeline.rs`, `crates/cli/tests/pipeline_*.rs`,
`crates/rag/tests/**`, `fixtures/rag/**`, `docs/adr/0013-local-knowledge-taxonomy.md`,
`.tasks/T045-generic-local-knowledge-taxonomy.md`

## Goal

Replace sales-only battlecard/account/call labels with meeting-general local resources and prior-
meeting memory without retaining a dual taxonomy.

## Plan

1. Define generic resource document, project note, meeting note, and prior meeting kinds.
2. Migrate current rows/fixtures one way with explicit compatibility evidence.
   Map legacy `product_document` and `battlecard` rows to `resource_document`. Because v4 does not
   carry a source meeting id, conservatively map legacy `account_note`, `past_call`, and `recap`
   rows to `resource_document` with an explicit legacy-unlinked provenance marker; never invent a
   project or meeting relationship.
3. Keep local RAG evidence provenance distinct from meeting events and MCP receipts.
4. Define retention/deletion for prior-meeting ingestion and derived meeting notes.
5. Provide the optional local-context seam for later proposal retrieval. This task does not add a
   core proposal citation field; T013 remains meeting+MCP-only until a later schema task defines a
   distinct local-evidence reference. Never encode local chunk ids as MCP external evidence ids.

## Contract for downstream tasks

Advisor and notes consumers query generic knowledge kinds only. T013 may ship MCP-only grounding
before T045, but must not use historical sales kinds.

## Acceptance

- Production local-knowledge/RAG enums, ingestion call sites, prompts owned under `crates/rag/**`,
  focused fixtures, and migrated current RAG rows contain no privileged sales taxonomy. Historical
  T017 recap types and non-RAG fixtures are explicitly outside this acceptance.
- Migration is deterministic and does not silently relabel ambiguous documents.
- The v5 schema uses generic `collection_id` metadata and optional `source_session_id`; deleting a
  meeting cascades only its prior-meeting/meeting-note documents, while independent resources and
  project notes survive.
- Retrieval citations resolve to exact local chunks and preserve source metadata.
- Deleting a meeting follows the declared prior-meeting and derived-artifact retention policy.

## Out of scope

MCP transport, external actions, embeddings-provider changes, and proposal rendering.

## Implementation handoff — 2026-08-12

- Replaced the production RAG taxonomy one way with `ResourceDocument`, `ProjectNote`,
  `MeetingNote`, and `PriorMeeting`; current ingestion, filters, retrieval metadata, and the CLI
  file-ingest default contain no sales-specific kind or account field.
- Migrated SQLite to v5 with generic `collection_id`, optional `source_session_id`, strict kind and
  source-session checks, and explicit `native` / `legacy_unlinked` provenance. The deterministic
  v4 migration preserves ids, chunks, embeddings, titles, paths, group values, arbitrary metadata,
  sessions, derived views, and T041 grounded artifacts. Unknown legacy kinds abort before mutation.
- Legacy-kind preflight runs before every v1–v4 upgrade step. Failed v1/v2/v3/v4 upgrades leave
  the original schema version and rows unchanged across repeated reopen; fresh/current v5 stores
  skip the legacy preflight and reopen normally. Intermediate grounded-schema migration stamps v4
  before the separate v5 taxonomy transaction.
- Added exact `LocalEvidenceReceipt` resolution plus local evidence-scope metadata, keeping local
  chunk ids distinct from numeric meeting `EventId`s and MCP evidence ids.
- Added completed-session prior-meeting ingestion from timestamped final utterances only, using
  neutral `You` / `Meeting audio` labels and capture-target metadata. Meeting-note ingestion also
  requires a durably completed source session; independent resources and project notes forbid one.
- Prior-meeting rendering uses strict active timeline replay, so a corrected final replaces its
  superseded final and malformed replay fails before any local meeting memory is written.
- Scoped vector and FTS candidate queries apply kind, collection, and source-session predicates at
  the document join and widen the ANN window geometrically to an explicit 256-candidate bound;
  more than `4*k` higher-ranked nonmatches cannot hide an in-scope result.
- Session deletion transactionally removes session-owned vector rows before the session cascade;
  its prior-meeting/meeting-note documents, chunks, FTS rows, events, and derived artifacts are
  removed while resource documents and project notes survive.
- Replaced the RAG fixture corpus and hybrid retrieval names with meeting-general resource,
  project-note, meeting-note, and prior-meeting examples. ADR-0013 records migration, provenance,
  evidence, and retention consequences.
- PASS — RAG tests: 18 passed; the pre-existing fastembed model-cache performance check remains
  explicitly ignored. Coverage includes v1/v2/v3 compatibility, v4→v5 mapping and rollback, all
  four current kinds/filters, receipts, final-only session memory, T041 preservation, and selective
  document/chunk/FTS/vector deletion, plus v1–v3 failure safety, corrected-final projection, and
  adversarial scoped retrieval beyond the original `4*k` candidate window.
- PASS — focused CLI pipeline default-kind test: 1 passed.
- PASS — RAG and CLI all-target/all-feature Clippy with warnings denied. CLI gates used
  `WHISPER_DONT_GENERATE_BINDINGS=1`; no ASR inference or model download ran.
- PASS — `cargo fmt --all -- --check` and `git diff --check`.
- NOT RUN — fastembed model download, live embedding/search latency, network, provider, MCP, GUI,
  or manual product acceptance. The existing ignored latency test was not presented as evidence.

## Independent acceptance — 2026-08-12

- Accepted the one-way v5 generic taxonomy and conservative legacy mapping. Legacy-kind preflight
  preserves v1–v4 state on failure, the v5 transaction preserves T041 grounded artifacts, and
  current v5 stores reopen without being mistaken for legacy input.
- Accepted exact local receipts, source-session constraints, active-final-only prior-meeting
  rendering, bounded scoped vector/FTS retrieval, and transactional session deletion including
  chunk, FTS, vector, and derived-artifact cleanup while independent knowledge survives.
- Confirmed T013 remains meeting+MCP-only until a later distinct core local-evidence citation
  schema; local chunk ids are not represented as MCP `ExternalEvidenceRef` values.
- PASS — fresh focused RAG run: 18 passed and the documented fastembed performance canary ignored;
  focused CLI default-kind test: 1 passed; strict RAG Clippy, workspace Rustfmt, and owned diff/
  production-taxonomy checks passed. No live model download or product acceptance was inferred.
