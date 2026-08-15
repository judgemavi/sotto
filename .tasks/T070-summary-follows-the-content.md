# T070 — Summarize what was recorded, not what a meeting would have been

**Status:** in-progress

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

## Notes — insight/schema handoff, 2026-08-14

- Review follow-up: legacy provider and `meeting_notes.v2` cache projections consolidate
  same-anchor blocks before stable ids are exposed. Map-reduce projects finalized partials back
  to the provider draft shape so Sotto-owned ids cannot be echoed. Block identities now use a
  truncated SHA-256 digest rather than two related FNV passes.
- Follow-up correction: legacy action ids use only the action text's base citations, matching the
  adaptive finalizer and persisted-id validator. Owner and due-date citations remain separately
  validated evidence and do not silently change block identity.

The unblocked schema and prompt half is implemented. `recording_notes.v1` / `recording_notes/v1`
is now the canonical grounded-summary artifact and cache identity. Its wire format is adaptive
`sections[]` containing typed claim or structured action blocks; it has no `basis` field. Provider
responses omit ids, then validation derives each `recording-block-v1-*` id from the recording id,
section kind, and the block's sorted, deduplicated citation anchors. Rewording and citation order do
not change identity. Same-section claims with the same anchors consolidate deterministically into
one addressable block rather than producing duplicate ids; distinct text is retained.

Every claim still requires a known meeting or external citation. Owner and due-date claims retain
their own separate evidence requirement. Empty sections, duplicate sections, wrong block kinds,
unknown citations, uncited claims, duplicate persisted ids, and ids that do not match their derived
address all fail closed. The v3 prompts summarize the content actually recorded and name useful
non-meeting sections (`explanations`, `findings`) while retaining the meeting-quality sections.

The handoff is explicit: `GroundedMeetingNotesReport.artifact` carries the canonical adaptive
artifact, while `report.notes` remains a temporary v2 projection so the in-review app compiles
until the app-owned half can switch the column to `artifact`. New runs persist only
`recording_notes.v1`. The latest-artifact loader prefers that kind and falls back to a readable
`meeting_notes.v2`, converting it explicitly to the adaptive representation; the separate legacy
`MeetingNotesGenerator::generate` path still reads and cache-hits `meeting_notes.v1` unchanged.
During this app handoff, the provider parser also accepts the former seven-field response and
canonicalizes it before validation/storage; it never persists that wire shape. This compatibility
can be removed when the app fixtures and column consume `artifact` directly.

Focused evidence:

- `cargo test -p insight` — 49 passed across unit and integration targets, including stable ids,
  same-anchor consolidation, no-basis serialization/rejection, citation failures, canonical v3
  persistence/cache replay, and explicit v2 fallback readability.
- `cargo clippy -p insight --all-targets --all-features -- -D warnings` — clean.
- `cargo check -p app --lib` and the transitional `cargo test -p app --lib` — clean; all 230 app
  tests pass without changing the in-review notes column.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test --workspace --locked` and strict workspace Clippy
  over all targets/features — clean; live/model/performance gates remain explicitly ignored.
- `cargo fmt` and scoped `git diff --check` — clean.

Still NOT RUN / blocked on the app-owned handoff: rendering adaptive section kinds directly,
recording-centric surface vocabulary, downgrade threading in `crates/app/src/notes/controller.rs`,
the evidence-button minimum-width fix in `crates/app/src/workspace/notes.rs`, rerunning the green
automated gates after that handoff, the real non-meeting captured-transcript judgment, the
old/new meeting-quality side-by-side comparison, and signed-app/manual acceptance. T075/T076 remain
in review, so this pass deliberately touched no `crates/app/**` file.
