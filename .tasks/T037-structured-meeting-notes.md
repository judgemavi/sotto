# T037 — Structured meeting-notes derived view

**Status:** done

**Wave:** N1 — minimum AI capability

**Depends on:** T036; T026; T028; T031; T034

**Owns:** `crates/insight/src/notes/**`, `crates/insight/src/lib.rs`,
`crates/insight/tests/meeting_notes.rs`, `prompts/notes/**`,
`.tasks/T037-structured-meeting-notes.md`

## Goal

Produce a provider-neutral, cached `MeetingNotes` derived artifact from a persisted timeline,
without sales-specific fields and without mutating the record.

## Plan

1. Define overview, topics, decisions, action items, open questions, risks, and follow-ups.
2. Require known nonempty `EventId` citations for factual items; owner and due date are optional
   and may appear only when explicitly evidenced.
3. Use transcript-first context and the existing optional typed screen-inspection second pass.
4. Cache by timeline, exact prompt, artifact schema, and backend fingerprint.
5. Keep the historical T017 recap API until consumers hand off; export the new product API
   alongside it rather than rewriting completed evidence.

## Contract for downstream tasks

`MeetingNotesGenerator::generate(session_id) -> MeetingNotesReport` returns validated notes,
usage, backend identity, and cache state. It performs no model call on a valid cache hit.

## Acceptance

- The JSON contract contains no rep/customer, objection, competitor, or sales fields.
- Unknown or empty citations and unsupported owner/due claims fail closed.
- Long-session map/reduce output validates all citations.
- Unchanged input hits cache with zero provider calls; prompt/backend changes miss cache.
- The serialized timeline is byte-identical before and after generation.
- Credential-free tests use a fake reasoning provider; no live call is required.

## Out of scope

App UI, MCP evidence, realtime proposals, and real-meeting quality acceptance.

## Notes — implementation pass 1 (2026-08-12)

Implemented the new product-facing notes API alongside the historical T017 recap API:

- `MeetingNotes` contains only overview, topics, decisions, action items, open questions, risks,
  and follow-ups. Its serde contract rejects unknown fields, including sales-only output.
- Every factual item requires nonempty, known timeline citations. Action owners and due dates are
  optional and require their own known evidence citations; missing, unexpected, or empty claims
  fail closed.
- `MeetingNotesGenerator` uses the shared transcript-first reasoning context and optional bounded
  `inspect_screen` second pass. Twenty-minute transcript windows are reduced for long meetings.
- Derived artifacts use the existing side-table and are keyed by `meeting_notes.v1`, the exact map
  and reduce prompts, schema id, capture-target transcript context, full serialized timeline, and
  backend fingerprint. Valid cache hits make zero provider calls and report the pinned backend
  identity.
- Generation validates both map outputs and the reduced artifact, persists only validated notes,
  and leaves the canonical timeline byte-identical.

No app, MCP, advisor, Cargo lockfile, or historical T017 implementation files were changed by this
task. There were no deviations from the plan.

Verification:

- `cargo test -p insight --test meeting_notes --locked`: 6 passed, 0 failed.
- `cargo test -p insight --locked`: 16 passed, 0 failed, including the historical recap,
  clustering, and screen-request regressions.
- `cargo clippy -p insight --all-targets --locked -- -D warnings`: passed.
- `cargo fmt -p insight -- --check`: passed.
- Scoped whitespace/diff inspection: passed; no live provider or real meeting was used or claimed.

## Independent review — 2026-08-12

Accepted after exact-window citation hardening. Each map result is now validated against only the
final-utterance ids rendered into that chronological request; adversarial regressions prove that a
first window cannot cite a later window for a factual item, owner claim, or due-date claim, and
generation stops after the invalid first map call. Reduce output and cached artifacts remain
validated against the complete persisted meeting.

Screen inspection remains truthful and fail closed: the shared helper may perform its one typed
second pass, but it does not yet return the inspected snapshot id to the notes generator. T037
therefore does not silently widen the map citation set or accept a screen-only claim as meeting
evidence. Supporting snapshot citations later requires an explicit provenance-bearing helper
contract; it cannot be inferred from temporal proximity.

Independent verification passed: focused meeting-notes tests 6/6, full insight tests 18/18, strict
all-target insight Clippy, package formatting, and scoped diff-check. The schema remains
meeting-general, optional owner/due claims require separate known evidence, cache identity includes
schema/prompts/transcript/timeline plus backend fingerprint, valid cache hits make zero calls, and
the canonical timeline remains byte-identical.

## Review-blocker fix — map-window citation scope (2026-08-12)

Map-stage validation now admits only the final utterance event ids present in that exact
twenty-minute prompt window. It no longer validates a partial map result against the full session,
so a model cannot cite an unseen future window merely because the event exists in SQLite. Reduce,
final-artifact, and cache-hit validation still use the full session because those stages receive or
reload the combined artifact.

The shared `complete_with_optional_inspection` result does not expose which snapshot event, if any,
its internal inspection pass returned. T037 therefore cannot safely add a screen-only event id to
the map evidence set and fails such citations closed; no broad session fallback was introduced.

Added adversarial two-window regressions proving the first map call rejects:

- a factual claim cited only to the unseen second-window event;
- an owner cited only to the unseen second-window event; and
- a due date cited only to the unseen second-window event.

Each regression also proves generation stops after that invalid first provider call. Final-tree
verification is the same command set recorded above, with the focused suite now at 6 passed and
the full `insight` suite at 16 passed.
