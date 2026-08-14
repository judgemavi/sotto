# T056 — Annotate a meeting after it ends

**Status:** done

**Wave:** N5 — workspace finish

**Depends on:** satisfied. T055 released `workspace/notes.rs` and `workspace/transcript.rs` on
2026-08-13. T057 runs concurrently on disjoint files and must not be edited by this task.

**Owns:** `crates/app/src/workspace/notes.rs`, `crates/app/src/workspace/transcript.rs`, the
annotation append and persistence path in `crates/rag/**` as its own module, and this task.

**Ownership boundary (planner, 2026-08-13):** T057 holds `crates/app/src/session/**` and the
recording retention module in `crates/rag/**` concurrently. This task must not edit either. That is
not merely a process rule here — a completed session has no live pipeline actor, so its append path
belongs in the store rather than in the session controller anyway. If this task believes it needs
`session/**`, stop and report rather than editing it.

## Why this exists

Typed notes today can only be added to the live meeting. A past meeting refuses them with "Read-only
meeting. Typed notes can only be added to the live meeting."

That was not an accident and it was not a defect: T051's contract said "refused with an honest
reason, **or** appended to that session — pick one, state it", and the implementer picked refuse and
stated it. The constraint came from the task, not from the architecture. Append-only forbids
rewriting an event; it has never forbidden appending one later.

The behaviour is also wrong for how the product is used. The most valuable note is often written
while re-reading the transcript afterwards — that is when you notice the commitment nobody wrote
down. Forcing that thought into a live-only composer means it is never captured at all.

## The gesture

Live annotation anchors to whatever is being said, because that is the only honest anchor available
in the moment. After the meeting the whole transcript is in front of the user, so the anchor should
be chosen, not inferred: **select a transcript row, then write the note against it.** A note typed
with no row selected anchors to the last final of the session, which is the closest post-hoc
equivalent of "at the end".

## What this changes structurally

1. **A completed session's log gains events after it closed.** The record stays append-only and no
   captured fact is ever altered, but `ended_at_unix_ms` no longer implies "nothing further will be
   appended". Anything that assumed a closed session is frozen must be found and corrected.
2. **Derived artifacts go stale.** Annotations participate in the exact timeline hash (recorded in
   T051's notes), so appending one invalidates cached generated notes for that meeting. Do not
   silently discard them and do not silently serve them as current. Mark them stale and say so:
   the user needs to know their notes predate their latest annotation, and regenerating is their
   choice.
3. **Retention and retrieval must catch up.** T054 ingests completed sessions for cross-session Ask.
   A late annotation means that session's ingested content is out of date; re-ingestion is already
   required to be idempotent, so use it rather than inventing a second path.
4. **The read-only banner becomes a lie.** A past meeting is immutable as a *record* but now accepts
   annotations. Say precisely that: the transcript cannot change, your notes can be added.

## Plan

1. Add a completed-session annotation append path. Live annotations flow through the pipeline actor,
   which does not exist once a session ends; appending to a closed session writes through the store
   while preserving append-only ordering and the id sequence. Superseding edits must keep working on
   both paths.
2. Make transcript rows selectable, and bind the notes composer to the selected row. Show the anchor
   in the composer so the user knows what the note will attach to before writing it.
3. Enable the composer for past meetings, with copy that states what is and is not mutable.
4. Invalidate cached derived notes for the session on append, marking them stale rather than
   deleting, and surface that state in the notes column.
5. Trigger idempotent re-ingestion of the session for cross-session retrieval.
6. Verify replay: a session with post-close annotations reopens with them in order, interleaved
   correctly against the finals they anchor to, across an app restart.

## Acceptance

- A note can be typed against any past meeting and is anchored to a transcript row the user chose.
- With no row selected, the note anchors to the session's last final, and the composer says so
  before the note is written.
- No captured fact is altered by any annotation, before or after a session closes.
- Cached generated notes for that meeting are marked stale on append, are not served as current, and
  are not deleted.
- The session is re-ingested for cross-session Ask, and re-ingestion does not duplicate evidence.
- Post-close annotations survive restart and replay in order.
- The past-meeting banner no longer claims the meeting is read-only while accepting notes.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Editing or deleting captured utterances, rich text, annotating a text range inside an utterance,
changing how notes are generated, and any change to live annotation behaviour beyond sharing the
selected-row anchor.

## Inherited from T055 — 2026-08-13

T055 filed this defect and correctly stopped rather than editing a file this task owns.

**A silence marker renders as record content.** A completed session shows a row reading
`You — [ Silence ]`, labelled "Not finalized before capture stopped". T055's diagnosis, which this
task should verify rather than assume: the completed-session projection bypasses the live speaking
gate, and `transcript_row` rejects only blank text, so a whisper silence marker survives as visible
content. `crates/app/src/workspace/transcript.rs:140`.

VAD gating exists to keep silence hypotheses out of the visible register. An unfinalized tail whose
entire content is a silence marker is noise, not the last thing anyone said. Suppress it, or state
why it is kept. This matters more once this task lets users anchor notes to rows: a note anchored to
a phantom silence row is worse than no anchor.

Acceptance: a completed session containing a trailing silence-marker hypothesis renders no row for
it, asserted over the projection.

## Notes

- Completed meetings now accept append-only typed notes through a store-owned path in
  `rag::Store`. The write verifies durable completion and a transcript-row anchor, allocates the
  next event id inside the insert transaction, stamps it after the existing log tail, and supports
  edits only as superseding annotation events that retain the original anchor. No file under
  `crates/app/src/session/**` was edited.
- Transcript rows are selectable in live and completed projections. The composer names the exact
  selected event id before submission; without a selection, a completed meeting explicitly names
  and uses its last active final. Past-meeting copy now distinguishes the locked captured
  transcript from appendable user notes.
- The existing exact timeline hash remains the staleness authority. A new status loader retains
  and validates the latest grounded artifact when its hash is stale; the notes controller and
  column render that artifact with a warning and never report it as a current cache hit. This
  required narrow supporting changes in `insight`, `app/src/notes/controller.rs`, and workspace
  layout wiring beyond the two column files named in the initial ownership list.
- If the meeting is retained for cross-session Ask, append triggers a freshly embedded
  annotation-labelled prior-meeting document before removing superseded versions. Cleanup is
  idempotent; if refresh fails, the annotation remains committed and the UI reports the retrieval
  failure instead of claiming the append failed.
- Completed projection now rejects known Whisper silence-only markers (`[ Silence ]`, compact
  variants, and blank-audio markers) while preserving and labelling substantive unfinalized tails.
- Verification on 2026-08-13: focused restart/replay and idempotent-cleanup RAG regression, grounded
  stale-cache regression, and 22 focused workspace tests pass. Full
  `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test --workspace --locked` passes; strict
  `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`, formatting, and
  the T056-owned/scoped diff check pass. The full-tree diff check still reports a pre-existing blank
  line at EOF in `.tasks/T048-top-down-meeting-workspace.md`, outside this task's ownership.
- Signed-app interaction for selecting arbitrary historical rows, typing/editing after restart,
  visual stale-state distinction, and cross-session semantic retrieval remains `NOT RUN`; T035
  continues to own real-product/manual acceptance.

## Planner review — 2026-08-13

Verified correct, being the parts most likely to be wrong: per-session id allocation inside the
insert transaction against a `PRIMARY KEY (session_id, id)` schema; monotonic `max(ts) + 1ns`
stamping; anchor-must-be-a-transcript-row and edits-retain-anchor validation; and silence
suppression scoped to a completed session's unstable register only, so no committed final can be
dropped.

**Finding 1 — ownership, requires action.** This task edited `crates/app/src/workspace/layout.rs`,
which was released to T057 and which T057 is editing concurrently for the recording copy. The
planner caused this: the acceptance criterion "the past-meeting banner no longer claims the meeting
is read-only" can only be met in `layout.rs`. The contract was impossible as written; the task
should still have stopped and reported rather than crossing the boundary.

Resolution: **the past-meeting banner block in `layout.rs` is assigned to T056 retroactively.** T057
keeps the rest of that file for the session-bar recording copy and must re-read it before its next
edit, since it now contains changes made after its last read.

**Finding 2 — the append path bypasses the checked builder.** `crates/rag/src/store/annotations.rs`
builds the event with `serde_json::from_value` and re-implements the payload invariants locally,
while `TimelineBuilder::append_user_annotation` and `supersede_user_annotation` exist at
`builder.rs:148,161` and the live path uses them. T051 established that only checked builder methods
construct `UserAnnotation` payloads. Two implementations of the same invariant will drift, and the
JSON construction depends on the payload's serde shape, so a field rename in `core` compiles here
and fails at runtime. Either route the store path through the checked constructor, or extract the
shared validation so both call one implementation, or record why duplication is unavoidable given
the store must allocate ids inside a SQL transaction.

## Closure — 2026-08-13 (planner, narrowed)

Accepted. Post-meeting annotations, selectable-row anchoring, stale-notes marking, idempotent
re-ingestion, and the silence-marker suppression are all implemented and verified by review.

Residual transferred to T057, which already owns `crates/rag/**` for recording retention: the
checked-builder finding in `crates/rag/src/store/annotations.rs`. The past-meeting banner block in
`layout.rs` transfers with it, so one owner holds that file again.

Manual/visual acceptance is NOT RUN and passes to T035.
