# T051 — Typed annotations on the record

**Status:** done

**Wave:** N4 — workspace layout

**Depends on:** T049 for `workspace/notes.rs`

**Owns:** the `UserAnnotation` builder path in `crates/core/src/timeline/builder.rs`
(planner-amended core ownership), `crates/app/src/workspace/notes.rs`, the annotation persistence
mapping in `crates/rag/**`, and this task

## Why this exists

The user must be able to write their own note during or after a session. Today they cannot, even
though the data model already expects it: `EventPayload::UserAnnotation`, `EventKind::UserAnnotation`
and `UserAnnotation { anchor, text, mark }` all exist in `core`, the CLI already parses
`annotation.user`, and `EventClass::UserInteraction` already classifies it away from captured fact
and system output. Nothing anywhere produces one. This task closes a hole that was designed and then
never wired.

A typed note is a first-class meeting fact in the sense that matters: it is the user's own words,
append-only, never rewritten by the model, and never mixed into derived output. The generated notes
column and the typed notes card must stay visually and structurally distinct.

## Core amendment

`TimelineBuilder` has no checked constructor for `UserAnnotation`, and proposal payloads show the
house pattern: only checked builder methods may construct one. The planner grants this task
ownership of that constructor path in `builder.rs`. Do not widen `UserAnnotation` itself, and do not
touch other payloads.

## The anchor decision

`UserAnnotation.anchor` is a required `EventId`, not an option. A note typed before any event exists
therefore has nothing valid to point at. Resolve this explicitly rather than by accident — pick one
and record it in `## Notes`:

- anchor to the most recent event of any kind, including the session's first `vad` or `error` event,
  so an anchor always exists; or
- keep the composer inert until the timeline has at least one event, and say why in the placeholder.

Do not make the anchor optional in `core` to dodge the choice.

## Plan

1. Add the checked `TimelineBuilder` constructor for `UserAnnotation`, mirroring how proposal
   payloads are constructed and validated. Reject empty text.
2. Add the composer to `workspace/notes.rs`: a single-line `gpui_component` `Input` pinned below the
   notes column, submitting on Return. On submit, append an annotation anchored per the decision
   above, stamped with the session-relative time at which it was typed.
3. Render annotations in both columns: pinned beneath the anchored transcript row, and listed in a
   typed-notes card at the top of the notes column with a control that focuses the anchored row.
   Style them so a reader can never confuse a typed note with a generated one.
4. Editing appends a superseding annotation referencing the superseded id. Nothing mutates. Deleting
   is out of scope for this task; do not fake it with a mutation.
5. Persist and replay: annotations survive a session round-trip, replay through `replay_lenient` in
   order, and reopen with a past session read-only.
6. Reasoning consumers see annotations as user interaction, never as captured meeting fact and never
   as model output. Confirm the notes generator either uses them as explicitly labelled user input
   or ignores them — and state which in `## Notes`.

## Contract

- Annotations are appended by the app and read by anything replaying the timeline. T053 and T054 may
  cite them, and must label them as the user's own words rather than as captured speech.

## Acceptance

- A note typed during a live session appears in both columns, anchored to the correct transcript
  row, and its focus control scrolls that row into view.
- Notes typed while a past session is open are refused with an honest reason, or are appended to
  that session — pick one, state it, and do not silently misfile them into the live session.
- Annotations survive persistence and replay in order, including across app restart.
- Nothing mutates an existing event; a superseding annotation references the superseded id.
- Generated notes and typed notes remain distinguishable in the rendered column.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Deleting annotations, rich text, tags beyond the existing `MarkKind`, annotating a specific text
range inside an utterance, and any change to how notes are generated.

## Notes

- The composer stays inert until the selected live session has an active utterance. It stores the
  most recent active partial or final utterance id, rather than VAD, prosody, diagnostics, or a
  prior annotation. Because rolling partial ids are transient, transcript projection and focus
  resolve that durable id forward through `supersedes()` into the current partial and eventually
  the settled final. The original annotation anchor is never rewritten.
- Past sessions remain read-only. Return on their disabled composer is refused with an explicit
  explanation; text is never redirected into a concurrently running meeting.
- An edit uses `supersede_user_annotation` and retains the original transcript anchor. The old
  event remains durable while replay projects only the active replacement. Deletion remains out of
  scope.
- Meeting-notes generation ignores annotations as prompt input today: its transcript renderer
  selects final utterances only. Annotation payloads still participate in the exact timeline hash,
  so adding or editing one invalidates a cached derived artifact without presenting the user's
  words as captured speech or model output.
- Verification on 2026-08-12: full `core` tests (29 total across unit/integration), full `rag`
  tests (19 passed, one opt-in model-cache benchmark ignored), full Rust-only `app` tests (90),
  strict all-target/all-feature Clippy for `core` and `rag`, formatting, and owned-file diff checks
  pass. The shared pipeline/session seam now carries both append and superseding-edit requests
  through the sole timeline actor, and the live Edit control fills the composer before Return
  appends the replacement. Strict app Clippy now passes. The app test used the documented fake
  Metal-tool PATH and proves Rust types/tests only, not runtime UI behavior. Live typing,
  focus/scroll, restart, and visual distinction remain `NOT RUN` manual product acceptance.
- Changes-requested defect resolved on 2026-08-13: annotations pinned to an in-progress partial no
  longer disappear when the next partial or final supersedes it. The unstable strip renders the
  pinned annotation while speech is live, the committed row inherits it after finalization, and
  `Show anchor` resolves the same chain before focusing. A partial → partial → final regression
  test proves both the active-row projection and preservation of the original durable anchor.

### Closure — 2026-08-13 (planner, narrowed)

Accepted. The append-only annotation path, the anchor decision, superseding edits, and persistence
round-trip all hold.

Review history worth keeping:

- Review found that anchoring to the newest active utterance meant anchoring to a rolling partial,
  which `pipeline/session.rs:214` supersedes roughly every 500 ms and `builder.rs:615` then drops
  from the active set. Any note typed while someone was speaking lost its transcript pin within
  about a second. The implementer fixed it by resolving the supersession chain at render time while
  keeping the stored anchor immutable, with a multi-hop partial -> partial -> final regression test.
  Verified by reading both the fix and the test.
- The reviewer, not the implementer, applied the double-lease fix in this task's owned file on
  2026-08-13: `render_source_context` read the `MeetingWorkspace` entity back through `cx.entity()`
  while inside its own render, aborting the process in `did_finish_launching`. It now takes
  `servers` and `selected_grant` as parameters. Recorded here because the ownership trail matters
  more than the size of the change.

Residual to T055: `annotations_by_anchor` runs a full `replay_lenient` per annotation per frame, on
the render path.

Live typing, focus, restart and visual distinction remain unrun and pass to T055 and T035.

