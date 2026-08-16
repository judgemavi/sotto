# T083 — A real toolbar, and a sidebar that gets out of the way

**Status:** done

**Wave:** N7 — v2 workspace

**Depends on:** T082 (`in-review`), which made the window titlebar transparent and reserved a strip
for the traffic lights.

**Owns:** `crates/app/src/workspace/layout.rs`, `crates/app/src/workspace/mod.rs`,
`crates/app/src/workspace/library.rs`, `crates/app/src/workspace/ask.rs`, and this task

## Why now

T082 removed the in-window title row and moved the app name, Settings and appearance to the menu
bar, then made the titlebar transparent so the theme reaches the top of the window. That left a
38px strip reserved for the traffic lights and holding **nothing**. This task gives it a job.

Three changes, all the maintainer's, and they fit together:

1. **The library rail collapses**, the way VS Code's sidebar does.
2. **Search moves out of the rail** into the toolbar.
3. **Ask becomes a control in the toolbar** rather than a permanently reserved 42px rail on the
   right.

## The shape

The reserved strip becomes the app's toolbar: traffic lights at the left, then a sidebar toggle,
then search, then Ask. It is one row, it already exists, and it costs no additional height.

- **Sidebar toggle** — collapses and restores the library rail. The rail is 248px (210px under
  980px), which is a quarter of a 1000px window; a person reading a transcript should be able to
  reclaim it. Remember the choice across launches, as the Ask panel already does through
  `workspace-state.json`.
- **Search** — currently in the rail, so it disappears when the rail does. In the toolbar it is
  always reachable. Keep its behaviour: it filters recordings by title *and* by indexed body
  (transcript, annotations, generated notes), which is more than "filter the list" and the
  placeholder should keep saying so.
- **Ask** — the right side currently reserves 42px for a vertical rail even when Ask is closed. Put
  the toggle in the toolbar and let the panel take no width when closed. Whether the panel itself
  stays on the right or moves is your call; the maintainer asked for the *control* to move, and the
  panel opening where it always has is less disruptive than moving both.

## Constraints

- **Stop stays reachable.** It lives on the capture bar, which is not this row, and none of this
  may push it off-screen or under the traffic lights at any width.
- **The traffic lights own their corner.** Nothing may be placed under them; the inset is
  `TRAFFIC_LIGHT_INSET` and the strip is `TITLE_STRIP_HEIGHT`.
- **Collapsed must be obvious and reversible.** A person who collapses the rail and forgets must be
  able to see how to bring it back — an icon that stays put, not a hover-to-reveal edge.
- Shrink roles apply here as everywhere: the sidebar toggle and Ask are essential, search
  ellipsizes, and nothing clips at the stated minimum width.
- ADR-0019 vocabulary throughout.

## Acceptance

- The rail collapses and restores from a toolbar control, and the choice survives a relaunch.
- Search sits in the toolbar, is reachable with the rail collapsed, and still matches titles and
  indexed bodies.
- Ask is toggled from the toolbar and reserves no width when closed.
- Stop remains fully visible and clickable at the minimum width in every combination of
  collapsed/expanded and Ask open/closed.
- Nothing is drawn under the traffic lights.
- A narrow-width test covers the collapsed and expanded states.
- Focused and full app tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Moving the Ask panel to the left, a VS Code-style activity bar with multiple views, new settings,
and the notes or transcript columns.

## Notes

### What was built

The reserved strip is now `render_toolbar` in `layout.rs`, one `ControlRow` at
`TITLE_STRIP_HEIGHT`, keeping the `title-strip` debug selector so T082's "the state bar sits
immediately under the strip" assertion still means what it said. Left padding is
`TOOLBAR_LEADING_INSET = 84px`: the traffic lights start at `TRAFFIC_LIGHT_INSET` (13) and macOS
lays its three buttons out 14px wide at 20px pitch, so the last one ends at 67 — and the normative
mock reserves 84 on the same row, which is the larger figure and the one that has been looked at.

Three controls, left to right:

- **`toolbar-library-toggle`** — `Essential`. Ghost button labelled `Library`, `selected` while the
  rail shows, tooltip flipping between `Show the Library` / `Hide the Library`. The *label* does
  not flip, so the control keeps its width and search does not jump beneath the cursor.
- **`toolbar-search`** — `Ellipsizing`, capped at 360px. The same `library_filter` `InputState` the
  rail used to hold, so the behaviour is unchanged: `library::search_index` still folds transcript,
  annotations and generated notes into the haystack and `group_rail` still matches title *or* body.
  The placeholder still says `Search recordings and notes`.
- **`toolbar-ask-toggle`** — `Essential`. Ghost button labelled `Ask`, `selected` while open.

Nothing in the row is `Expendable`: a chrome control that collapses out is a control a person
cannot get back to, which is the opposite of what the sidebar toggle is for.

**Collapse.** `MeetingWorkspace::library_collapsed`, persisted as `library_collapsed` in
`workspace-state.json` beside `ask_open` and `theme`, `#[serde(default)]` so an existing state file
still parses and defaults to *shown*. Collapsed, the rail column is not rendered at all, so the
stage takes the full width back rather than looking at a hidden 248px.

**Searching while collapsed restores the rail.** The results of a search are rail rows; filtering a
list nobody can see would have made the toolbar's search a dead control in exactly the state it was
moved to survive. So a non-empty query while collapsed expands the rail and records that, the way
the toggle would have. Clearing the query deliberately does not re-collapse: one unasked-for move
is enough.

**Ask.** The toggle moved; the panel did not. It still opens on the right, at `ASK_PANEL_WIDTH`
(344px, unchanged), and closed it renders nothing — the old 42px vertical rail existed only to
carry the control that is now in the toolbar. Moving the panel as well would have moved two things
for a change the maintainer asked for as one, and the left edge is the rail's.

### Deviations

- **No `panel-left` icon.** The sidebar toggle is worded (`Library`) rather than iconified: the
  Lucide asset is not vendored, `crates/app/assets/icons/` and `icons.rs` are not in this task's
  `Owns`, and hand-authoring an SVG was explicitly ruled out. Words are also the safer default here
  under T082's own reasoning — GPUI 0.2.2 publishes no accessibility tree, so an icon-only
  control's name reaches a person through a tooltip and nothing else. **If the icon is wanted:
  vendor Lucide `panel-left.svg` (ISC) and add it to `ASSETS`.**
- `Input::cleanable(true)` was tried and removed. Its clear button draws `IconName::CircleX` →
  `icons/circle-x.svg`, which Sotto does not vendor; `Assets::load` answers `Ok(None)`, so it would
  render as an invisible but clickable control — the "picture the app does not own" defect T082
  ended. Vendoring `circle-x.svg` would enable it.
- `ask.rs` copy moved to ADR-0019 vocabulary ("this recording", "Every retained recording", "Select
  a stopped recording…", placeholder "Ask about this recording"). The task's constraints ask for
  ADR-0019 vocabulary throughout and `ask.rs` is owned here. Pinned by `scope_line`, a pure helper
  with its own test.
- Removed a stray `eprintln!("T082-PROBE: toggle_settings entered…")` that shipped in `mod.rs` on
  the settings path. Debug residue from T082, in a file this task owns.
- The rail's `library-search` element is gone, and `library::render` lost its `input` argument —
  which dropped it to seven parameters, so its `#[expect(clippy::too_many_arguments)]` had to go
  with it or the expectation would have gone stale and failed the build.

### Evidence

`cargo test -p app` — 212 lib tests pass, plus strict Clippy over `--workspace --all-targets
--all-features`, `cargo fmt --all --check`, and `git diff --check`. New or changed tests:

- `layout::the_toolbar_fills_the_strip_without_reaching_under_the_traffic_lights` — every toolbar
  control has real width, starts at or after `TOOLBAR_LEADING_INSET`, and fits inside the strip.
- `layout::the_library_collapses_from_the_toolbar_and_comes_back` — the stage gains the rail's
  whole width, the toggle's rectangle is *identical* before and after (the "stays put" requirement),
  and a second click restores the exact prior layout.
- `layout::a_collapsed_library_is_still_collapsed_after_a_relaunch` — the choice reaches
  `workspace-state.json`, and a second shell mounted over the same store opens collapsed with the
  toggle already on screen.
- `layout::toolbar_search_reaches_a_collapsed_library_and_still_matches_bodies` — no `library-search`
  in the rail; searching a phrase that appears only in transcript rows expands the rail, and the
  index still contains it.
- `layout::ask_reserves_no_width_until_the_toolbar_opens_it` — closed, the stage reaches the
  window's right edge; open, it gives up exactly `ASK_PANEL_WIDTH`; closing returns the exact
  layout, and both states are recorded.
- `layout::stop_survives_every_sidebar_and_ask_combination_at_the_minimum_width` — all four
  combinations at 680px: Stop whole and inside the capture bar, toolbar controls clear of the
  traffic lights and inside the window, stage width non-zero; then Stop is clicked in the tightest
  combination and the lifecycle really moves to `Stopping`.
- `layout::ask_theme_and_sidebar_state_roundtrip` — round trip plus a pre-T083 state file.
- `ask::the_scope_line_speaks_about_recordings`.

### Not verified by automation — the maintainer runs these

1. **Traffic lights.** Launch and look at the top-left. Pass: three system buttons, then air, then
   the `Library` button. Fail: any overlap, or a control starting left of the zoom button.
2. **Collapse.** Click `Library`. Pass: the rail disappears, the transcript widens into it, and the
   `Library` button stays exactly where it was, now looking unselected. Fail: the button moves, or
   the rail leaves a gap.
3. **Relaunch collapsed.** Quit with the rail collapsed and reopen. Pass: it opens collapsed with
   `Library` visible in the toolbar. Fail: the rail returns, or the toolbar is empty.
4. **Search while collapsed.** Collapse, then type a word you know is only *spoken* in a recording.
   Pass: the rail comes back and the matching recording is listed. Fail: nothing happens.
5. **Ask.** Click `Ask`. Pass: the panel opens on the right, the button looks selected; click again
   and the panel is gone with no leftover strip at the right edge. Fail: a thin empty column stays.
6. **Stop at the minimum width.** Start a recording, drag the window to its narrowest, and try all
   four combinations of collapsed/expanded × Ask open/closed. Pass: Stop is fully visible and stops
   the recording in every one. Fail: Stop is clipped, or the window will not narrow that far.
7. **Both appearances.** Repeat 1, 2 and 5 in Light and Dark. Pass: the toolbar ground matches the
   window and the selected state of both toggles is legible in both. Fail: either toggle's selected
   and unselected states are indistinguishable.
8. **Words, not a picture.** Confirm the worded `Library` toggle is acceptable, or ask for Lucide
   `panel-left.svg` to be vendored (see Deviations).
