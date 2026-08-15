# T073 — The library rail and the three ways a session begins

**Status:** in-review

**Wave:** N7 — v2 workspace

**Depends on:** ADR-0019 and `docs/design/workspace-v2-mock.html`, which is normative.

**Owns:** `crates/app/src/workspace/library.rs` and this task

**Concurrency (planner, 2026-08-14):** T072 holds `layout.rs`, `mod.rs` and `tokens.rs`; T074 holds
`transcript.rs`; T075 holds `notes.rs`; T069 holds `settings/mod.rs` and `ask.rs`. Edit none of
them. If you need a module registered, ask T072 rather than editing `mod.rs`.

## What the mock changes

The rail becomes a **Library**, not a list of Meetings. Its head carries the label and a count. Below
it sit two actions — **New recording** (primary) and **Import…** — then a search field placed
*below* the actions, then the grouped list.

The bigger change is the idle state. When nothing is selected the transcript column shows three
**equally weighted entry points**, not one button:

- **Capture an app** — screen and audio from one app, nothing else heard
- **Just my microphone** — audio notes, no picker and no screen capture
- **Import a file** — an audio or video file you already have

The mock states the product thesis above them: *"Record anything you can hear."* Microphone-only and
import are first-class ways to start, not degraded fallbacks, and the design must not rank them by
making one a button and the others links.

## What exists and what does not

- **Capture an app** works today.
- **Microphone-only** is T050, currently `in-review`.
- **Import** is T071, not started.

Render all three. Where the capability is not yet wired, the entry point must say so plainly and do
nothing, rather than appearing to work. Do not stub a fake session.

## Plan

1. Rebuild the rail head, actions and search to the mock's order and vocabulary.
2. Group sessions as the mock does, with an imported badge where relevant, and titles that ellipsize
   from the start rather than clipping — this closes T055's long-deferred rail-title defect.
3. Build the three entry points with equal weight.
4. Route each to its capability, or to an honest unavailable state.
5. Make the search filter recordings and notes as its placeholder claims, or change the placeholder.

## Acceptance

- The rail lists a session from the moment it starts and after it ends, with no app restart.
- Titles preserve their beginning and truncate with a visible ellipsis at the stated minimum width.
- All three entry points are present and visually equal; an unwired one states its own unavailability
  and cannot create a session.
- Search does what its placeholder claims.
- Focused and full app tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Implementing microphone-only capture (T050) or import (T071), the shell bars (T072), transcript rows
(T074), the summary (T075), Settings and Ask (T069).

## Notes

All work is in `crates/app/src/workspace/library.rs`. Nothing else was touched.

### The rail

Rebuilt to the mock's order and vocabulary: a `LIBRARY` eyebrow with a right-aligned count, the two
actions, the search field *below* the actions, then the grouped list. Grouping is now calendar-based
(`Now` for a running recording, then `Today` / `Yesterday` / weekday / dated), replacing the previous
`Today` / `This week` / `Earlier` buckets. A running recording is hoisted to the front of the sort so
`Now` always leads. Each row carries a glyph (`▣` captured, `●` microphone-only), the title, and a
mono meta line: `recording` while live, otherwise `12:34 · captured` or `12:34 · microphone`.

`group_rail` is a pure function over `SessionSummary`, so grouping, labelling, sorting and filtering
are tested without a window.

### The title-truncation defect

Row titles go through T060's control-row primitive with the title in the `Ellipsizing` role and the
glyph `Essential`; no width is bounded at the call site. T072 reshaped that primitive mid-flight
(`ControlRow::new` → `ControlRow::for_width`, and `finish()` now returns a `ControlRowElement` that
is not stateful), so the clickable row is a `div().id(..)` wrapper *around* the control row rather
than the control row itself. Roles are unchanged by that.

`a_long_rail_title_keeps_its_beginning_inside_the_rail_at_minimum_width` renders the real workspace
at 680 px with a long window title and asserts the title's bounds start at or after the rail's left
edge and end at or before its right edge, with non-zero width. Truncation is `text_ellipsis`, so the
beginning is preserved and the tail carries the ellipsis glyph.

### The three beginnings

`START_CHOICES` is the model; `render_start_choices` is the surface. All three are the same card:
same size, same border, same glyph plate, same type scale. None is a button while the others are
links, and none is greyed into meaninglessness.

- **Capture an app** — wired. Calls `MeetingWorkspace::start_scoped_session`.
- **Record just your microphone** — **wired**, contrary to the task's assumption. T050 is still
  `in-review` but its app-side capability has landed: `SessionController::start_microphone_only`
  exists and drives `CaptureSelection::MicrophoneOnly`. Rendering it inert would have been a lie in
  the other direction, so the card calls it.
- **Import audio or video** — **not wired**, and says so in place: *"Not built yet — Sotto cannot
  turn a file into a session, so this does nothing."* The card renders with a default cursor and no
  click handler at all, so there is no code path from it to a session.

A ready card also states its own blockage when a recording is already running, rather than silently
doing nothing.

The rail's `Import…` button is disabled, carries the same sentence as a tooltip, and is followed by
a persistent one-line caption under the actions row. A test clicks it and asserts the session
lifecycle is still `Idle`.

### Handoff to T072 — `render_start_choices` is built but not mounted

The mock puts the idle surface in the transcript column. `layout.rs` and `transcript.rs` belong to
T072 and T074, so this task could not mount it. `render_start_choices(can_start, cx)` is complete and
carries an `#[expect(dead_code, …)]` that goes stale — and therefore fails the build — the moment it
is called. **T072: mount it where the transcript column currently renders its idle empty message, and
delete that expectation.** Until then the three-card surface is not on screen; the rail's own two
actions are.

### Two strings T073 does not own

- The search placeholder is set in `mod.rs` as `"Filter transcript and notes"`. The mock says
  `"Search recordings and notes"`, and `InputState` only accepts a placeholder at construction or
  through `set_placeholder(.., window, cx)`, which is not callable from render. The index behind it
  already covers titles, transcript finals and partials, derived transcripts, user annotations and
  generated notes — so the current wording is truthful, just not the mock's. **T072 owns the change.**
- The mock's `⌘N` / `⌘⇧N` / `⌘O` shortcut hints were deliberately dropped: the app registers no such
  key bindings, and printing a shortcut that does nothing is the same defect as a dead button.

### Not delivered

- **The `imported` badge.** Nothing in `SessionSummary` or `CaptureTarget` can express "this session
  came from a file" — that provenance is T071's to add per ADR-0019. No badge was invented, and the
  `⇥` glyph is reserved for it.

### Review follow-up — 2026-08-14

The review found that library search appended `Debug` output for generated notes. That indexed
Rust schema field names as if a person had written them, so queries such as `risk` or `topic` could
match an unrelated summary. Search now appends only rendered claim/action text plus visible owner
and due-date values. `summary_search_indexes_written_content_not_schema_field_names` covers the
false-positive and content-match paths. The focused test passes; the task remains in review for its
integrated visual/owner gate.
