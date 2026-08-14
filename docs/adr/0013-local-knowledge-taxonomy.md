# ADR-0013: Generic local knowledge and session-owned meeting memory

- Status: Accepted
- Date: 2026-08-12
- Decision owners: Sotto maintainers

## Context

The original local index classified documents as battlecards, product documents, account notes,
past calls, and recaps. Those labels encode one sales workflow and cannot truthfully describe the
general meeting resources Sotto now supports. Schema v4 also stored an account id but no source
meeting id, so old call and recap rows cannot be linked to a meeting without inventing provenance.

Local indexed evidence is a third evidence plane. It is neither a numeric event in the current
meeting nor an MCP receipt retrieved from a remote server. A later proposal consumer needs to
retain that distinction and be able to resolve a citation to the exact local chunk.

## Decision

1. Schema v5 has exactly four document kinds: `resource_document`, `project_note`,
   `meeting_note`, and `prior_meeting`. The old kinds are accepted only by the v4 migration and
   never by current ingestion or filtering APIs.
2. `collection_id` is an optional opaque local grouping. It replaces `account_id` without
   claiming that an old account is a project. The unused `accounts` table is removed.
3. The migration maps old product-document and battlecard rows to `resource_document`. It also
   maps old account-note, past-call, and recap rows to `resource_document`, marks their chunk
   provenance `legacy_unlinked`, and preserves document ids, chunk ids, text, embeddings, titles,
   paths, grouping values, and other metadata. An unknown old kind aborts migration before any
   mutation.
4. New `prior_meeting` and `meeting_note` rows require a `source_session_id` referencing a durably
   completed session. New resource documents and project notes forbid that field and remain
   independent of meeting retention.
5. Prior-meeting ingestion uses only final utterances from the append-only timeline. It renders
   timestamps with neutral `You` and `Meeting audio` labels plus capture-target metadata. Partials,
   proposals, and other model output are not meeting facts and are not copied into this document.
6. A local evidence receipt resolves a chunk id to its exact text, document id, ordinal, kind,
   title, path, collection, source session, and stored metadata. Search results also identify
   `evidence_scope=local`. MCP evidence ids and meeting event ids remain separate types at their
   respective boundaries.
7. Deleting a session first deletes vector rows for its session-owned local documents, then
   deletes the session. Foreign-key cascades remove its prior-meeting and meeting-note documents,
   chunks, full-text rows, events, and derived artifacts atomically. Independent resource
   documents and project notes survive.

## Consequences

- Migration retains old ambiguous material as generic resources but does not claim it came from a
  known meeting or project.
- Two meetings with identical transcript text remain distinct because document identity includes
  kind and source session rather than globally deduplicating only by content hash.
- Completed meetings remain available to later local retrieval until the user deletes the source
  meeting. Deletion removes both the factual meeting record and every local index projection owned
  by it.
- File ingestion defaults to `resource_document`. A future UI may explicitly choose project notes
  or initiate completed-session ingestion, but no sales taxonomy is exposed.

## Revisit if

- local evidence needs an independent retention period after its source meeting is deleted;
- project collections require a first-class table rather than opaque grouping; or
- a future import format can prove source-session identity for legacy meeting material.
