# T074 — The transcript column, attributed by source

**Status:** done

**Wave:** N7 — v2 workspace

**Depends on:** ADR-0019 and `docs/design/workspace-v2-mock.html`, which is normative.

**Owns:** `crates/app/src/workspace/transcript.rs` and this task

**Concurrency (planner, 2026-08-14):** T072 holds `layout.rs`, `mod.rs` and `tokens.rs`; T073 holds
`library.rs`; T075 holds `notes.rs`; T069 holds `settings/mod.rs` and `ask.rs`. Edit none of them.

## What the mock changes

- Rows are attributed **captured audio** and **your microphone**, not "Meeting audio" and "You".
  A small legend in the column head explains the two dots, and collapses when space is short.
- The column head carries the label and a row count.
- Citation chips in the summary **flip to this tab and flash the cited row**. That gesture is the
  product's central claim — every generated line is checkable — so the receiving half lives here.

## What must survive

- The transcript is append-only. Typed notes are appended at the moment they were typed and never
  rewrite captured facts.
- Selecting a row sets the note anchor.
- A row's screen frame is fetched only on demand. Nothing decodes by default.

## The silence problem

The `[ Silence ]` and `[ Pause ]` suppression from T062 landed, but whisper still emits its own
non-speech annotations — a real run produced `(soft music)` and `(people chattering)` rows attributed
to the microphone while the user was simply not speaking. In a session where one channel is quiet
throughout, these accumulate and crowd out the content.

Decide how they are presented and say why. They are not false — the model did hear something — so
deleting them silently is not obviously right either. This is a judgement call the task owner makes
and records.

## Plan

1. Re-attribute rows and add the collapsing legend.
2. Implement the citation target: an incoming citation selects this tab, scrolls the row into view,
   and flashes it.
3. Keep the anchor gesture and the on-demand frame contract intact.
4. Resolve the non-speech annotation noise.
5. Apply the mock's shrink roles to the column head; use T060's primitive rather than per-site widths.

## Acceptance

- Rows name their source in ADR-0019's vocabulary, and the legend collapses before content does.
- A citation from the summary lands on the exact cited row, visibly.
- Typed notes still append without rewriting captured facts, asserted by test.
- No decode or OCR occurs on the default path.
- Non-speech rows no longer crowd a quiet channel, with the chosen treatment recorded.
- Focused and full app tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

The shell (T072), the rail (T073), the summary itself (T075), the notes taxonomy (T070), Settings
and Ask (T069).

## Notes

All work landed in `crates/app/src/workspace/transcript.rs`. No sibling-owned file was touched.

### Closure review (2026-08-15)

**Not closable yet:** the legend gets narrower, but does not actually collapse before content.
Production builds the head with `ControlRow::new()`, whose contract never drops expendable
children; only `ControlRow::for_width(...)` applies the shared collapse threshold. The mounted test
also requires the narrow legend to remain present and proves only that its measured width shrinks.
This misses the task's explicit acceptance and the normative mock's `.collapses` behaviour.

The correct repair must wait for the current owners of `transcript.rs` (T077) and `layout.rs`
(T078): pass the available width from `MeetingWorkspace::render` through the transcript renderer,
construct the head with `ControlRow::for_width(available)`, and assert that the legend has positive
bounds when wide but is absent at or below `ControlRow::COLLAPSE_WIDTH`, while the label remains.
Do not substitute a transcript-local breakpoint.

Current automated evidence is otherwise sound. The focused transcript suite passes 33 tests and
strict app Clippy is clean. Later T077/T078 work now covers row anchoring and routes summary chips
through `open_citation` to `reveal_citation`, so the older NOT RUN statements below about having no
caller and no automated anchor assertion are historical. The mounted citation test establishes the
tab/focus landing, but it still does not assert the visible flash state or its 1.4-second decay.

### Attribution and the collapsing legend

`source_label` now returns ADR-0019's vocabulary — `captured audio` for `Source::System`,
`your microphone` for `Source::Mic` — and every surface in the column uses it: committed rows, the
provisional strip, the legend, and the non-speech aside. Each label is paired with the mock's dot
(`--accent` for captured audio, `--warn` for the microphone) so the legend and the rows teach the
same thing.

The head is built with T060's `ControlRow` rather than per-site widths: the label and the row count
are `Essential`, an empty `Ellipsizing` child plays the mock's `.spacer`, the legend is
`Expendable`, and the live `Follow live` button is `Essential`. That is the mock's shrink order —
`.keep` for the label, `.collapses` for the legend — expressed in the primitive we already own.
`layout_tests::the_legend_gives_up_its_width_before_the_column_label_does` mounts a real window at
900 px and again at 200 px and asserts from measured bounds that the legend loses width while the
label's is unchanged.

Row shape follows the mock: a right-aligned 46 px monospace timecode, then the dot-and-source chip
inline ahead of the wrapped text, instead of the old fixed 92 px speaker column that could not have
held "your microphone".

### The judgement call: Whisper's own non-speech annotations

**Decision: demote and fold, disclosing the count — never delete silently, never one row each.**

`(soft music)` and `(people chattering)` are true. Whisper did hear something, and a transcript that
throws them away is claiming a silence that did not happen. But one row per annotation is also
false in effect: on a session where one channel is quiet throughout, they outnumber the speech on
the other channel and the record becomes unreadable. Both failure modes are honesty failures, so
the treatment has to keep the fact and drop the volume.

What ships:

1. **Recognition is conservative.** `is_non_speech_annotation` matches only when the *whole*
   utterance is one bracketed annotation — `(soft music)`, `[people chattering]`, `*coughs*`, `♪`.
   `(laughs) yeah, agreed` contains speech and stays an ordinary row. A wrong classification here
   would delete real content, so the rule fails towards keeping the row.
2. **They never enter the provisional strip.** That strip is a live monitor of what is being said;
   a non-speech hypothesis flickering there is pure noise, in live and completed modes alike.
3. **Each unbroken stretch on one source folds to a single quiet aside.** A stretch is broken by
   *speech on the same source*, not by speech on the other — which is exactly the case being
   fixed, where a quiet mic's annotations are interleaved in time with the other channel's
   conversation. The aside is italic, faint, META-sized, carries no source chip, and reads
   `(soft music) · 38 non-speech moments on your microphone, through 41:02`. The count and the span
   are the point: nothing disappears without saying how much of it there was.
4. **Nothing is removed from the projection.** Folding is a render-time role assignment
   (`classify_rows`), not a filter. Folded rows keep their list slot, so `TranscriptPacer`
   indices, `citation_index`, annotation anchors and `scroll_to_reveal_item` all still address
   exactly what they always addressed, and the timeline itself is of course untouched.
5. **A citation into a folded stretch lands on the line that renders it.** `presented_event` walks
   a folded row back to its leader before `reveal_citation` scrolls and flashes, so the gesture
   never lands on a blank slot.

Rejected alternatives: deleting them (claims a silence that did not occur); keeping one row each
but styling them down (a quiet channel still produces more rows than the speaking one); and moving
them to a footer tally (loses *when*, and makes a cited moment unreachable).

Side effect worth recording: the strip's silence gate now also drops `[ Silence ]`/`[ Pause ]`
during live capture, not only on review. T062 suppressed those as rows; they had still been able to
flicker through the live strip.

### The citation target

`MeetingWorkspace::reveal_citation(event_id, cx) -> bool` in `transcript.rs` is the whole receiving
half and the only call T075 needs. It selects `StageTab::Transcript`, resolves the citation through
the append-only supersede chain, redirects a folded non-speech moment to the line that renders it,
scrolls that row into view, sets it as the note anchor, and flashes it for 1.4 s. It returns
`false` — with the workspace message already set — when the evidence is not in the transcript on
screen, so the caller must not also claim a successful jump.

**T075 must rewire from `open_citation` to `reveal_citation`.** The planner's mid-flight note had
T075's chips calling `MeetingWorkspace::open_citation`, which lands on the row but neither switches
the tab nor flashes. Both of those are what makes the gesture legible, and both live in
`reveal_citation`. It is a drop-in replacement — same argument, same failure message — and it lives
in this task's file rather than in `mod.rs`, which this task does not own. The tab switch is done by
calling T072's existing `select_stage_tab(StageTab::Transcript, cx)`; nothing in `mod.rs` or
`layout.rs` was edited to achieve it.

The flash is transient view state held in a GPUI `Global` (`CitationFlash`) with a generation
counter, cleared by its own timer. It lives there rather than on `MeetingWorkspace` because that
struct belongs to T072 in this wave; there is exactly one transcript column per app. If a later
task consolidates workspace view state, this is the obvious thing to move onto the struct.

`MeetingWorkspace::open_citation` in `mod.rs` (T069's Ask path) still does the older
scroll-and-select without the tab switch, the folded-row redirect, or the flash. Routing it through
`reveal_citation` is a one-line follow-up for whoever next owns `mod.rs`.

### What was asserted, not assumed

- `typed_notes_append_beside_captured_rows_without_rewriting_them` — appending a `UserAnnotation`
  leaves every earlier event byte-identical, adds exactly one event, produces no transcript row,
  and hangs the note off the row it anchors to.
- `the_default_transcript_path_reads_no_screen_frame` — a timeline carrying a `ScreenSnapshot`
  projects no row, no annotation, and no consultation. The frame reference is never dereferenced on
  the default path; `screen_consultation_disclosure(&[])` is `None`, so a run that inspected nothing
  claims nothing.
- `a_quiet_channel_folds_to_one_disclosed_aside_while_the_other_keeps_speaking`,
  `a_folded_stretch_discloses_what_it_stands_for`,
  `whole_non_speech_annotations_are_recognised_and_partial_ones_are_not`,
  `a_citation_into_a_folded_stretch_lands_on_the_line_that_renders_it`,
  `rows_name_their_source_in_adr_0019_vocabulary`.

### Maintainer observation — 2026-08-15

**The citation flash is confirmed by observation.** Following a summary citation chip lands on the
cited transcript row and that row is visibly highlighted, screenshotted at the 00:23 row of a real
recording. This is the evidence the acceptance item "a citation from the summary lands on the exact
cited row, *visibly*" was missing — the landing was already driven end to end by T077/T078's rewire
through `open_citation` to `reveal_citation`, but the visible state had no test because the flash is
a spawned timer.

Recorded honestly: the highlight was observed, the 1.4-second decay was not separately timed. The
acceptance asks for visible, and visible is what was confirmed.

**The legend blocker is resolved** by T095, which threaded the transcript column's real width to the
head and collapses the legend at the shared threshold, asserting absence rather than a smaller
width. T095 also found and fixed a second defect this task had not caught: with the legend gone, the
essential Copy and Follow controls still overflowed the narrow column.

The remaining item — whether one folded aside per quiet stretch reads correctly over a long
recording with a silent channel — stays assigned to T035's gate, where it was already deferred.

### NOT RUN

- **The citation gesture end to end.** `reveal_citation` has no caller until T075 rewires its
  chips, so no test drives chip → tab → scroll → flash as one motion. The flash decay itself (a
  spawned timer) is likewise unexercised.
- **Row selection setting the note anchor is not asserted by an automated test.** The handler is
  unchanged and attached to both the spoken row and the non-speech aside, but exercising
  `select_annotation_anchor` needs a mounted `MeetingWorkspace` with a database, and that harness
  belongs to `layout.rs`.
- **No visual acceptance on a real recording.** The non-speech treatment was designed against the
  reported `(soft music)` / `(people chattering)` run, not re-observed against it. Whether one
  folded aside per quiet stretch reads correctly over a 40-minute session with a silent channel is
  a maintainer observation, and it belongs with T035's gate.

### Verification

`cargo test -p app` — 161 passed, 0 failed, including this task's 19.
`cargo clippy -p app --all-targets --all-features -- -D warnings` — clean.
`rustfmt --check` and `git diff --check` over `crates/app/src/workspace/transcript.rs` — clean.

Mid-flight note for the reviewer: while T072's `layout.rs` was being rewritten,
`a_stopped_session_opens_on_notes_with_transcript_one_tab_away` failed on
`transcript_list.logical_scroll_top()` across a tab round-trip. It passes now. It is worth knowing
that this assertion's numbers move with row height, and this task changed row metrics to the mock's
(`.t-row { padding: 5px 16px 5px 10px }`, one text line instead of a fixed speaker column), so a
future change to either side can shake it loose again.
