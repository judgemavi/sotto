# T054 — Ask across sessions

**Status:** done

**Wave:** A1 — ask surface

**Depends on:** T053 accepted, which owns `workspace/ask.rs` until then. T008 and T045 are accepted
history.

**Owns:** past-timeline ingestion under `crates/rag/**`, the cross-session scope in the
`crates/insight` ask module, `crates/app/src/workspace/ask.rs` by sequential handoff from T053, and
this task

## Why this exists

"Have rate limits come up before?" is the question that makes a pile of recordings into a memory.
It is also the point where the privacy story gets tested, because answering it means one session's
question reaching into another session's record.

AGENTS.md already permits this and constrains it: past timelines may be ingested into RAG **under an
explicit retention policy**, so future notes and proposals can reference prior meetings. T045
established the local knowledge taxonomy, exact local receipts, bounded scoped retrieval, and
session retention. This task spends that groundwork rather than reinventing it.

## Sequential handoff

T053 established and released the answer, citation and refusal shapes before this implementation
extended the panel. T054 reuses them unchanged.

## Plan

1. Ingest completed session timelines into the existing sqlite-vec store through T045's taxonomy and
   receipts. Ingestion is governed by the retention policy, is visible, and is reversible: a user can
   see what is searchable and exclude a session from it.
2. Retrieval is bounded the way MCP evidence is bounded: capped result count, capped excerpt size,
   provenance recorded for every excerpt, and no unbounded scan. Retrieved excerpts are evidence with
   ids, not free text spliced into a prompt.
3. Scope is explicit and never silently widened. The panel shows which scope is active — this session
   or every session — and switching is a deliberate user act. A question asked in single-session
   scope must never retrieve from another session.
4. Citations name the session and the `EventId`. Selecting one opens that session read-only and
   focuses the row, using T049's session selection and citation focus.
5. Excluded sessions are unreachable, not merely unranked. Prove exclusion with a test that asks a
   question whose only answer lives in an excluded session and asserts a refusal.
6. Cost and latency: retrieval is in-process with the pipeline (AGENTS.md), so a cross-session
   question adds no IPC hop. Record the observed added latency rather than assuming it is free.

## Contract

- Cross-session answers reuse T053's answer, citation and refusal shapes unchanged. A reader cannot
  tell the two scopes apart except by the citation labels and the visible scope control.

## Acceptance

- A question in all-sessions scope returns citations naming the session and row, and selecting one
  opens that session and focuses that row.
- A question in single-session scope retrieves nothing from any other session, asserted by test.
- A session excluded from retention is unreachable: the same question that succeeds with it included
  produces a refusal with it excluded.
- Retrieval is bounded and every excerpt carries provenance; no path performs an unbounded scan.
- Ingestion is idempotent: re-ingesting a session does not duplicate evidence or citations.
- Observed added latency for a cross-session question is recorded in `## Notes`.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Ingesting anything other than completed local session timelines, MCP sources in cross-session
answers, cloud sync, sharing sessions between users, and any automatic cross-session summarization.

## Notes

- SQLite schema v6 adds a durable per-session search policy that defaults excluded. Inclusion is a
  visible user action and idempotently indexes only completed final transcripts. Exclusion deletes
  the prior-meeting vectors and document; both dense and FTS retrieval additionally require an
  included policy row, so excluded sessions are unreachable rather than merely down-ranked.
- All-session retrieval is capped at five receipts and each excerpt at 2,000 characters. Every
  excerpt retains its evidence id, session id, label, text and citable event ids. Single-session
  scope bypasses RAG entirely. Cross-session citations display the session and event, open that
  meeting read-only, and focus the row.
- The in-memory bounded retained-meeting query observed 1.70 ms added retrieval time on this host.
  This excludes first-use embedding-model download/load and is not a signed-app latency claim.
- Focused RAG/insight/app tests, the full workspace suite, strict Clippy, formatting and diff checks
  pass. Live model, first-use model download, signed-app, and visual acceptance are `NOT RUN`.

### Closure — 2026-08-13 (planner, narrowed)

Accepted: explicit reversible retention, bounded retrieval with provenance, scope that is never
silently widened, and session-aware citations that open the cited session and focus the row.

Live-provider and real multi-session evidence pass to T035. Visual acceptance passes to T055.

