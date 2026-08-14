# T049 — Two-column meeting workspace and session library

**Status:** done

**Wave:** N4 — workspace layout

**Depends on:** T048 accepted. T048 currently owns `main.rs`, `notes/view.rs`, `AGENTS.md`, and
`.tasks/README.md`; do not start while it is `in-review`.

**Owns:** `crates/app/src/workspace/**` (new), `crates/app/src/notes/view.rs` (moved and retired),
`crates/app/src/notes/mod.rs`, `crates/app/src/main.rs` for mounting,
`docs/adr/0016-two-column-meeting-workspace.md`, the UI-model section of `AGENTS.md`,
`.tasks/README.md`, and this task

## Why this exists

ADR-0015 replaced the Board with one vertically ordered record: transcript first, notes beneath.
Reviewing an interactive mock of the shipped shape showed that vertical ordering is wrong for how
the workspace is actually used. Notes are now generated on request, so they are a working surface
the user reads *against* the transcript, not an appendix consumed after it. Every citation is a
lateral glance in a two-column layout and a scroll in a stacked one.

This task changes only the projection. ADR-0015's substantive rulings all stand: no Board lens, no
canvas, no zoom or pan, no spatial overlay, citations focus the exact transcript `EventId`,
follow-live pauses on manual scroll, and visual calm is a hard requirement.

The second reason this task exists is structural. It splits one 1101-line `notes/view.rs` into
column modules so that T051, T052, T053, and T054 can each own a separate file and run
concurrently under the parallelism rule. Getting that split right is as much of the deliverable as
the layout.

## Plan

1. Write `docs/adr/0016-two-column-meeting-workspace.md`. It amends ADR-0015's ordering only, and
   must say so explicitly: what changed (stacked → side-by-side, plus a session rail and a
   collapsible ask dock), what is unchanged (every other ADR-0015 ruling), and why on-request note
   generation is the reason the ordering flipped.
2. Create `crates/app/src/workspace/` and move `MeetingWorkspace` into it, split as:
   - `mod.rs` — the four-pane layout, dock and resize wiring, session selection state, and the
     shared `focused_event` citation focus that crosses columns.
   - `library.rs` — the session rail: the persisted meeting catalogue currently rendered inline in
     `notes/view.rs`, grouped by recency, with a text filter over persisted transcript text, and
     the two session-start controls.
   - `transcript.rs` — the transcript column. Behaviour unchanged in this task; T052 owns its
     rendering rewrite.
   - `notes.rs` — the notes column: the existing on-request `generate-notes` control, cited note
     cards, and MCP source receipts.
   - `ask.rs` — a stub panel that renders a disabled, honest empty state. T053 owns its behaviour.
   No behaviour changes ride along with the move; a reviewer must be able to read the split as a
   move plus wiring.
3. Lay the columns out with `gpui_component::dock` for the collapsible ask panel and
   `gpui_component::resizable` for the dividers. Both are present in the pinned 0.5.1; do not
   hand-roll either.
4. Ask panel is collapsed by default and its expanded state persists across app launches with the
   other window state.
5. Selecting a past session opens it read-only: a banner naming the session, its capture target and
   date, plus a `read-only` marker and a `Back to live` control. A running session keeps recording
   and keeps its rail entry marked live while a past session is open.
6. Keep the idle state honest: with no session running the transcript column states that nothing is
   being captured and offers the two start controls, and the notes column states that no notes have
   been generated.

## Contract

- `crates/app/src/workspace/{transcript,notes,ask,library}.rs` are the ownership units for
  T051–T054. Each later task writes one file and does not touch its siblings.
- `mod.rs` exposes the citation focus entry point (`open_citation`, currently in `notes/view.rs`)
  so a citation raised in any column can focus a transcript row in another.
- `library.rs` renders both session-start controls but owns neither capture path; the microphone
  control calls the session-controller method that T050 lands.

## Acceptance

- The shell constructs no `BoardCanvas`, Board tab, zoom or pan control, and no canvas overlay.
- Transcript and notes render as side-by-side resizable columns; the ask panel collapses to a rail
  and restores; the session rail lists persisted sessions and filters by note and transcript text.
- A citation in the notes column focuses the exact transcript row in the transcript column without
  scrolling the notes column.
- Opening a past session while recording does not stop, pause, or reorder the live session, and
  `Back to live` returns to it with follow-live intact.
- No file under `crates/app/src/workspace/` exceeds ~400 lines, so the ownership split is real.
- Focused and full app tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Ask panel behaviour (T053), transcript streaming presentation (T052), typed annotations (T051),
microphone-only capture (T050), and deleting the historical board implementation files.

## Notes

- ADR-0016 amends only the ordering decision from ADR-0015. The mounted shell remains free of Board
  construction and uses `gpui_component::resizable` for the session/transcript/notes dividers and
  `gpui_component::dock` for the collapsible Ask rail.
- Ask expansion persists in `workspace-state.json` beside the session database. The file contains
  only the boolean layout choice.
- The session filter indexes persisted final/partial transcript text, typed annotation text already
  present in replay, target names, and the latest integrity-checked generated notes.
- T050 has not landed its microphone-only capture path, so the required rail control is rendered
  disabled with an explicit reason. T050 owns enabling and wiring that existing control.
- `cargo check -p app --all-targets` passes with a temporary build-only shader shim. A normal app
  build is `NOT RUN`: Xcode 26.4.1 reports that the Metal Toolchain component is missing before Rust
  app code is compiled. The shim validates Rust types only and is not runtime/visual evidence.
- After the sequential T051–T054 handoffs, their substantive column modules now exceed the
  foundation's approximate 400-line sizing target. The ownership boundaries remain separate, but a
  further internal submodule split is a review residual if the line budget is enforced on the
  combined feature tree rather than at the T049 handoff.
- The full workspace test suite passes outside the sandbox (local mock servers/process cleanup
  require it), and strict app Clippy plus formatting/diff checks pass. Signed-app layout, restore,
  past-while-live navigation and citation-focus acceptance remain `NOT RUN`.

### Closure — 2026-08-13 (planner, narrowed)

Accepted on the structural contract only: four panes, dock and resize wiring, session rail, shared
citation focus, and the module split that let T051-T054 run concurrently. That split is real and did
its job.

Narrowed, with residuals assigned to T055:

- The ~400-line budget was missed after the sequential handoffs (`notes.rs` 639, `mod.rs` 530,
  `ask.rs` 495). Ownership boundaries held, so this is accepted as a sizing residual, not a failure.
- `layout.rs:50` formats `meeting.started_at_unix_ms` into user-facing copy as
  "started 1786625633040".
- The two-window shell and the absence of any design token system are the substance of T055.
- Per-frame cost: `render` calls `selected_events(cx)`, which clones the full event vector every
  frame. Handed to T055 with the related annotation cost below.

Visual and signed-app acceptance were never run here and are not claimed.

