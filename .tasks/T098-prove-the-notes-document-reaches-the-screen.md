# T098 — Prove the notes document reaches the screen

**Status:** todo

**Wave:** N8 — entry workspace

**Depends on:** T087, which built the overlay and is `in-review` pending this. Nothing else.

**Owns:** mounted coverage in `crates/app/src/workspace/notes.rs` and
`crates/app/src/notes/controller.rs`, and this task. **Test-only.** If proving an acceptance item
requires a production change, that is a finding — report it rather than changing behaviour under
cover of adding a test.

## Why this exists

T087 built the overlay correctly and proved it thoroughly at the data layer: append-only replay
after reopen, matched edits surviving regeneration verbatim, orphaned edits kept and flagged, and —
the property that matters most — a sentinel user block that never appears in any serialized provider
input.

Two of its acceptance items are claims about what a person *sees*, and neither is proven:

- *"A generated block can be reworded; the document **shows** the user's words with provenance."*
- *"A user-added block never carries or implies a citation; provenance for all three block states is
  **visible** and correct."*

`workspace/notes.rs` gained 401 lines and `notes/controller.rs` 91 — composed-document rendering,
provenance, and hover actions for reword, add, hide, reorder, check/uncheck, owner, and due date.
The app suite was 249 tests before that change and 249 after. None were added.

So the model is proven and the presentation is not, in a task whose entire premise is a document you
can see both authors in. A composition that is correct in memory and wrong on screen fails the task
exactly as badly as the reverse.

## This is not untestable UI

`crates/app/src/workspace/notes.rs` already carries mounted coverage using `TestAppContext`,
`VisualTestContext`, `debug_bounds`, `assert_in_column`, and `simulate_click`. That harness is how
the *absence* of the per-claim evidence controls is currently pinned. It was available and unused.

Read the existing `rendered` test module before writing anything — particularly
`the_summary_reads_as_prose_and_gives_up_its_evidence_only_when_asked`, which mounts a real
workspace over a temp database and follows a citation chip to `focused_event`.

**One harness trap, recorded in T076 and worth repeating here:** `VisualTestContext::debug_bounds`
never clears between frames, because `Frame::clear()` skips it. An `is_none()` assertion therefore
only proves something for a selector that has **never** been drawn in that window. An assertion that
an element *disappeared* after being drawn will pass whether or not it did. Where this task needs to
prove something is hidden after being shown — a hidden block, say — assert over the composed state
directly rather than over bounds.

## What to prove

1. **A reworded block shows the user's words, not the model's.** Mount a summary, apply a reword
   through the same path the UI uses, and assert the rendered text is the user's — while the stored
   artifact is byte-unchanged, which the data layer already asserts and this must not contradict.
2. **All three provenance states are visibly distinguishable**: generated, user-edited, user-added.
   A reader must be able to tell which author a block came from without opening anything.
3. **A user-added block carries no citation affordance.** Not merely an empty citation list — no
   chip, and nothing implying evidence exists for a sentence the model never wrote.
4. **An orphaned edit is visible and marked.** T087 keeps it and flags it in the composed document;
   prove the flag reaches the screen, since silently showing an orphaned edit as though it were
   still anchored is the failure mode this behaviour exists to prevent.
5. **Each operation's control is reachable and does what it says** — reword, add, hide, reorder,
   check/uncheck, owner, due date. If mounted coverage for all seven is disproportionate, cover the
   ones that change what the reader believes (reword, add, hide, check) and say plainly which you
   left to the data-layer tests and why.

## Acceptance

- Each of the five items above is asserted by a mounted test, or explicitly justified as covered
  elsewhere with the reasoning recorded here.
- The app test count rises. A task that adds no test has not closed this one.
- No production behaviour changes. If an item cannot be proven without one, stop and report.
- Full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Changing the overlay model, its persistence, or its composition — T087 owns those and they are
proven. Restyling anything. Adding operations.

## Notes

Filed 2026-08-15 while reviewing T087's handoff, which also marked itself `done`; the board reserves
that for the planner and it was returned to `in-review`. T087 should close when this closes.
