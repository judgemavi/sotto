# T072 — The workspace shell, its two title bars, and recording vocabulary

**Status:** in-review

**Wave:** N7 — v2 workspace

**Depends on:** ADR-0019 and `docs/design/workspace-v2-mock.html`, which is normative.

**Owns:** `crates/app/src/workspace/layout.rs`, `crates/app/src/workspace/mod.rs`,
`crates/app/src/workspace/tokens.rs`, the control-row primitive under
`crates/app/src/workspace/`, and this task

**Concurrency (planner, 2026-08-14):** T073 holds `library.rs`, T074 holds `transcript.rs`, T075
holds `notes.rs`. T069 holds `settings/mod.rs` and `ask.rs` until it closes. Edit none of them. You
own `mod.rs`, so those tasks route module registration through you.

## What the mock changes

The shipped shell has one bar and four permanent columns. The mock has **two distinct bars**, and
the column set depends on state.

- **`captureBar`** — shown while recording. Left to right: a record dot, the kind, the target name,
  scope chips, an elapsed clock, Pause, Stop.
- **`viewBar`** — shown when a stopped session is open. Title, an `Imported` badge when relevant,
  duration and size, a centred **Notes / Transcript** tab pair, Reveal recording, Delete.

The tab pair matters: a stopped session is a **single tabbed stage**, not two side-by-side columns.
Notes is the default and Transcript is one tab away, because after a recording ends the summary is
what a person came for. While recording, transcript and notes sit side by side instead.

## Shrink priority is already specified

The mock marks every control with its role: `keep` (never clipped), `ellip` (truncates with an
ellipsis), `collapses` (drops when space runs out). That is exactly T060's essential / ellipsizing /
expendable primitive, and the mock tells you which role each control takes — `Stop` and the clock
are `keep`, the target name is `ellip`, Pause and the scope chips are `collapses`.

Use the primitive. Do not bound widths at call sites; that approach has now produced five separate
clipping defects.

## Vocabulary

Per ADR-0019, and visible in the mock's markup: **Library**, **New recording**, **Import…**,
**captured audio**, **your microphone**. "Meeting" survives only where the content genuinely is one.
This task owns the shell's share of that change; the columns own theirs.

## Plan

1. Build the two bars with the mock's control order and shrink roles.
2. Drive them from session state: idle, recording, stopped-session-open.
3. Implement the Notes/Transcript tab switch for a stopped session, and the side-by-side arrangement
   while recording.
4. Align the design tokens with the mock's custom properties, both themes. Dark is the maintainer's
   daily theme and must be the better of the two.
5. Retire shell vocabulary that ADR-0019 supersedes.

## Acceptance

- Stop and the elapsed clock are fully visible and clickable at the stated minimum width while
  recording; the target name ellipsizes; Pause and scope chips collapse before anything essential.
- A stopped session opens on Notes with Transcript one tab away, and switching preserves scroll
  position in each.
- Both themes render with no color defined only inside a theme branch.
- A narrow-width test covers both bars and fails if a future control declares no role.
- Focused and full app tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

The library rail (T073), transcript rows (T074), the summary (T075), Settings and Ask (T069),
microphone-only capture (T050), and import (T071).

## Notes

### What was built

**`tokens.rs`** — the palette is now the mock's `:root` block, value for value, in both themes. Every
field is defined in both branches of `WorkspaceTokens::resolve`, so a colour cannot exist in only one
theme: the struct literal makes that a compile error rather than a review question. Added
`accent_ink`, `live_ink`, `live_line`, `scrim`, and the two on-accent inks the mock writes inline.
Two names the mock dropped are kept and documented because the columns use them: `surface_2` is the
mock's `--hover`, and `ink_2` is the mid-weight body ink the mock has no token for. `TypeScale` gained
`CHIP`, `CONTROL` and `TITLE`, and `CLOCK`/`BODY` moved to the mock's 17 px / 13 px.

**`control_row.rs`** — the T060 primitive now carries the mock's third behaviour. `.keep` /
`.ellip` / `.collapses` map to `Essential` / `Ellipsizing` / `Expendable`; `Ellipsizing` became
`flex: 1 1 auto` (exactly `.ellip`) so a title is squeezed only by real pressure rather than by
sharing free space with the spacers that centre the view bar's tabs; and `Expendable` is now *dropped*
below `ControlRow::COLLAPSE_WIDTH` (760 px) rather than painted mid-clip. `finish()` returns
`ControlRowElement`, which forwards `Styled`, `InteractiveElement` and `Element` to the row it wraps
but deliberately does not implement `ParentElement` — a control appended after the fact would carry no
role, and that is now a compile error rather than a clipping defect. `ControlRow::new()` remains for
rows inside a container whose width the caller cannot see (the rail, column heads); it never drops,
because it has no honest basis for deciding it must.

**`layout.rs`** — two bars, and a stage whose arrangement is derived, never stored:

- `capture-bar` — record dot, kind, target name, scope chips, clock, Pause, Stop, in the mock's
  order and with the mock's roles. It follows the *lifecycle*, not the stage, so ADR-0015's "Stop is
  always one visible action away" holds even while the user reads a stopped session.
- `view-bar` — title, duration · size, started-at, centred Notes/Transcript tab pair, Back to
  recording, Re-transcribe, Reveal recording, Delete.
- `Stage::Idle` mounts T073's `render_start_choices`; `Stage::Recording` puts transcript and notes
  side by side at the mock's 1.35 : 1; `Stage::Review` is one tabbed stage.
- The library rail is the mock's fixed 248 px (210 px under 980 px). `h_resizable` is gone.

**`mod.rs`** — `StageTab` (Notes default), `OpenRecording` (path/duration/size read once on open,
not per frame), `delete_armed`; `select_stage_tab`, `return_to_live`, `reveal_open_recording`,
`delete_open_session`; `open_citation` now routes through T074's `reveal_citation`; ADR-0019
vocabulary throughout the shell's strings.

### Cross-task items delivered

- Mounted `library::render_start_choices` in the idle stage and deleted its now-stale
  `#[expect(dead_code)]`.
- Built `CitationTimes` from `frame.transcript` and passed it to `notes::render_with_citation_times`,
  so chips read `mm:ss` instead of `#412`.
- Routed `open_citation` through `MeetingWorkspace::reveal_citation`.

### Acceptance

| Item | Verdict |
|---|---|
| Stop and the clock fully visible and clickable at 680 px; target name ellipsizes; Pause and chips collapse first | **PROVEN** — `a_narrow_capture_bar_keeps_the_clock_and_a_clickable_stop` asserts bounds containment, collapse, ellipsis order and a real click that reaches `Stopping` |
| A stopped session opens on Notes with Transcript one tab away, and switching preserves scroll | **PROVEN** — `a_stopped_session_opens_on_notes_with_transcript_one_tab_away` over an 80-row transcript |
| Both themes render with no colour defined only inside a theme branch | **PROVEN** structurally (both branches of one struct literal; no colour literal outside `tokens.rs`) and by `the_shell_mounts_and_renders_in_both_themes` |
| A narrow-width test covers both bars and fails if a future control declares no role | **PROVEN for both bars** (`a_narrow_capture_bar_…`, `a_narrow_view_bar_keeps_the_tabs_and_delete`). "Declares no role" is prevented at compile time by `ControlRowElement`, not by a runtime test — a role-less control cannot be written |
| Focused app tests, strict Clippy, formatting, diff checks | **PROVEN** — 162 app tests, `clippy --workspace --all-targets --all-features -D warnings`, `cargo fmt --all --check`, `git diff --check` over the owned files, all clean |
| Signed-app launch on real capture | **NOT RUN** — belongs to T035. The automated evidence builds and clicks a real GPUI render tree in both themes, which is the check the board added after five tasks passed without one |

### Deviations from the mock, and why

- **The mock's two collapse breakpoints (900 px chips, 700 px `.collapses`) became one at 760 px.**
  T072 specifies a single expendable role; two thresholds would need a second role the task does not
  define. The consequence is that chips leave slightly earlier than the mock, never later.
- **The titlebar and statusbar are not built.** The task enumerates `captureBar`, `viewBar`,
  `viewTabs`, the `:root` properties and the shrink classes as this task's share. The statusbar
  carries a load-bearing privacy claim, so it is preserved for now as a `stays on this Mac` scope
  chip in the capture bar. A follow-up should own the real statusbar and reclaim that chip.
- **`Imported` badge is not rendered.** There is no import provenance to read: `CaptureTarget` has no
  imported variant and `SessionSummary` carries no origin. Rendering a badge that can never be true
  would be decoration. Blocked on T071.
- **The view bar carries three controls the mock does not** — started-at, Back to recording,
  Re-transcribe — all `collapses`. Back to recording preserves ADR-0015's return path when a
  recording runs while a past session is open; Re-transcribe is existing shipped functionality with
  no other home; started-at is what the old bar showed.
- **`h_resizable` was removed** in favour of the mock's fixed rail. Column resizing is gone.

### What GPUI could not express, and what that cost

- **There is no `display: none`.** A tab switch that unmounts the hidden column would lose its scroll
  position, because GPUI drops element state — including `gpui_component`'s `use_keyed_state` scroll
  handle — the moment an element stops rendering for a frame. The review stage therefore keeps both
  layers mounted and parks the inactive one at `left: 100%` inside a clipped stage. Correct, and the
  cost is that both columns render every frame while a stopped session is open.
- **There are no media queries.** Collapse is driven by `window.viewport_size().width` handed to
  `ControlRow::for_width`. That is fine for the two full-width bars and is why `ControlRow::new()`
  still exists for rows that do not know their own width.
- **`Frame::clear` does not clear `debug_bounds`.** A control painted in an earlier, wider frame stays
  in the map forever, so "this control collapsed out" cannot be asserted after a resize. The
  narrow-width tests open their window at the target size instead of resizing into it. Any future
  collapse test must do the same or it will pass against a stale entry.

### For the reviewer

- `MeetingWorkspace` still carries "meeting" in its type name and `select_meeting` in its method name.
  Both are referenced outside this task's `Owns` list (`main.rs`, `library.rs`, `ask.rs`), so renaming
  them needs a task that owns those files. No user-visible string in the shell says "meeting".
- `notes::render` — T075's thin wrapper that passed an empty `CitationTimes` — was deleted when the
  shell started calling `render_with_citation_times`. Under `-D dead-code` it was that or a
  module-wide `expect`, which would have blanketed all future dead code in `notes.rs`.
- The tab-switch test depends on row height. T074 warns that it changed row metrics to the mock's; it
  passes now, but re-check it rather than assume if layout metrics move again.
