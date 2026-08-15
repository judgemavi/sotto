# T095 — A control marked expendable must be able to collapse

**Status:** in-review

**Wave:** N7 — v2 workspace

**Depends on:** nothing. `ControlRow::for_width` and its collapse threshold already exist and are
tested (`crates/app/src/workspace/control_row.rs`); nothing in that file needs to change for the
narrow fix.

**Owns:** `crates/app/src/workspace/transcript.rs`, the width plumbing its head builder needs from
its caller, and `crates/app/src/workspace/control_row.rs` if the guard in step 2 is taken. This is a
planner-amended exception to per-file ownership, because the caller supplying the width is not
`transcript.rs`. It does **not** grant redesign of anything it touches.

**Concurrency:** T077 and T078 hold work in these files. Coordinate before starting; if a lane is
active, stop and report rather than racing it.

## Why this exists

`ControlRow` has two constructors, and only `for_width` can collapse anything:
`control_row.rs:82` drops an `Expendable` child only when `self.available <= COLLAPSE_WIDTH`, and
`available` is set by `for_width`.

**`ControlRow::new()` is not a defect.** T072 records the reasoning, and it is sound: `new()` exists
for rows inside a container whose width the caller cannot see — the rail, the column heads — and
*"it never drops, because it has no honest basis for deciding it must."* A width-blind row that
refused to render a control would be guessing.

The defect is the *contradiction*: `transcript.rs:740` builds its head with `new()`, and
`transcript.rs:761` marks the legend `Expendable` inside it. The role promises a collapse the
constructor can never deliver. The label is a claim about behaviour that does not happen.

Scope check, so this is not over-fixed: `ask.rs` and `notes.rs` also build rows with `new()`, and
**both are correct** — neither marks any child `Expendable`. The transcript head is the only site
where the two disagree. An earlier revision of this task claimed all three were defective; that was
wrong, and fixing the two correct ones would have removed deliberate behaviour.

## The trap in the existing test

T074's mounted test asserted the narrow legend *remains present* while its measured width shrinks.
That is evidence of the wrong thing: it passes precisely because the row does not collapse. Any test
here must assert **absence** at or below the threshold, not a smaller width. `control_row.rs:252-261`
has the shape that actually proves collapse.

## Plan

1. Resolve the contradiction in the transcript head. Either thread the available width from the
   caller and build with `for_width(available)`, following the parameter pattern `layout.rs:335`,
   `:512`, and `:607` already use — or, if the width genuinely cannot be known there, stop marking
   the legend `Expendable` and say in the code why it does not collapse. **Threading the width is
   the expected answer**; the mock treats the legend as collapsing and T074's acceptance requires it.
   Do not invent a transcript-local breakpoint: `COLLAPSE_WIDTH` is shared so every column sheds at
   the same moment rather than the window becoming a patchwork.

2. Consider making the contradiction unrepresentable. T072 already made a role-less control a
   compile error rather than a clipping defect, by having `finish()` return a type that does not
   implement `ParentElement`. The same instinct applies here: a width-blind row accepting an
   `Expendable` child is a silent lie, and it stayed hidden long enough to become a review finding.
   A debug assertion is the cheap version; refusing it in the type system is the honest one. If you
   judge the cost too high, say so and why — do not skip it silently.

## Acceptance

- At or below `ControlRow::COLLAPSE_WIDTH` the transcript legend is **absent**, asserted by test —
  not merely narrower. Above the threshold it is present with positive bounds.
- The transcript label remains at every width; only the legend collapses.
- No control in the transcript column clips at the stated minimum width.
- Nothing essential is reclassified to make a row fit.
- `ask.rs` and `notes.rs` are unchanged unless step 2's guard forces a signature change.
- Full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Changing `COLLAPSE_WIDTH`'s value; the rail, which documents why it uses a width-blind row;
restyling any control; the summary taxonomy.

## Notes

Found while reviewing T076's handoff on 2026-08-15, and corrected the same day after T072's design
note showed the original framing was wrong. T074 remains open on its own acceptance and should close
on this task's completion rather than duplicating the fix.

## Implementation notes — 2026-08-15

The transcript head now receives the width it actually occupies. `layout.rs` deducts the visible
rail (210 px or 248 px) and the open Ask panel (344 px) from the viewport, then gives a live
transcript its real 57.5% share of that stage; a review transcript receives the full stage. The head
then deducts its own two 12 px horizontal insets before calling `ControlRow::for_width`. A 900 px
viewport with the narrow rail therefore supplies 396.75 px to a live transcript column, not the
misleading 900 px, and the shared threshold makes the decision from the inner row width.

At or below `ControlRow::COLLAPSE_WIDTH`, the legend is not constructed. The label and row count
remain in the first row. A mounted 680 px live-shell regression exposed a second, pre-existing
truth: after the legend disappeared, the essential `Copy` and `Following live` controls still could
not fit beside the metadata in the ~270 px transcript column (`Following live` reached x=644 while
the column ended at x=480.5). Nothing essential was reclassified. At the same shared threshold,
those actions now receive a second `ControlRow`; the wide head remains one row. This is a responsive
placement of the same controls, not a new breakpoint or a restyle.

### Step 2 guard judgment

Taken in the cheap form. `ControlRow` records explicitly whether its width is known, and a debug
assertion rejects `ControlRow::new().child(ControlRole::Expendable, ...)`. A regression pins the
failure. A type-state split would make the invalid state unrepresentable in release builds, but it
would duplicate the builder surface or add generic state to every current call site for one misuse;
that cost is disproportionate here. The explicit flag avoids the brittle alternative of inferring
width knowledge from `Pixels::MAX`, and `ask.rs` / `notes.rs` remain unchanged because their
width-blind rows contain only essential and ellipsizing children.

### Acceptance evidence

- `the_legend_collapses_at_the_shared_threshold_while_the_label_remains` uses a fresh mounted
  harness at each width (debug bounds persist between frames), proves positive legend bounds at
  threshold + 1 px, proves the legend absent at and below the threshold, and proves the label
  remains.
- `transcript_width_uses_its_real_stage_share_not_the_viewport` pins narrow/wide rail, collapsed
  rail, Ask-open, live split, and full-width review calculations.
- `the_live_head_keeps_every_essential_inside_its_narrow_column` mounts the live Recording layout at
  the stated 680 px minimum with a row present. Label, Copy, and Follow all have positive bounds
  wholly inside `stage-transcript`; the legend is absent.
- Focused app gate: 38 passed, 0 failed across `workspace::control_row::tests`,
  `workspace::transcript`, and the width-plumbing test.
- `rustfmt --edition 2024 --check` and `git diff --check` over this task's three code paths are clean.
- Final combined gate: `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test --workspace --locked --quiet`
  passed outside the sandbox; the app result was 242 passed and 4 explicitly ignored real-media
  cases. The sandboxed rerun failed only eight unchanged ASR model-cache fixture tests with macOS
  `Operation not permitted`, and the same full command passed unsandboxed.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo clippy --workspace --all-targets --all-features --locked
  -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check` all pass on the final tree.

No signed-app visual run was performed. T074's citation-flash lifecycle remains its own acceptance
residual; its current mounted test asserts navigation/focus but does not inspect the 1.4-second
visual decay.
