# T019 — Meeting topical clustering

**Status:** blocked

**Wave:** Phase 3 — optional derived meeting view

**Depends on:** T036; T008; T024/T028; T029 for live OpenAI evidence; T035 for participant
validation

**Owns:** `crates/insight/src/clustering/**`, `crates/insight/tests/clustering.rs`,
`prompts/clustering/**`, `.tasks/T019-topical-clustering.md`

## Goal

Provide the optional second organizing axis over a meeting: a small set of participant-recognized
topic regions, cross-time links, and conservative open threads. This is a derived artifact and
never changes the append-only meeting record.

ADR-0011 supersedes the original sales-only examples and `Objection`-only open-thread taxonomy.
Pricing or sales objections remain possible meeting content, not privileged product schema.

## Plan

1. Preserve the existing persistence, prompt/timeline/backend fingerprint cache identity,
   citation validation, and byte-identical timeline guarantees.
2. Generalize open-thread kinds and prompt language to question, decision, action item, and risk.
   Prefer few confident regions over speculative clutter.
3. Keep regions and cross-time links keyed only by known `EventId`s. Empty regions, unknown ids,
   and unsupported relationship claims fail closed.
4. Use transcript-first context and T028's optional typed screen inspection; no eager OCR/image.
5. Degrade normally to chronology with no model. Report cached state and provider usage.
6. After deterministic migration is reviewed, return to `blocked` until T035/T029 provide the real
   participant-recognized validation input.

## Contract for downstream tasks

`Clusterer::cluster(session_id) -> ClusterReport` yields cached derived regions, links, and open
threads with meeting-general kinds and valid citations. T038 may render it as an optional overlay
after Notes/Board UI acceptance.

## Acceptance

- Production clustering types/prompts contain no required sales roles or objection-only schema.
- Existing cache, backend separation, citation, idempotency, and timeline-immutability tests pass.
- No-backend mode leaves the chronological board complete and truthful.
- A T035-accepted real meeting later produces regions/links/open threads a participant recognizes;
  that manual gate is not satisfied by fixtures.

## Out of scope

Meeting notes (T037), MCP enrichment, realtime proposals (T013), and board rendering.

## Historical evidence

The earlier implementation established a sound derived-view table, prompt-aware content hash,
backend fingerprint, known-event validation, and byte-identical timeline tests. Its sales-shaped
`Objection` vocabulary is the only deterministic migration now requested; real-call recognition
was never claimed.

## Notes — meeting-general migration complete (2026-08-12)

The deterministic migration is complete. `OpenThreadKind` is now a one-way meeting taxonomy of
`Question`, `Decision`, `ActionItem`, and `Risk`; no legacy `Objection` alias remains. The versioned
prompt uses the same four kinds, assumes no participant role or meeting domain, and keeps screen
inspection optional and derived. The derived-view kind advanced to `topical_clusters.v2`, while
the existing prompt/timeline/backend fingerprint cache dimensions, known-event validation,
idempotency, and byte-identical timeline guarantees remain intact.

Regression coverage proves all four kinds serialize and deserialize, the legacy objection value
fails closed, unchanged input is cached without mutating timeline bytes, and identical model names
under different backend fingerprints do not share artifacts. The focused clustering tests pass
3/3; the full insight suite passes 18/18; strict insight Clippy, insight formatting, and scoped
diff checks pass.

No real meeting, live OpenAI request, or participant review ran. T019 is blocked only on T035's
accepted real meeting and T029's live backend evidence for participant-recognized regions, links,
and open threads. Synthetic fixtures do not satisfy that manual acceptance.

## Review response — rendered-evidence citation boundary (2026-08-12)

Citation validation now admits only `utterance.final` ids, matching the evidence rendered into the
initial clustering request. A persisted event being known to the session is no longer sufficient:
partial utterance, VAD, prosody, screen snapshot, and system-output ids fail closed. Inspection-only
ids remain outside the allowed set until an explicit inspection-provenance citation contract exists.

`DerivedView`, `TopicRegion`, `TopicLink`, and `OpenThread` now deny unknown JSON fields. Regression
tests reject both the legacy root `objections` field and the legacy `objection` kind, reject an
unknown nested field, and prove that a known VAD id omitted from the transcript prompt cannot be
cited.

Verification after the fix: focused clustering tests pass 6/6; the full insight suite passes 21/21;
strict insight Clippy, insight formatting, and scoped diff checks pass. No real meeting, live OpenAI
request, screen-inspection citation, or participant validation ran. The only remaining gate is the
T035/T029-backed participant review already recorded above.

## Independent deterministic-slice review — 2026-08-12

Accepted. The `topical_clusters.v2` namespace makes the migration one-way at the artifact boundary;
the meeting-general enum has no legacy alias, and strict serde contracts reject the old root field
plus unknown nested fields. Production types and prompt contain no privileged sales taxonomy.

Citation validation now matches the evidence actually rendered: only final-utterance ids are
admitted, so a session-known VAD or other non-transcript event cannot pass merely because it exists
in the timeline. Screen-inspection ids remain fail closed until the shared helper exposes explicit
inspection provenance. Cache hits remain zero-call and backend-separated, prompt text remains in
the content hash, and clustering leaves serialized timeline bytes unchanged.

Independent gates passed: focused clustering tests 6/6, full insight tests 21/21, strict all-target
insight Clippy, package formatting, and scoped diff-check. Status correctly remains `blocked`: no
T035-accepted real meeting, live T029 OpenAI evidence, or participant recognition review ran.
