# T016 — Board canvas v1: the timeline as a readable whiteboard

**Status:** done

**Wave:** 3 — Phase 2, the note-taker dogfood gate

**Depends on:** T003 (**do not start until the spike passes**; on a fail this task is
rewritten against the Electron fallback) · T011 (live timeline) · T012 (the tokio↔GPUI
seam and the dev window it establishes) · T015 (screen snapshots to pin)

**Owns:** `crates/app/src/board/**`, `crates/app/src/lib.rs`

## Goal

The first half of the Phase 2 dogfood gate. `AGENTS.md`: *"If the timeline isn't good
enough to read, it isn't good enough to reason over."* This task makes the fused
audio+screen timeline readable as a spatial artifact — and the honest test of whether the
whole premise works. No advising is built until this is good.

Note the explicit non-goal: the note-taker is an **internal milestone, never shipped as a
product**. Build it to be used by us, on real calls, not to be demoed.

## Plan

1. **Utterance blocks along the time axis**, coloured per speaker, laid out with the
   append-only discipline the spike proved: position computed once on arrival, never
   recomputed. A correction (`supersedes`) updates a block's text **in place** without
   moving it or anything after it — this is exactly why T014 made the timeline
   append-only, and it is the property that makes the board glanceable.

2. **Prosody as space, not decoration.** A long pause renders as a literal gap;
   interruptions overlap; speech rate can modulate block density. This is the cheapest
   high-value thing on the board — the rep sees hesitation without reading a label.
   Use T006's annotations; do not invent a second prosody source.

3. **Screen thumbnails pinned to their interval.** Each `ScreenSnapshot` pins along the
   stretch of conversation it was visible for, using `visible_from..visible_to`. Load
   thumbnails lazily by `FrameRef` and evict off-screen ones — the board must not hold a
   two-hour call's frames in memory.

4. **Panning and zoom** per the spike's approach, with a "follow the frontier" mode that
   auto-scrolls to newest content and yields the moment the user pans away. Returning to
   the frontier should be one obvious gesture.

5. **Visual calm is a hard requirement**, not polish. New objects arrive gently at the
   frontier; nothing already read jumps or reflows. If a layout decision trades calm for
   density, choose calm — `AGENTS.md` makes this a differentiator, and a board that
   twitches during a live call gets closed exactly like a chatty copilot does.

6. **Post-call review** is the same view with no live tail: pan the map of the
   conversation rather than scrub a transcript. Loading a persisted timeline from SQLite
   (T008) must produce a board identical to the one built live — replay equivalence is
   testable, so test it.

7. **Do not build** suggestion cards, topic clustering, or open-objection tracking here.
   Those are Phase 3/4 and depend on the advisor. Leave the anchoring seam — a block must
   be able to host a card budding off it — but no advising UI.

## Acceptance

- A real recorded call renders as a readable board: correct speakers, visible prosody,
  screen thumbnails on the right intervals.
- Corrections update in place with zero movement of surrounding content.
- Replaying a persisted timeline reproduces the live board exactly.
- Frame time and memory stable over a two-hour session with thumbnails.
- **The gate:** we use this on real calls and agree the timeline is accurate and readable.
  That judgement is the deliverable — record it in the task notes with what was wrong.

## Out of scope

Suggestions and any advising UI, topic clustering, user annotations (Phase 4), the
overlay lens (T012 owns the seam; the production lens is Phase 4), shipping this.


## Amendment — the board is now a shipping tier, not a dogfood gate (2026-07-29)

`AGENTS.md` has changed underneath this task. The map is no longer an internal milestone we
use and discard: it is **the first usable product tier, and it ships**. It works with no API
key configured, and onboarding runs through it — a rep tries Sotto on one call before anyone
asks them for a credential.

That raises the bar here. Previously "readable enough to prove the timeline works" was
enough. Now:

- **It must be good standalone.** Someone with no model configured should find reviewing a
  call on this board genuinely better than scrubbing a recording. That is the bar.
- **No dead ends where the reasoning tier would be.** With no key configured there is no
  empty "summary" panel, no greyed-out "clusters" button, no error state. Absence of the
  reasoning tier is a *normal* configuration, not a degraded one — the UI should not imply
  the user is missing something broken.
- **Leave the second axis room.** T019 produces topic regions, cross-links and open threads
  as a derived view keyed by `EventId`. The board renders that as an **optional overlay** the
  user can switch off, falling back to pure chronology. Design the layout so clusters can be
  layered on later without reflowing what chronology already placed — the append-only
  discipline you already need for live capture is the same mechanism.

The gate still applies — use it on real calls and agree the timeline is accurate and
readable before advising work starts — but it is now a product bar as well as a checkpoint.


## Amendment — the gate does not need sales calls (2026-07-29)

Clarified in `AGENTS.md`: capture is scoped to any window or application, so validating this
tier does not require access to a sales pipeline. Any two-party conversation exercises
everything the map tier does — two audio streams with speaker attribution, prosody, screen
frames, OCR, board layout, post-call review. A 1:1 with a colleague is sufficient: mic is
you, the captured app is them.

Only the advisor's validation needs real sales calls, because trigger types and battlecard
grounding are the sales-specific parts. Do not block this gate on sales-call access.

## Added acceptance criteria — inherited from T020's residual risk (2026-07-29)

T020 accepted GPUI on culling evidence (112,500 accumulated objects → 2/4/26 visible by zoom)
plus a one-minute p95 of 13.5 ms, while explicitly *not* running the long interactive test. The
harness now reports non-cumulative frame intervals, so the measurement is possible; it just has
not happened.

Board frame time at scale is this task's problem, so it lands here:

- **Frame time reported at 1, 10, 20 and 30 minutes as separate figures**, not an average — an
  average over 30 minutes hides exactly the accumulation drift being looked for.
- Confirmed no reflow of already-placed content across the whole run.
- Idle CPU, append CPU, RSS and GPU measured **with a real call running on the same machine**.

If long-run frame time does degrade, that reopens ADR-0003 rather than being worked around
here — say so and stop, do not paper over it.

## Transcript-first reasoning clarification (2026-08-11)

Local thumbnails remain part of the board and post-call artifact. Their presence does not
put OCR text or image bytes into model context. T028 owns that separate, opt-in,
timestamp-addressed reasoning path. Do not couple board thumbnail loading to whether a
reasoning backend is configured.

## Implementation notes — stable projection/layout slice (2026-08-11)

- Added a deterministic `BoardProjection` over the shared append-only timeline. Partial-to-final
  supersession replaces card content under one stable board identity and preserves the original
  rectangle; live append and persisted replay take the same projection path.
- Utterance placement uses event time on a fixed horizontal scale and separate speaker lanes, so
  pauses remain gaps and simultaneous turns overlap. Viewport queries use timestamp-sorted
  culling rather than scanning the full call.
- Screen cards retain only the local `FrameRef`, capture label, and visible interval. OCR and
  reasoning state are deliberately absent, keeping board thumbnails independent from T028's
  opt-in inspection path.
- Added the minimal `pub mod board` registration and a `BoardState` adapter that consumes unseen
  events from T012's existing `Entity<TimelineState>` seam. No second Tokio channel was added.
- Five focused projection tests pass when compiled directly against `sotto_core`: stable
  supersession, time-axis gaps/overlap, local screen-card isolation, live/replay equivalence, and
  bounded viewport density after 10,000 events.
- `cargo test -p app --lib board::projection::tests --no-fail-fast` remains environment-blocked
  before the app crate is compiled: GPUI's build script cannot run `metal` because the Xcode Metal
  Toolchain is not installed. The GPUI-backed `BoardState` therefore still needs a normal app
  compile once that owner-managed prerequisite is available.

This task remains `in-progress`. The visual GPUI renderer, lazy frame loading/eviction, pan/zoom
and follow-frontier interaction, real-call/long-run performance measurements, and owner dogfood
judgement remain pending. Runtime mounting belongs in the existing app shell integration files and
requires planner ownership rather than expanding this task's file boundary.

## Implementation notes — GPUI board-lens slice (2026-08-11)

This slice supersedes the renderer/navigation items in the preceding residual list:

- Added `BoardCanvas` over the existing `Entity<TimelineState>` → `BoardState` seam. Each render
  consumes only the unseen timeline suffix, computes a world-space viewport, and asks the stable
  projection for visible cards only; the renderer does not introduce a second channel or rebuild
  placement.
- Added quiet manual pan/zoom plus explicit follow-frontier state. Manual panning yields follow
  immediately, zoom is bounded and centre-preserving away from the live edge, and one visible
  `Follow newest` control restores frontier tracking.
- Added speaker-lane utterance rendering and local screen thumbnails. GPUI receives a local
  `FrameRef` only when its card is visible, supplies loading/unavailable placeholders, and evicts
  the image asset after it leaves the viewport. OCR, reasoning output, and provider state remain
  outside the board.
- Verification passed with the host-compatible feature/toolchain substitutions:
  - `cargo test -p app --lib board:: --locked --features gpui/runtime_shaders` with
    `WHISPER_DONT_GENERATE_BINDINGS=1`: 9 passed, 0 failed.
  - `cargo check -p app --lib --locked --features gpui/runtime_shaders` with the same Whisper
    substitution: passed.
  - `cargo clippy -p app --lib --locked --features gpui/runtime_shaders -- -D warnings` with the
    same substitution: passed.
  - `cargo fmt -p app -- --check`: passed.

This task remains `in-progress`. T032 owns mounting this lens into the product shell and wiring
explicit live/post-call surface selection; T016 does not cross that ownership seam. A real
conversation/post-call review, visual-calm judgement, and the required 1/10/20/30-minute frame,
CPU, RSS, and GPU measurements remain owner/manual acceptance gates. The default embedded-shader
build also still requires the missing Xcode Metal Toolchain; focused verification used GPUI runtime
shaders instead. No long-run stability or readability claim is made from the automated checks.

## Measurement-harness readiness (2026-08-11)

- Restored the opt-in board-owned interval harness needed by the inherited T020 gates. With
  `SOTTO_BOARD_METRICS=1`, measurement begins at the first projected board item, continuously
  requests animation frames, and emits separate non-cumulative 1/10/20/30-minute scheduling
  intervals plus total/visible/load-eligible-thumbnail-path counts. The latter records visible
  paths admitted to GPUI's loader, not confirmed decoded or cached image residency. The harness
  remains inert by default.
- `docs/experiments/map-tier-manual-acceptance.md` gives the signed-app launch, process sampling,
  GPU caveat, evidence schema, and board-readability rubric. T035 may provide overlapping real-call
  evidence without changing this task's status.
- No signed app, picker, capture, real call, or performance observation was run for this note.
  Readability, no-reflow, real-call idle/append CPU, RSS/GPU, 20/30-minute data, and two-hour
  stability therefore remain manual acceptance gates.
- Focused readiness verification passed with `WHISPER_DONT_GENERATE_BINDINGS=1`, a fresh target
  directory, and `gpui/runtime_shaders`: board tests 15 passed; app library check passed; strict
  app library clippy passed. Formatting and scoped diff checks also passed. The initial sandboxed
  attempt was environment-blocked by Swift's unwritable module cache; the identical outside-sandbox
  run passed.

## Independent measurement/runbook review — 2026-08-11

- Confirmed the harness is inert unless `SOTTO_BOARD_METRICS=1`, starts at the first projected
  item, uses nearest-rank percentile selection, clears intervals after every checkpoint, and
  disables itself after the final checkpoint so it neither retains samples nor requests animation
  frames indefinitely. Its output is explicitly a scheduling interval, never render duration or
  inferred FPS.
- Renamed the thumbnail field to `eligible_thumbnail_paths`. The board can observe paths that are
  visible and admitted to GPUI's image loader, but not whether asynchronous decode/cache loading
  succeeded; the prior residency label overstated that evidence.
- T016 remains `in-progress`. No signed-app performance observation, visual judgement, GPU/process
  attribution, no-reflow observation, or two-hour stability gate was inferred by this review.

## Notes/Board workspace ownership handoff — 2026-08-12

The deterministic board code slice and its focused automated review are settled. T016 releases
`crates/app/src/board/**` and the required app integration seam to T038 for the sequential
Notes/Board workspace implementation. T016 remains `in-progress` solely for its signed real-call
readability, no-reflow, resource, GPU, and long-run manual gates. T038 may integrate the board but
must not claim or alter those manual acceptance results.

## Owner zoom-usability correction — 2026-08-12

An owner screenshot showed the canvas at its 20% overview zoom, where transcript text is
intentionally too small to read, but the UI exposed no zoom state or recovery control. The Board
now renders explicit Zoom out, current-percent/reset, Zoom in, and Follow controls. Reset restores
100%, the top lanes, and frontier following. The focused reset regression and the full 80-test app
library suite pass; strict app Clippy passes. Signed visual/readability acceptance remains open.

## Product-surface retirement — 2026-08-12

ADR-0015/T048 supersedes the Board as a product surface after owner smoke testing found the spatial
canvas less readable than ordinary text. The implemented canvas remains historical code and
automated evidence, but the app no longer mounts it. Its unrun visual, zoom, GPU, and long-run
canvas gates are deliberately retired rather than claimed; T035 now validates the top-down live
transcript and persisted review instead.
