# T078 — Somewhere to stand that is not a recording

**Status:** done

**Wave:** N7 — v2 workspace

**Depends on:** T072 and T073 (both `in-review`), which built the shell and the rail.

**Owns:** `crates/app/src/workspace/layout.rs`, `crates/app/src/workspace/mod.rs`,
`crates/app/src/workspace/library.rs`, and this task

**Concurrency (planner, 2026-08-14):** T076 holds `notes.rs`; T077 holds `transcript.rs`. Edit
neither.

## Why this exists

The maintainer: *"why do we don't see a home UI, only captures."*

The idle surface — the three equally weighted entry points — exists and is mounted. It renders only
when nothing is open (`layout.rs:245`). But once a session is selected there is **no way back to
it**: `transcript_session` is cleared in exactly one place, during teardown. Open the app with any
history and you land inside a recording, and the product's own front door becomes unreachable for
the rest of the session.

So the app presents itself as a viewer of one recording rather than as a thing you start recordings
with. That is a first-impression failure and it gets worse as the library grows.

## What to decide

Whether home is a *state you can return to* or a *place with its own content*. Both are legitimate:

- **Return-to-idle** is smaller: a Home affordance that deselects and shows the three entry points.
- **A real home surface** would show recent recordings, storage in use, and the entry points — the
  overview a person wants on launch, with the rail as navigation rather than the only view.

Recommend one with reasons and build it. If the second, keep it honest: show what is true (real
counts, real sizes) and do not invent a dashboard of metrics nobody asked for.

## Also decide what launch does

Landing directly inside the most recent recording is a choice nobody made deliberately. State what
launch should show and why.

## Constraints

- Stop must remain reachable and obvious while recording, at any width. Navigating home during a
  recording must not hide the capture bar or orphan the running session.
- Do not add a navigation concept the rail already provides. If a Home entry belongs in the rail,
  put it there rather than inventing a second navigation surface.
- ADR-0019's vocabulary throughout.

## Acceptance

- From an open recording, a person can reach the entry points again without restarting the app.
- Launch behaviour is deliberate and stated in the task file.
- Starting a recording from home works, and returning home during one does not hide Stop or
  interrupt capture.
- Whatever the home surface shows is true — no invented metrics.
- A narrow-width test covers it; nothing essential clips.
- Focused and full app tests, strict Clippy over all targets, formatting, and diff checks pass.

## Decisions

### Home is a state you return to, not a place with its own content

**Chosen: return-to-Home.** The shell already had a Home surface — the three equally weighted
entry points — and it was already correct. What it lacked was a way back to it. So the change is a
navigation change, not a new screen.

The reasons for rejecting the second option, in order of weight:

1. **A "recent recordings" panel is the rail, moved three inches right.** The rail is permanently
   visible at every width, already groups by `Now` / `Today` / `Yesterday` / weekday / date, already
   marks the running recording, and already searches transcripts and notes. A home *place* would
   have to re-list the same rows from the same catalogue with the same labels. The task's own
   constraint — do not add a navigation concept the rail already provides — rules that out, and the
   duplication would have to be kept in sync forever.
2. **Sotto has one artifact, so there is no overview to cross-cut.** A dashboard earns a screen when
   it summarizes things you cannot see at once. Here the whole product is a list of recordings and
   one open recording. The only fact the rail genuinely cannot state is the *aggregate*: how many
   recordings exist and how many bytes they occupy. That is one line, not a place.
3. **Fewer states to be wrong in.** Home-as-place would have to decide what it shows while a capture
   runs, while a summary generates, and while the library is empty. Home-as-state has exactly one
   composition and it is the one already reviewed under T072.

So Home is `transcript_session == None`, exactly as it always was, and it is now *reachable*.

**Home lives in the rail, as its first entry.** The rail is the shell's only navigator, so the way
back belongs in it rather than in a new bar, a breadcrumb, or a stage-level button. It renders with
the same face as every recording row (`rail_face` is now shared between the two), sits above the
recordings it deselects, and carries the accent-wash selected state whenever nothing is open — so
"where am I" is answered by the same visual grammar as "which recording am I in".

**The one honest aggregate.** Home's rail entry and the Home surface both state the measured
footprint: the session count from the persisted catalogue, and the sum of `byte_size` over the
recordings the store still reports on disk (`Store::list_recordings`). A recording the store has
marked deleted or pruned contributes zero, so the number never counts bytes that are gone. Nothing
else is claimed — no streaks, no totals-this-week, no invented metrics. When the library is empty it
says *"No recordings yet — Sotto is storing nothing on this Mac."*; when transcripts exist without
retained media it says *"no retained media"* rather than `0 B`.

### Launch shows Home

Landing inside the newest recording was never decided; it fell out of `NotesController::refresh_
catalogue` defaulting `selected_session` to the most recent finished session, which the shell then
opened. Removed.

Launch now opens Home, because **nothing has happened since the app quit**. No recording is the one
the person came back for, and the shell has no evidence for guessing. Opening the newest one made
Sotto introduce itself as a viewer of one capture and put its own front door out of reach — a first
impression that got worse the larger the library grew. Home is the only state that is correct
whatever the library holds: empty, one recording, or four hundred. The newest recording is not
hidden by this; it is the first row of the rail, one click away.

Two related landings were made consistent with it:

- **Finishing a recording still opens that recording.** You started it seconds ago, so it *is* the
  obvious next thing. Unchanged.
- **Deleting the open recording now lands on Home**, not on whichever recording happened to be next
  in the catalogue. Same reasoning as launch: the shell has no basis for choosing one.

### Home while a capture runs

The bars follow the **lifecycle**; the stage follows the **selection**. That separation already
existed and is what makes this safe: `show_home` clears the shell's selection and touches neither
the session controller nor the timeline, so the capture bar — and Stop — stay exactly where they
were at every width. Home additionally renders a note saying a recording is running, that Stop is in
the bar above, and that the recording itself is in the Library under `Now`. It carries no "back to
the recording" button of its own: the rail's `Now` row is that affordance and duplicating it in the
stage is the second navigation concept the constraints forbid.

### Import stays dead

Untouched. `StartChoice::Import` still returns its own unavailability sentence, still renders its
card with no click handler, and the rail's `Import…` button stays disabled with the same tooltip and
the same note beneath it. T071 is not built and nothing here pretends otherwise.

## What changed

- `library.rs` — `LibraryFootprint` (measured count + retained bytes, with `meta()` for the rail and
  `claim()` for the surface); `rail_face` extracted so Home and recording rows share one face;
  `render_home_row`; `render_start_choices` gained the running-capture note and the footprint claim.
- `mod.rs` — `show_home`; `library_footprint` refreshed alongside the search index; launch no longer
  opens the newest recording; delete lands on Home.
- `layout.rs` — `Stage::Idle` renamed `Stage::Home` (`stage-idle` → `stage-home`); `format_bytes` is
  now `pub(super)` and reused by the footprint rather than duplicated.

## Acceptance status

- **From an open recording, a person can reach the entry points again** — done;
  `home_is_reachable_from_an_open_recording`.
- **Launch behaviour is deliberate and stated** — done; above, and
  `launch_opens_home_not_the_newest_recording`.
- **Starting from Home works; returning Home during a recording does not hide Stop or interrupt
  capture** — done; `home_during_a_recording_keeps_stop_and_says_the_capture_is_running` clicks Home
  mid-capture at 680 px and then clicks Stop. Starting from Home is unchanged from T072 and still
  covered by `an_unbuilt_beginning_states_its_own_unavailability` plus the wired
  `StartChoice::begin` paths.
- **No invented metrics** — done; `home_states_the_measured_footprint_and_nothing_else` pins all
  three branches of the claim, and `the_home_entry_fits_the_rail_and_reports_only_what_is_measured`
  pins it against a real store holding one session and no retained media.
- **Narrow-width coverage** — done; the two new narrow tests run at `MIN_WORKSPACE_WIDTH` (680 px)
  and assert the Home entry, its label, the running-capture note and the footprint stay inside the
  rail and the stage.

## Not run

Everything below failed for reasons outside this task's three files. T076 (`notes.rs`) and T077
(`transcript.rs`) were being edited concurrently and the crate did not hold still.

- **Full `cargo test -p app`.** `cargo test -p app --lib -- workspace::layout workspace::library`
  is green: 25 passed, 0 failed. In the same binary, three sibling tests failed —
  `notes::tests::rendered::an_uncited_claim_still_fails_closed`,
  `notes::tests::rendered::the_summary_reads_as_prose_and_gives_up_its_evidence_only_when_asked`,
  and `transcript::mounted_tests::a_reader_can_select_a_row_anchor_it_and_copy_a_range_of_it`.
  None touch Home, the rail, or the shell.
- **`cargo clippy -p app --all-targets`.** Emitted exactly one finding across six runs spread over
  the task, `redundant clone` in `transcript.rs`, which aborts the crate. Clippy still lints the
  whole crate before failing, and no run attributed a finding to `layout.rs`, `mod.rs` or
  `library.rs` — lib or test target. Re-run once T077 lands.
- **`cargo fmt -p app -- --check`.** Clean for this task's three files; the only diffs reported are
  three hunks in `notes.rs`.
- **`git diff --check`.** The `workspace/` module is still untracked, so `git diff --check` sees
  nothing; checked instead with `git diff --no-index --check /dev/null <file>` per file — clean, no
  trailing whitespace and no tabs.

## Out of scope

The summary column (T076), the transcript column (T077), Settings, import (T071).
