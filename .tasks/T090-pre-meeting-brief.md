# T090 — The brief: walk in knowing what was promised

**Status:** blocked

**Wave:** B1 — preparation

**Depends on:** T086 and T089 (an entry to render into, before recording), T087 (open action
items readable from composed documents). Uses the existing bounded retrieval (T054/ADR-0020) and
the existing reasoning runtime; live-backend evidence folds into T044 like every other reasoning
surface.

**Owns:** the brief generator in `crates/insight/**` as its own module, its prompt under
`prompts/brief/**`, the brief panel on the entry page in `crates/app/src/workspace/**`
(sequential handoff from T089), and this task

## Why this exists

Every note-taker on the market writes down what happened. Nobody reliably answers the question a
person actually has *before* the meeting: what did we say last time, what is still open, what did
I promise. Sotto is the only tool positioned to answer it privately — the material is a local
index of prior entries plus explicitly granted MCP sources. The brief is a one-shot generation
into the prepared entry, so it dodges every live-latency and streaming-cost problem by
construction.

## Plan

1. **Trigger and placement:** on a prepared entry (T089), a person asks for a brief — one
   explicit action, like generating a summary. Never automatic, never on a timer. The brief
   renders as a panel on the entry page, clearly derived, regenerable, and dismissible; it is not
   part of the notes document and is never projected to the vault as user content.
2. **Evidence assembly, all local until the reasoning call:** related prior entries via the
   existing bounded retrieval (seeded by the entry's title, series link, and prep-note text),
   open action items from composed documents (T087 — unchecked tasks are the query, not a model
   inference), and, only under the session's explicit grants, MCP resources per the standing
   T039/T041 contract.
3. **The request is transcript-first and bounded** like notes: excerpts with evidence ids, capped
   count and size, provenance on every excerpt. The output schema is small: open items (each
   citing its source entry and event), unresolved questions, and a short "last time" synthesis —
   every claim cited, uncited claims failing closed through the existing validation.
4. **Citations open the source:** each brief item's citation opens the cited prior entry
   read-only and focuses the row, through the existing cross-session citation path (T054).
5. **Honest degradation:** no reasoning backend → the panel states why and still shows the
   deterministic half (open tasks from prior composed documents need no model — render them
   regardless). No related history → say so, plainly, not an empty panel.
6. **Fixtures first:** a fixture library with a series of prior entries containing known open
   items; assert the brief surfaces exactly those, cited. Precision matters more than recall —
   an invented commitment in a brief is worse than a missed one, and the eval weights it so.

## Acceptance

- A prepared entry can produce a brief citing prior entries; every citation opens the source
  entry and focuses the cited row.
- Open action items from prior composed documents appear deterministically, unchecked state
  respected, with no model call required for that section.
- Every model-written claim carries citations and uncited claims fail closed, asserted by test.
- No MCP query leaves without the standing per-session grant; no retrieval is unbounded.
- Backend-absent and no-history states are explicit and useful, not empty.
- Regenerating replaces the brief panel; nothing about the brief mutates the notes document or
  the record.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Automatic triggering, calendar integration, the live watcher (T013), commitment *extraction*
(T093 — this task reads checked/unchecked tasks that already exist as blocks), series-page
generation (T088), and any new retrieval machinery.
