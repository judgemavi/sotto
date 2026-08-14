# T092 — Screen consultation, finished: a budget, Ask's reach, and a disclosure that survives

**Status:** blocked

**Wave:** D1 — living notes

**Depends on:** T066 (`in-review`) whose `## Open gaps` section is this task's charter, T070
(`todo`) for the notes surface and artifact schema this task extends, and T075/T076's close for
`workspace/notes.rs`. Blocked until they close. The one-inspection rule this amends is
ADR-0009/T028's; the amendment is recorded here and needs a short ADR entry on acceptance.

**Owns:** the inspection budget in `crates/insight/**`, the Ask inspection seam in the
`crates/insight` ask module, persisted consultation records in `crates/rag/**` alongside the
derived artifact, the consultation receipt in `crates/app/src/workspace/notes.rs` and
`crates/app/src/reasoning/**` (sequential handoff from T066), and this task

## Why this exists

T066 joined the seam: a notes run can pull a frame, disclosed and image-denied. Three gaps keep
the capability smaller than the product needs, all recorded by T066 itself:

1. **One inspection per run.** Over a sixty-minute recording full of "as you can see here", a
   single inspection means the summary mostly cannot use the screen even when the transcript begs
   for it. The cap exists for cost and privacy discipline, not because one is the right number.
2. **Ask cannot look at all.** `AskEngine` has no inspector seam, so "what was on the slide when
   she said that" — the most natural screen question — is unanswerable.
3. **The disclosure dies with the run.** Consultations live in memory per run; a cached reopen
   serves notes that consulted the screen with no trace that they did. A model that looked at the
   screen and a record that does not say so is precisely what the transparency posture forbids.

## Plan

1. **Replace the single inspection with a budget of N per run** (small, fixed, stated — start at
   three and record the reasoning). Every inspection still goes through the same typed request,
   provenance, and image-deny policy; the budget exhausting is reported to the model and logged,
   never silent. The prompt tells the model its budget so it spends inspections where the
   transcript most needs them.
2. **Give Ask the seam notes has:** thread the inspector assembly through `AskEngine` the way
   `NotesController` does, budget included, live-recording resolution behaving exactly as T066
   left it (a still-growing recording is honestly unavailable).
3. **Persist the consultation log with the derived artifact** it informed. Reopening a cached
   summary shows the same disclosure the original run showed: which moments, what precision, what
   was refused, and that no image left the device. The record is append-only alongside the
   artifact version, not a mutation of it.
4. **Render the receipt in the notes column** beside the model/provenance receipt, per T066's
   handoff note — and in Ask's answer receipt when an Ask run consulted. Quiet: a line that
   expands, not a panel.
5. Keep every T066 invariant under the new budget, re-asserted: no decode on runs that need no
   visual context, image requests refused before decode, nothing serialized into a request beyond
   the OCR text and stated metadata.

## Acceptance

- A notes run can perform up to N inspections; the N+1th is refused, reported to the model, and
  logged; a run needing none still decodes nothing — all asserted through the app's own runtime
  assembly, not doubles.
- An Ask run over a retained recording can consult a frame and its answer receipt shows it.
- A cached reopen of a summary whose run consulted the screen shows the full consultation
  disclosure after app restart, asserted by test.
- No image reaches a backend without the separate opt-in, asserted over the serialized request —
  unchanged and re-proven under the budget.
- The ADR entry amending the one-inspection rule is written and referenced here.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

The image-transport opt-in itself (Phase 5), decoder or OCR changes, proposal-path inspection
(T013), and any UI beyond the two receipts.
