# T076 — Let the summary be read, not audited

**Status:** in-review

**Wave:** N7 — v2 workspace

**Depends on:** T075 (`in-review`), which built this column.

**Owns:** `crates/app/src/workspace/notes.rs` and this task

**Concurrency (planner, 2026-08-14):** T077 holds `transcript.rs`; T078 holds `layout.rs`, `mod.rs`
and `library.rs`. Edit neither.

## Two defects from the maintainer's first real read

### 1. Citations drown the prose

One claim carried **ten** timecode chips on its own line; several carry five or six. The chips
occupy more vertical space than the sentences they support, and a summary that cannot be read is
not a summary. The maintainer's call: *"I don't think we need to cite timestamps by default, can we
make them toggleable."*

**Citations stay mandatory in the data.** Validation is unchanged: a claim without evidence still
fails closed, and every chip must still resolve to a real transcript row. This is a display
decision only. Do not touch the citation contract, and do not let a display toggle become a reason
to stop requiring evidence.

Default to quiet. A claim should read as a sentence, with its evidence available on demand — a
per-summary toggle that reveals every chip, and a compact affordance on each claim so a reader can
check one line without revealing all of them. Keyboard and screen-reader users must still reach the
evidence.

### 2. The sources block explains a feature nobody is using

With no MCP sources configured, the column shows four lines: a heading, two paragraphs of policy
about read-only resources and bounded evidence, and a note that nothing is configured. The
maintainer: *"this banner serves no purpose."*

They are right in the empty case, and it matters more than it looks: those paragraphs are the
product's disclosure about what a summary is allowed to reach, and burying them in a permanent
banner that applies to nobody is how disclosures stop being read.

Show what is true. With no sources configured, the block should be absent or a single quiet line —
not a policy essay about a capability in no use. When sources exist, the policy is relevant and
should be stated where the reader can act on it.

## Also here

Summary text is not selectable — the maintainer cannot copy a line out. `gpui_component::text::TextView`
has `selectable(bool)` (`text_view.rs:497`) and the workspace has never used it. Make summary and
note text selectable. A note-taking product whose notes cannot be copied is failing at its own job.

## Acceptance

- A summary reads as prose at a glance; chips are hidden by default and revealed by an explicit
  toggle, with a per-claim path to its own evidence.
- Every revealed chip still resolves to the row it cites.
- Validation is untouched: an unsupported claim still fails closed, asserted by test.
- With no sources configured the block is absent or one quiet line; with sources it states the
  policy where it applies.
- Summary and note text can be selected and copied.
- No control clips at the stated minimum width.
- Focused and full app tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

The notes taxonomy (T070), the transcript column (T077), the shell and library (T078), Settings.

## Notes

### What was built

All three defects are closed inside `crates/app/src/workspace/notes.rs`. No other file was edited.

**1. Citations are quiet by default.** `EvidenceDisclosure` decides, per rendered summary, which
claims spend vertical space on chips. It is *display* state and holds nothing else: it lives in
window element state (`Window::use_keyed_state`), not on `MeetingWorkspace` and not in the record,
so revealing evidence cannot touch a session. Two ways to see the evidence:

- **One claim:** each claim carries a compact ghost control labelled with what it is holding —
  `10 timecodes`, `2 timecodes · 1 source`. Activating it opens that claim's chips and no others;
  the control then reads `Hide evidence`.
- **The whole summary:** one control above the first section reads `Show timecodes` /
  `Hide timecodes`. While it is on, the per-claim controls retire — the chips are already there.

A summary's identity is a fingerprint over its headings and claim texts; a re-summarize starts
quiet, because claim ordinals do not survive it.

**The citation contract is untouched.** Nothing in `insight` changed. `an_unsupported_claim_still_
fails_closed` runs the real generator over the real store twice — once with a claim citing nothing,
once with a claim citing a row that does not exist — and asserts both are rejected rather than
rendered. The rendered test still follows a revealed chip into the transcript row it cites.

**2. The sources block says only what is true.** `SourceContext::resolve` replaces the unconditional
banner:

- **Nothing configured** — one faint line: *"No outside sources configured; a summary is written
  from this recording alone. Add one in Settings."* No heading, no policy paragraphs.
- **A source configured** — the heading, the read-only policy, and the retrieval receipt, stated
  directly above the resource and query-disclosure controls the policy governs. The disclosure was
  not deleted; it was moved to where a reader can act on it.

**3. Text is selectable.** `SelectableText` wraps `gpui_component::text::TextView` with
`.selectable(true)` — the workspace's first use of it — for summary claims, typed notes in the
column, and typed notes pinned under transcript rows. `TextView` parses markdown, so
`as_literal_markdown` backslash-escapes ASCII punctuation first: prose that happens to contain `*`
or a leading `- ` must not silently restyle itself. Selection copies the rendered text, so the
escape never reaches the clipboard.

### Deviations and things the next task should know

- **`Window` reached the column through a `RenderOnce` component.** `notes::render_with_citation_
  times` is handed no `Window` by `layout.rs` (T078's file), and both `TextView` and
  `use_keyed_state` need one. `SummaryBody` and `SelectableText` are `RenderOnce`, which receive a
  `Window` at draw time. No public signature in this module changed, so `layout.rs` and
  `transcript.rs` were not touched. `SummaryBody` carries a `WeakEntity<MeetingWorkspace>` rather
  than a `Context`, so it cannot re-lease the entity `MeetingWorkspace::render` already holds.
- **`VisualTestContext::debug_bounds` never clears between frames.** `Frame::clear()` skips
  `debug_bounds`, so an "is_none" assertion is only meaningful for a selector that has *never* been
  drawn in that window. Two absence assertions here are written against selectors that were never
  rendered; the "per-claim control retires once everything is revealed" behaviour is asserted over
  `Revealed` directly instead. Worth knowing before writing another mounted test.
- **`cargo fmt -p app` was run once and may have reformatted `crates/app/src/workspace/
  transcript.rs`** (T077's file) while that task was mid-flight. Formatting only, no semantic edit;
  subsequent formatting used `rustfmt` on `notes.rs` alone. Flagged because it crosses an ownership
  line.

### Acceptance

| Item | State |
|---|---|
| Reads as prose; chips hidden by default, per-claim and per-summary reveal | PASS — `the_summary_reads_as_prose_and_gives_up_its_evidence_only_when_asked`, `evidence_is_hidden_until_a_reader_asks_for_one_claim_or_for_all_of_them` |
| Every revealed chip resolves to the row it cites | PASS — same rendered test follows a chip to `focused_event` |
| Validation untouched; an unsupported claim fails closed | PASS — `an_unsupported_claim_still_fails_closed` |
| Empty sources = one quiet line; configured = policy where it applies | PASS — `the_sources_block_is_one_quiet_line_until_a_source_exists`, `a_configured_source_states_its_policy_where_the_controls_are` |
| Summary and note text can be selected and copied | PASS — `a_summary_claim_can_be_selected_and_copied` drags across a rendered claim, presses the copy binding and reads the real clipboard |
| No control clips at 680 px | PASS — bounds asserted for every control in both the quiet and revealed states |
| Focused and full app tests, strict Clippy, formatting, diff checks | PASS — 188 app tests; `cargo clippy --workspace --all-targets --all-features -- -D warnings` exit 0; `cargo fmt --all -- --check` exit 0 |

**NOT RUN**

- **Keyboard activation of the evidence controls.** The controls are tab stops with spoken text
  labels, and `evidence_control` adds an Enter/Space listener because `gpui-component`'s button
  binds no key activation. That key path has no automated coverage: the mounted-render harness
  builds a window whose root view is `MeetingWorkspace` rather than `gpui_component::Root`, so a
  simulated keystroke panics inside `gpui-component`'s root lookup, and simulated mouse events in
  that harness never move focus onto a button. Both are properties of how the window is mounted
  (`mod.rs`, T078), not of this control. Needs a manual pass, or a `Root`-mounted harness.
- **Screen-reader announcement.** GPUI exposes no accessibility tree to assert against; the
  mitigation is that every control carries a readable text label rather than an icon.
- **Visual acceptance of the quiet summary on a real recording.** Verified in the render tree at
  680 px and 900 px, not by eye on a real summary.
