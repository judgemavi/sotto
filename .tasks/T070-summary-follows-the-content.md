# T070 — Summarize what was recorded, not what a meeting would have been

**Status:** todo

**Wave:** R3 — reasoning product

**Depends on:** ADR-0019. T069 holds `crates/app/src/settings/mod.rs` and
`crates/app/src/workspace/ask.rs` until it closes.

**Owns:** `crates/insight/src/notes/**`, `prompts/notes/**`, the notes artifact kinds and schema ids,
`crates/app/src/notes/**`, `crates/app/src/workspace/notes.rs`, and this task

## Why this exists

On 2026-08-13 a maintainer captured a Brooklyn Nine-Nine clip and generated notes. The result:

> Transcript contains entertainment dialogue from a Brooklyn Nine-Nine video; no meeting content is
> present.

Every layer worked. Capture, the recording, lagged transcription, the Codex connector, citation
validation — all correct. The product asked for a meeting report, so a correct answer was useless.

`GroundedMeetingNotes` fixes seven sections and gives action items owners and due dates. For a
lecture, a podcast, a debugging session or a sitcom, most are empty and the rest are forced. The
model is being asked to fill a form rather than to summarize.

## What to change

1. **Make the section set adaptive.** Sections exist because the content supports them. A planning
   call still yields decisions and action items; a lecture yields topics and explanations; a
   debugging session yields findings. Omit an empty section rather than rendering it blank.
2. **Keep citations mandatory.** Every claim cites the transcript events supporting it, and
   unsupported claims still fail closed. Adaptivity governs which sections exist, never whether a
   claim is evidenced. If a design choice trades these against each other, keep the citations and
   report the conflict — this is the line ADR-0019 forbids crossing.
3. **Supersede rather than edit.** `meeting_notes.v1` and `v2` are meeting-shaped. Introduce a new
   artifact kind and schema id; do not redefine the existing ones. Cached notes from before this
   change must remain interpretable as what they were, and must not silently reinterpret under a new
   schema. The content hash already invalidates on schema id, so use that mechanism.
4. **Retire the meeting vocabulary in the notes surface.** "Meeting notes", "the live meeting
   record", "Sources for this meeting" become recording-centric. Transcript attribution becomes
   captured audio and your microphone rather than "Meeting audio" and "You".
5. **Do not lose meeting quality.** When the content is a meeting, decisions, action items, owners
   and due dates must be as good as they are today. Prove it: run the same meeting transcript
   through the old and new paths and compare. A general summarizer that is worse at meetings is a
   regression, not a widening.

## A defect to fix while here

The "Open transcript evidence" buttons in the notes column overflow their container and the third is
clipped mid-word. T060 built a shrink-priority row primitive for exactly this and it was not applied
to `notes.rs`. Use it. This is the fifth instance of the same defect.

## Acceptance

- A non-meeting recording produces a genuinely useful summary rather than a statement that it is not
  a meeting. Judged against a real captured transcript, by a human.
- A meeting recording still produces decisions, action items with owners and due dates, and open
  questions, at no lower quality than today. Compared side by side, not asserted.
- Every claim carries citations; an unsupported claim still fails closed, asserted by test.
- Empty sections are absent from the output rather than rendered blank.
- The previous artifact kinds remain readable, and a cached pre-change result is not reinterpreted
  under the new schema.
- No meeting-specific vocabulary remains in the notes surface where the content may not be a meeting.
- Evidence buttons stay within bounds at the stated minimum width.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Importing media (T071), microphone-only sessions (T050), the Ask surface, the settings surface
(T069), and any change to the citation contract itself.

## Required by T075 — surface the downgrades — planner, 2026-08-14

T075 rebuilt the summary column and could not complete one acceptance item, because the data is
dropped in a file this task owns.

`GroundedMeetingNotesReport.normalizations` — T067's record of which requested controls the backend
could not honour — is discarded when `NotesController` builds `NotesState`, and neither `Ready` nor
`Stale` carries a field for it. Both live in `crates/app/src/notes/controller.rs`.

Add `normalizations: Vec<ObservedRequestNormalization>` to `NotesState::Ready` and `Stale` and thread
the report's value through. The shape is already settled: the Ask surface renders exactly this from
`AskResult.normalizations`, so follow it rather than inventing a second presentation.

This matters beyond tidiness. Every Codex summary currently loses three controls — `max_tokens`,
`temperature`, and the JSON-object guarantee — and the result is presented as though nothing was
compromised. T067 exists so a downgrade is visible rather than silent; dropping it at the last hop
defeats the whole seam.

T075 surfaces the model and the summary's provenance already. Only the downgrade list is missing,
and it is blocked here.

## Evidence basis is now derived, not declared — planner, 2026-08-14

A re-summarize failed with `action_items declares an evidence basis that does not match its
citations`, discarding an otherwise correct summary because one item's label disagreed with its own
citations.

`basis` carried no information its citations did not already carry. The prompt's rule — "meeting
basis requires meeting citations only, external requires external only, mixed requires both" — was
a restatement of which arrays are non-empty, so the field was a third value the model had to keep
consistent by hand, with no structured output to enforce it. Under Codex that is a pure failure
surface.

`derived_basis` now computes it, `GroundedMeetingNotes::normalize_evidence_basis` rewrites every
declared label from its own citations before validation and before storage, and a mislabelled but
properly cited claim is corrected rather than rejected. The citations are the evidence; the label
was always commentary on them.

**Nothing was relaxed.** A claim citing nothing is still rejected — that check is now the *only*
meaning of `InvalidEvidenceBasis`, and its message says so: "states a claim with no citation to
support it". A citation naming evidence that does not exist still fails closed. Both are asserted
by tests, alongside a third proving a mislabelled-but-cited claim survives with its basis corrected.

When this task supersedes the schema, consider removing `basis` from the wire format entirely
rather than deriving it after the fact. It is a field that can only be wrong.

## Blocks carry identity — planner, 2026-08-14 (ADR-0021)

The new artifact kind this task introduces is the schema two later tasks stand on: T087 renders
`artifact ⊕ user-edit overlay` and merges edits across regenerations; T088 projects blocks to
markdown with Obsidian `^blockid` anchors. Both address individual claims. So the superseding
schema must satisfy, in addition to everything above:

1. **Every claim and action item is a block with a stable id, serialized in the artifact.** Ids
   are content-addressed from the block's citation anchors (its cited `(session, event)` set plus
   section kind), not positional and not model-invented — the model never writes ids; validation
   derives them. Two regenerations whose corresponding claims cite the same moments must yield
   the same block id; that property is the entire merge contract, so assert it by test.
2. **Action items are structured blocks** (text, optional owner, optional due date) rather than
   prose lines, so a checkbox and an owner edit have a field to bind to.
3. Drop `basis` from the new wire format, as this file already argues. Under block identity it is
   doubly redundant.

Scope discipline: this task ships the schema and ids only. No overlay, no editing UI, no
markdown — those are T087/T088. If block identity forces a design conflict with adaptivity or
citations, citations win, then identity, then adaptivity; report the conflict rather than
weakening the first two.
