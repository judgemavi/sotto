# T075 — The summary column: your notes, then what the recording supports

**Status:** in-review

**Wave:** N7 — v2 workspace

**Depends on:** ADR-0019 and `docs/design/workspace-v2-mock.html`, which is normative. T070 changes
the notes *taxonomy*; this task renders whatever that taxonomy produces and must not assume seven
fixed sections.

**Owns:** `crates/app/src/workspace/notes.rs` and this task

**Concurrency (planner, 2026-08-14):** T072 holds `layout.rs`, `mod.rs` and `tokens.rs`; T073 holds
`library.rs`; T074 holds `transcript.rs`; T069 holds `settings/mod.rs` and `ask.rs`; T070 holds
`crates/insight/src/notes/**` and `crates/app/src/notes/**`. Edit none of them.

## What the mock changes

- **Your notes come first**, in their own block, headed "appended to the record — never rewritten".
  When empty it says what typing will do rather than reporting emptiness.
- **The summary is adaptive.** The mock demonstrates five sessions with five different section
  shapes. Sections exist because the content supports them; an empty section is **never rendered**.
  The meta line reads "N sections · every claim cited".
- **Before summarizing**, the pending state explains what will happen: reading the transcript and
  writing only the sections this recording supports, with a timecode on every claim.
- **Every claim carries a citation chip** that jumps to the transcript row. That is the product's
  central promise made operable, not decoration.
- The composer sits at the bottom, pinned, labelled with the moment it will attach to.

## The defect to fix here

The shipped notes column renders evidence buttons that overflow and clip mid-word — three "Open
transcript evidence" buttons with the third cut off. This is the fifth instance of the same defect.
T060's shrink-priority row primitive exists for it; the mock marks the roles. Use it.

## Plan

1. Rebuild the column: your-notes block, then summary area, then pinned composer.
2. Render sections from what the taxonomy returns. Do not hardcode a section list, and do not render
   a heading for an empty one.
3. Give every claim a citation chip that selects the transcript tab and flashes the cited row.
4. Build the pending and generating states the mock shows, including a failure that names its cause.
5. Show the model that produced the summary, and any downgraded controls T067 recorded — a run that
   lost a requested control must say so rather than presenting the result as unqualified.

## Acceptance

- A summary with three sections renders three; a summary with seven renders seven; neither renders
  an empty one.
- Every claim has a citation chip, and following it lands on the cited transcript row.
- No control in the column clips at the stated minimum width.
- Typed notes appear immediately, before any summary exists, and are never rewritten by one.
- A failed summary names its cause; a downgraded one says what was lost.
- Focused and full app tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

The notes taxonomy and prompts (T070), the shell (T072), the rail (T073), transcript rows (T074),
Settings and Ask (T069).

## Notes

### What was built

`crates/app/src/workspace/notes.rs` was rebuilt in the mock's order: your-notes block, summary area,
pinned composer.

- **`summary_sections` is the only place a section is named.** It takes whatever the taxonomy hands
  over and returns `Vec<SummarySection>`; every renderer below it walks that vector. Empty input
  vectors never become sections, so a heading with nothing under it cannot be drawn. When T070 lands
  a new taxonomy, this one function changes and the column does not.
- **The head meta line** reads "N sections · every claim cited" (singular for one), or names the
  state when there is nothing to count: `summary comes after you stop`, `not summarized yet`,
  `summarizing…`, `not summarized`, `no sections supported`.
- **`SummaryView::resolve` owns every state**: live, no recording, not summarized, generating,
  failed (names its cause in a warn band), ready, stale (warn band saying it predates the typed
  note, with the summary still readable beneath it). A `Ready` result whose taxonomy supported no
  section says so instead of reporting a count of zero.
- **Provenance is stated, not implied**: the model that produced the summary and whether it was
  written now or loaded from disk, plus the source footing (none selected / retrieved / unavailable).
- **Citation chips** are short moment labels that call `MeetingWorkspace::open_citation`.
- **Your notes** lead the column, headed "appended to the record — never rewritten", and when empty
  say what typing will do rather than reporting emptiness. Each typed note keeps Edit and a jump to
  its anchor. A visible `Add` button now sits beside the composer input; Return still works.

### The clipping defect

The shipped column's three "Open transcript evidence" buttons sat in a bare `flex` row with no
`min-w-0` and no wrapping, so the third was painted mid-word. Two changes, both the mock's own:

1. Every **fixed-arity** control row in the column now goes through T060's `ControlRow` with explicit
   roles — column head (`Notes` / meta / Summarize), your-notes head, each typed-note row, section
   headings, the composer, and each source and resource row. No width is bounded at a call site.
2. The **citation chip row wraps**. A chip list has no fixed arity, so no shrink priority can rescue
   it: with four citations, whichever child ranks last clips regardless of role. The chip is a short
   moment label and the row wraps, which is exactly what the mock does (its chips are inline in the
   claim text and reflow).

`the_summary_column_renders_its_sections_and_no_control_clips` builds the real render tree over a
persisted three-section summary at the 680 px minimum width, asserts head, Summarize, your-notes,
summary area, composer, Add and all four citation chips lie wholly inside the column, and clicks the
third chip to prove it lands on the row it cites.

### Deviations and open handoffs

- **Citation chips carry `#<event id>`, not a timecode.** `render`'s signature is frozen (its call
  site is in T072's `layout.rs`), and nothing in the arguments it receives carries transcript media
  time. `render_with_citation_times` takes a `CitationTimes` map and is the real body; `render`
  calls it with an empty map. T072 passing a `BTreeMap<EventId, Duration>` built from
  `frame.transcript` rows switches every chip and the composer's anchor label to `mm:ss` with no
  other change.
- **Downgraded controls are NOT surfaced.** `GroundedMeetingNotesReport.normalizations` (T067) is
  dropped when `NotesController` builds `NotesState`, and `NotesState::Ready`/`Stale` carry no field
  for it; both live in `crates/app/src/notes/controller.rs`, which T070 owns. The column cannot say
  what a run lost until that state carries it. `Ask` already renders the same information from
  `AskResult.normalizations`, so the shape is settled — `NotesState::Ready`/`Stale` need
  `normalizations: Vec<ObservedRequestNormalization>`.
- **`.collapses` has no direct GPUI equivalent under this column.** `ControlRole::Expendable` drops
  a child below `ControlRow::COLLAPSE_WIDTH`, but a column child cannot see its own width, so
  `ControlRow::new()` is used throughout and nothing in this column is expendable — every control
  here is either terminal or ellipsizing.

### Verification

Run against a copy of the working tree with two in-flight sibling compile errors patched out
(`control_row.rs` dead `rendered_children`, `layout.rs` `let mut visual`), because the workspace does
not currently build in files owned by T072.

- `cargo test -p app` — 156 passed, 5 failed, all five in `workspace::layout::tests` (T072's file).
  All 18 `workspace::notes` tests pass, including the render-tree test.
- `cargo clippy -p app --all-targets --all-features -- -D warnings` — clean.
- `cargo fmt` — `notes.rs` reports no diff.
- Whitespace scan over `notes.rs` — clean (the file is untracked at HEAD, so `git diff --check`
  inspects nothing).

NOT RUN: the full-workspace build and test suite, and any launch of the signed app.
