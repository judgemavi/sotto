# T077 — The transcript must be selectable and copyable

**Status:** in-review

**Wave:** N7 — v2 workspace

**Depends on:** T074 (`in-review`), which built this column.

**Owns:** `crates/app/src/workspace/transcript.rs` and this task

**Concurrency (planner, 2026-08-14):** T076 holds `notes.rs`; T078 holds `layout.rs`, `mod.rs` and
`library.rs`. Edit neither.

## Why this exists

The maintainer, reading a real transcript: *"why am I not able to select and copy any of the text?"*

Nothing in the workspace is selectable. A search for `selectable`, `TextView` or `InteractiveText`
across `crates/app/src/workspace/` returns nothing. Every row is a plain label.

This is not a small polish item. Sotto's product claim is that it produces a record you can check
and use — and a record you cannot quote is a record you can only look at. Paste into a message, a
ticket, a document: that is what a person does with a transcript within a minute of reading one.

`gpui_component::text::TextView` exposes `selectable(bool)` at `text_view.rs:497`. The capability
has been available the whole time and was never wired.

## Plan

1. Make transcript text selectable, including across rows if the primitive allows it. If selection
   cannot span rows, say so plainly rather than leaving the reader to discover it.
2. Keep every existing row gesture working: clicking a row still sets the note anchor, and a
   citation still scrolls, redirects through the supersede chain, and flashes. Selection must not
   swallow the click that sets an anchor — if the two conflict, resolve it deliberately and record
   which gesture wins and why.
3. Support copying a useful unit, not just a fragment. A person selecting several rows expects the
   text; decide whether timecode and source attribution travel with it and justify the choice.
4. Preserve the folded non-speech asides from T074 — a fold must not become a hole in a copied
   range without indication.
5. Check the live path as well as the completed one. A provisional row that is still being revised
   should not hand the reader text that is about to change without signalling it.

## Acceptance

- Transcript text can be selected and copied, in both a live and a completed session.
- Row selection still sets the note anchor, and citation reveal still scrolls and flashes.
- The copied result is usable in another application, with the timecode/attribution decision stated.
- Folded non-speech ranges are represented honestly in a copied selection.
- Focused and full app tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

The summary column (T076), the shell and library (T078), exporting a whole transcript to a file,
and the notes taxonomy.

## Notes

### What was built

Every line of transcript text is now a `gpui_component::text::TextView` with `selectable(true)`,
wrapped in a `SelectableLine` `RenderOnce` so the primitive can obtain the `&mut Window` that
`transcript::render`'s signature — owned by T078 — does not carry. Three surfaces are selectable:
the spoken text of a committed row, the folded non-speech aside, and the live provisional line.

Text reaches the view as **escaped HTML**, not Markdown. Markdown would have eaten a speaker's
`*emphasis*` and turned `[brackets](parens)` into a link on the way to the clipboard; four HTML
substitutions round-trip exactly through html5ever's entity decoding. A test drags across a row
containing `"ship it" & nothing else` and asserts the clipboard holds it verbatim.

Two explicit multi-row units, because selection cannot span rows:

- **`Copy` in the column head** takes the whole presented transcript.
- **Shift-click a row** copies the range from the current note anchor through that row.

### Selection does not span rows — stated, not left to be discovered

`TextView` owns one selection per instance, and one instance per row is what keeps a row clickable,
flashable and individually anchorable. Making the column one `TextView` would have bought
cross-row selection at the cost of per-row anchoring, the citation flash, pinned annotations and
`scroll_to_reveal_item`. The module doc says the boundary out loud, and
`a_drag_past_the_row_boundary_copies_only_the_row_it_started_in` holds the app to it: a drag from
row 1 into row 2 copies row 1 only.

### Selection versus the note anchor: the anchor wins the click

GPUI fires `on_click` on mouse-up regardless of how far the pointer travelled, and `TextView`
installs its selection handlers on the window without stopping propagation. So a drag inside a row
both selects text and anchors that row. That is deliberate and additive: the two gestures address
the same row, and anchoring appends nothing to the record. Shift is the one modifier that means
something else, and it never moves the anchor. Tested both ways.

### What a copied range contains, and why

`[mm:ss] <source>(<qualifiers>): <text>`, one line per presented row. Media time and source travel
with the text because a transcript excerpt in a ticket that names neither cannot be checked against
the recording, and checking the record is the product claim. Qualifiers carry prosody and
`Not finalized before capture stopped`, so a row's caveats cannot be separated from its words.

Folds are honest in three ways: a folded follower contributes no line because its leader already
states the count and the span; the leader's copied line is the exact string on screen; and a range
is re-classified **over the range**, so a selection that opens inside a folded stretch makes its
first non-speech row a leader with the range's own count rather than copying as nothing.

### The live path

A provisional row is selectable, but the warning is *inside* the selectable line
(`still being revised · …`) rather than beside it — the clipboard has no room for the surrounding
strip, and this is the one surface whose text changes under the reader. A mounted test drives the
real ingress seam and asserts the provisional line renders with real bounds.

### Defect found and fixed while wiring this

Every committed row used the element id `"transcript-row"`. GPUI keys per-element click state by
the element-id path and `gpui::list` does not scope its items, so all rows shared one
`pending_mouse_down`: the first row's mouse-up listener runs first in the capture phase, sees the
press was not over itself, and clears it before the row the reader actually clicked is reached.
**Only the topmost transcript row could be made the note anchor.** The id now names the row, and
`a_row_below_the_first_can_still_be_made_the_note_anchor` clicks every row in reverse order.

### Verification

31 focused tests in `workspace::transcript` pass, including five that mount the real shell over a
real store and drive it with real pointer and keyboard input. `cargo clippy --workspace
--all-targets --all-features -- -D warnings` is clean. `rustfmt --edition 2024` is clean on this
file; `cargo fmt --check` over the crate reports diffs only in `notes.rs`, which T076 holds.
`cargo test -p app` is 187 passed / 1 failed, the failure being T076's in-flight
`notes::tests::rendered::evidence_can_be_revealed_from_the_keyboard`. `git diff --check` is clean
for this task's paths.

**NOT RUN:** no signed-app or on-device check. Nobody has dragged across a real transcript in the
built app, pasted into a real ticket, or judged whether the copied format reads well outside Sotto.
Nor has anyone measured the per-row `TextView` cost on a long live transcript — parsing is
synchronous on a row's first layout and re-parses only when its text changes, but that is reasoning,
not a measurement. Both belong to T035's manual gate.

### Maintainer review — 2026-08-16

Manual pass in the built app confirmed the four behaviours that carry this task: clicking a row
anchors it, dragging selects and copies its text, `Copy` in the head takes the whole transcript,
and a drag stops at the row it started in. The last is the documented limit, met without surprise.

One gap found, and fixed here rather than filed: **a shift-clicked range had no visual extent.**
The gesture has three effects — the clipboard, the Ask scope T091 added, and the status line — and
two of them were invisible. Only the anchor was washed, so a reader could not see how far back the
range they had just copied and scoped a question to actually reached. `AskSelection.event_ids`
already held exactly that range; nothing drew it.

Every row of the range now carries the wash, and the anchor keeps a left rule so the range's origin
— the row a typed note attaches to — stays distinguishable from the rows it reached. `row_mark`
decides between plain, in-range, anchor and flashed, and is unit-tested; a flashed row outranks a
standing mark, because it answers a question the reader asked a moment ago.

**The rule takes its two pixels out of the row's own padding**, and that is load-bearing rather than
tidiness. A left border that widens the box reflows the row's text under the reader mid-gesture: a
drag that ends by anchoring its row moved the words 2px right on mouse-up and lost the selection the
drag had just made. `a_reader_can_select_a_row_anchor_it_and_copy_a_range_of_it` caught it, and
fails without the compensation. The same reflow existed on the citation flash and is fixed with it.

Residual, recorded rather than claimed: the wash and the rule are asserted through `row_mark`, not
through a rendered pixel. `debug_bounds` cannot report a background colour, so *"the range is
marked"* is proven as a decision and the token application is a one-line binding either side of it.

### Review follow-up — 2026-08-14

Review found that the column-level `Copy` action read only committed pacer rows even while the
provisional strip was visibly rendering `unstable` rows. It now appends the current live unstable
projection to the copied unit, preserving the provisional warning, and the mounted live test
asserts both the warning and hypothesis text reach the real clipboard. The direct selection path
is unchanged. Signed-app drag/paste judgment and long-live `TextView` measurement remain NOT RUN.
