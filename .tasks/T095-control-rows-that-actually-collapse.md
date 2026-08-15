# T095 — Control rows that actually collapse

**Status:** todo

**Wave:** N7 — v2 workspace

**Depends on:** nothing. `ControlRow::for_width` and its collapse threshold already exist and are
tested (`crates/app/src/workspace/control_row.rs`); nothing in that file needs to change.

**Owns:** the `ControlRow` construction sites and the width plumbing they need in
`crates/app/src/workspace/transcript.rs`, `ask.rs`, `notes.rs`, and whichever caller supplies the
available width. This is a planner-amended exception to per-file ownership: the defect is one
mistake repeated across three columns, and fixing one while the other two keep it is how this bug
reached its fifth recurrence. It does **not** grant redesign of anything it touches — pass the
width, change nothing else.

**Concurrency:** T077 and T078 hold work in these files. Coordinate before starting; if a lane is
active, stop and report rather than racing it.

## Why this exists

`ControlRow` has two constructors and only one of them collapses anything.
`crates/app/src/workspace/control_row.rs:82` drops an `Expendable` child only when
`self.available <= Self::COLLAPSE_WIDTH`, and `available` is set by `for_width`. A row built with
`ControlRow::new()` therefore never sheds a control no matter how narrow the window gets — its
children merely compress.

Three columns build rows with `new()`:

- `crates/app/src/workspace/transcript.rs:740` — `column_head`, whose legend is marked `Expendable`
- `crates/app/src/workspace/ask.rs:226` and `:258`
- `crates/app/src/workspace/notes.rs:113` and `:163`

`crates/app/src/workspace/layout.rs:335`, `:512`, and `:607` use `for_width(width)` and collapse
correctly, so the working pattern is already in the codebase to copy.

This is the same clipping defect the workspace has now hit repeatedly: a control marked expendable
that cannot actually yield, or a row with no `min-w-0` painting its last child mid-word. T074's
review found it in the transcript legend and correctly declined to fix that one in isolation. The
count matters — fixing the transcript alone leaves two columns with the identical defect and a task
board that believes the class is closed.

## The trap in the existing test

T074's mounted test asserted that the narrow legend *remains present* while its measured width
shrinks. That is evidence of the wrong thing: it passes precisely because the row does not collapse.
Any test written for this task must assert **absence** at or below the threshold, not a smaller
width. Check the sibling assertions in `control_row.rs:252-261` for the shape that actually proves
collapse.

## Plan

1. Thread the available width to each of the three columns' head builders, following the parameter
   pattern `layout.rs` already uses. Do not invent a per-column breakpoint — `COLLAPSE_WIDTH` is
   shared on purpose, so every column collapses at the same moment and the window does not become a
   patchwork of independent thresholds.
2. Construct each row with `for_width(available)`.
3. Confirm each column's shrink order is honest: what is marked `Expendable` is genuinely the thing
   that should go first, and nothing essential is marked expendable to make a row fit.
4. Replace or rewrite any test that asserts a shrinking width where it should assert absence.

## Acceptance

- At or below `ControlRow::COLLAPSE_WIDTH`, every `Expendable` control in the transcript, Ask, and
  summary columns is **absent**, asserted by test — not merely narrower.
- Above the threshold each is present with positive bounds.
- No control in any of the three columns clips at the stated minimum width.
- Essential controls — Stop, the clock, the composer's submit — never collapse, and no control is
  reclassified to make a row fit.
- Full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Changing `ControlRow` itself or `COLLAPSE_WIDTH`'s value; the rail (`library.rs`), which already
documents why it uses the shared threshold; restyling any control; and the summary taxonomy.

## Notes

Found while reviewing T076's handoff on 2026-08-15. T074 remains open on its own acceptance and
should close on this task's completion rather than duplicating the fix.
