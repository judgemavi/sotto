# ADR-0022: Three-screen consultation budget and durable receipts

- Status: Accepted
- Date: 2026-08-16
- Decision owners: Sotto maintainers
- Amends: ADR-0009 (one screen inspection per reasoning run)
- Upholds: ADR-0004 (append-only timeline), ADR-0007 (derived-view persistence), ADR-0020
  (app-level Ask)

## Context

ADR-0009 replaced eager OCR with a typed, on-demand second pass and capped a reasoning run at one
inspection. That cap established the privacy boundary, but one frame is too small for a long
recording with several transcript moments whose meaning depends on the screen. Notes and Ask also
had different reach: notes could consult one frame, while Ask could not consult any. Finally, the
app kept consultation details only in memory, so a cached summary could outlive the disclosure of
the screen evidence that informed it.

## Decision

One notes or single-recording Ask run may perform at most **three** typed screen inspections. Three
is deliberately small: it permits comparison across a beginning, middle, and end or three distinct
ambiguous moments without turning retained video into ambient prompt context. The initial prompt
states the budget and every response states how many inspections remain.

The fourth request is not dispatched to the inspector. It performs no decode or OCR, is recorded as
`inspection_budget_exhausted`, and is returned to the model once so it can finish from the evidence
already available. A further request fails the run rather than creating an unbounded refusal loop.

Ask uses the same recording-backed inspector assembly as notes when one retained or live recording
is explicitly in scope. A still-growing recording remains honestly unavailable because the
inspector resolves only a settled durable recording. Library-wide Ask remains transcript-retrieval
only: there is no single recording whose screen authority could be inferred.

Every consultation record includes the requested moment and evidence kind, the model's reason, the
actual decoded or sampled precision and time (or explicit refusal), and whether local OCR returned
text. Notes commit the complete record atomically beside the immutable derived artifact. Cached and
stale loads restore that same record after restart. Ask keeps the record on the answer turn. Both
surfaces render a quiet expandable receipt. Consultation records are derived evidence and never
amend the factual session timeline.

ADR-0009's image boundary is unchanged. Local decode and OCR are not image-transport consent;
without the separate Phase 5 opt-in, image requests are refused before decode and serialized
backend requests contain no image bytes or local media path.

## Consequences

- A transcript-sufficient run still performs zero screen work.
- A long recording can ground several materially visual moments while retaining a hard, visible
  privacy and cost limit.
- A cached summary and its disclosure now share one durable identity; an artifact cannot reopen
  without the consultation record that informed it.
- The schema adds an append-only `consultations` field to grounded derived views. Pre-existing
  artifacts migrate with an empty receipt because no durable consultation record existed to
  reconstruct truthfully.
- Proposals retain their existing behavior; proposal-path inspection remains separately scoped.

