# T003 — Spike B: GPUI whiteboard canvas + overlay lens

**Status:** done

**Wave:** 1 — start early alongside T002; **timeboxed to 7 days**

**Depends on:** T001 (workspace only — this spike uses fake data, not the timeline)

**Owns:** `crates/app/**`, `docs/adr/0003-ui-framework-decision.md`

> Rewritten after the `AGENTS.md` reframe. The earlier version spiked only a suggestion
> overlay at a 5-day timebox. The product is now a spatial whiteboard with the overlay as
> one *lens onto it*, which is a materially harder rendering problem — hence the rewrite
> and the extended timebox.

## Goal

Decide whether GPUI can draw the whiteboard. `AGENTS.md` argues GPUI is the right tool
precisely *because* of this canvas — "a zoomable, smoothly-scrolling, constantly-appending
scene with hundreds of live objects." This spike tests that claim before the product is
built on it. Pass and GPUI is confirmed; fail and we fall back to a thin Electron shell
with a WebGL/2D canvas over the identical headless core.

**Do not exceed the timebox.** A clean "fail" with an ADR is a successful outcome of this
task. A two-week fight is not.

## Plan

1. **Pin the version.** `gpui` and `gpui-component` at an *exact* version — never `*`.
   Prefer the crates.io release; if a needed fix is unreleased, pin an exact git revision
   of the Zed repo and justify it in the ADR. Record the resolved version in the notes —
   every later UI task must match it.

2. **Part 1 — the canvas.** A zoomable, pannable scene that appends fake utterance blocks
   continuously for **30+ minutes at 60fps**, with a suggestion card streaming
   token-by-token anchored to one of those blocks. Requirements that are the actual test:
   - **Append-only layout with no reflow.** Nothing already on screen may move when new
     content arrives. `AGENTS.md` makes this a hard requirement — "the rep glances; the
     rep never *watches*" — and it is also what makes the canvas tractable: exploit the
     append-only timeline, compute each block's position once, never recompute.
   - **Culling.** Thirty minutes of blocks is far more than fits in a frame or a GPU
     buffer. Only visible objects should cost anything. Measure with the viewport at
     several zoom levels.
   - **Zoom that stays smooth** with hundreds of live objects, and text that stays legible
     (or degrades deliberately to blocks) as it shrinks.

3. **Part 2 — the overlay lens.** The *same* scene presented through a compact
   always-on-top, non-activating window showing the board's newest edge. Prove:
   - (a) stays visible over full-screen Zoom/Meet;
   - (b) **never steals keyboard focus** — typing in meeting chat while it updates must
     not drop a keystroke;
   - (c) click-through toggling between passive and interactive modes.

   In AppKit terms: a non-activating `NSPanel` at `.floating`/`.statusBar` level with
   `collectionBehavior` including `.canJoinAllSpaces` and `.fullScreenAuxiliary`. The real
   question is how much GPUI exposes and how much needs raw `objc2` underneath. Reaching
   under GPUI is acceptable — document every place you had to.

4. **One model, two lenses.** Build both views over a single scene structure, since
   `AGENTS.md` requires the overlay be "a viewport onto the board's newest edge — same
   data, same objects, zoomed in. Never a separate UI with separate state." If that proves
   impossible in GPUI, say so loudly: it is a finding about the architecture, not just
   about this spike.

5. **The tokio↔GPUI seam.** Drive the fake append stream from a tokio task the way real
   timeline events will arrive. Note in the ADR exactly which primitive bridged it
   (`cx.spawn` / `AsyncApp` / channel drain on the foreground executor) — T012 inherits
   this and it is meant to be the one seam in the product.

6. **Measure.** Frame time at 1/10/30 minutes of accumulated content, idle CPU, CPU while
   appending, RSS, and GPU usage — all with a real Zoom call running on the same machine.
   The small-footprint claim is a product feature. Frame-time drift as content accumulates
   is the single number that decides this spike.

7. **ADR-0003 with the verdict.**
   - **Pass →** GPUI confirmed. The ADR becomes the reference for T012 and T016,
     including every objc2 escape hatch and the culling/layout approach that worked.
   - **Fail →** record precisely which part failed — canvas performance or NSPanel
     behaviour — and what was tried. Recommend the Electron fallback. The core is
     unchanged either way; this decides only who draws pixels.

## Acceptance

- 30-minute continuous append at 60fps with no reflow of existing content and no
  frame-time degradation, or an ADR explaining what broke.
- All three overlay behaviours demonstrated against a real full-screen call.
- Both lenses driven from one scene model.
- Exact GPUI version pinned; resource measurements recorded.
- Verdict written before the timebox expires.

## Out of scope

Real timeline data, settings screens, tray/menubar, persistence, styling beyond what the
performance test requires. This is a throwaway spike — T012 and T016 build the real thing
from its findings.

## Notes

- Pinned aligned latest releases exactly: `gpui = 0.2.2`, `gpui-component = 0.5.1`.
- Built a shared append-only `Scene`, stable anchored suggestions, viewport culling, and
  board/overlay lenses over one GPUI entity. Model tests cover no-reflow and culling with
  10,000 accumulated utterances.
- The executable's Tokio producer now starts an anchored suggestion periodically and
  streams five incremental text updates at 100 ms intervals. Scroll pans a lens in world
  space; Control-scroll zooms between 0.15x and 3x without modifying shared placements.
- The fake producer runs on Tokio and crosses one bounded channel drained by one GPUI
  foreground task; ADR-0003 records the seam for T012.
- Full validation now passes: `cargo check -p app`, `cargo test -p app` (3/3), strict app
  Clippy, plus `cargo test --workspace --all-features` (21 passed, 5 ignored) and strict
  workspace Clippy. The two `manual_is_multiple_of` findings are fixed.
- Culling now uses the monotonic append order to seek with `partition_point`; it does not
  linearly scan the whole accumulated scene. The release harness appends at ~60 Hz and
  requests continuous animation frames.
- GPUI's popup implementation already supplies NSPanel, non-activating style, popup level,
  all-Spaces, and full-screen-auxiliary behavior. The only objc2 escape hatch obtains the
  owning NSWindow from GPUI's raw NSView and calls `setIgnoresMouseEvents:`; the harness
  alternates passive/interactive modes every five seconds.
- Toolchain: GPUI 0.2.2; Xcode 26.6 (17F113); Metal Toolchain 17F109 / Apple metal
  32023.883. Transitive `block 0.1.6` and `proc-macro-error2 0.2.1` emit non-actionable
  future-incompatibility warnings.
- One-minute release smoke evidence: 7,179 frame intervals, p50 8.332 ms, p95 13.517 ms;
  process CPU 8.7%, RSS 72,608 KiB at 1:29 elapsed. This was not run beside a real call.
- The task remains `in-progress`: the real 30-minute resource measurements, full-screen
  call test, focus/keystroke test, and click-through behavior have not been demonstrated.
  The 10/30-minute frame-time drift, multiple-zoom visible counts, GPU usage, real-call
  resource load, full-screen presence, keystroke focus, and manual click-through evidence
  remain unrun. No verdict is claimed.

## Environment update — Xcode installed (2026-07-29)

Xcode 26.6 (17F113) is now installed at `/Applications/Xcode.app`, replacing the
Command Line Tools–only host that blocked this task. Two consequences:

1. **The Metal Toolchain is a separate 688 MB component in Xcode 26**, not part of the base
   install. It has now been downloaded (`Metal Toolchain 17F109`) and verified:
   `xcrun metal --version` reports `Apple metal version 32023.883`, target
   `air64-apple-darwin25.5.0`. GPUI compiles Metal shaders at build time, so this — not
   Xcode itself — was the real gate. **No environmental blocker remains.**

   Record both the Xcode version (26.6 / 17F113) and the Metal Toolchain version (17F109)
   in ADR-0003 alongside the pinned GPUI version. GPUI is pinned exactly, and a host
   toolchain that only works on one Xcode version is exactly the fragility that ADR exists
   to capture. T010 needs the same two versions pinned in CI.

2. **Nothing about the spike's substance changes.** The timebox is unchanged and still
   measured in working days, not calendar days lost to environment setup. The verdict is
   still the deliverable, and a clean "fail" with an ADR is still a successful outcome.

With the toolchain resolved, the parts that were deferred as unverifiable are now in
scope and are the point of the task:

- the 30-minute continuous append at 60fps with no reflow, measured, with **frame-time
  drift as content accumulates** reported explicitly — that single number decides this
  spike;
- culling verified at several zoom levels, not assumed;
- all three overlay behaviours against a real full-screen Zoom or Meet call, especially
  the keystroke test: type in meeting chat while the panel updates and confirm nothing is
  dropped;
- resource measurements with a real call running on the same machine.

Report the resolved GPUI version and the exact Xcode version together — T012 and T016
inherit both.

### First full-workspace verification (2026-07-29, post-Metal)

With the Metal Toolchain installed, `app` compiles for the first time and the **whole
workspace is buildable** — a state this project has never been in. Results:

- `cargo check -p app` — **passes.** `gpui = "=0.2.2"`, `gpui-component = "=0.5.1"`, both
  pinned exactly as required. Build takes ~27s incremental from warm deps.
- `cargo test --workspace --all-features` — **21 passed, 0 failed, 5 ignored** (up from 18;
  `app` contributed 3).
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — **fails, 2
  errors in `app`**, both `clippy::manual_is_multiple_of` at `crates/app/src/main.rs:108`
  and `:110` (`index % 2 == 0` → `index.is_multiple_of(2)`).

Fix the two lints. They are trivial, but the reason they exist is worth noting: **`app` has
never once been linted**, because the Metal gate meant it could not compile. Assume nothing
in this crate has been checked against the strict table and re-read it accordingly — the
board README documents the house idiom.

Two upstream crates (`block v0.1.6`, `proc-macro-error2 v2.0.1`, both transitive through
GPUI) emit future-incompatibility warnings. Not actionable now; note them in ADR-0003 as
part of the GPUI dependency picture, since pinning GPUI exactly means we inherit its
dependency graph deliberately.


## Superseded by T020 for the remaining measurements (2026-07-29)

The canvas, culling and click-through work here is accepted. The unclaimed checks — 30-minute
append, zoom/GPU, full-screen call, keystroke, click-through — plus the verdict itself are
split into **T020** so they can be picked up cold and finished, rather than staying open while
T012 and T016 wait.
