# T003 — Spike B: GPUI whiteboard canvas + overlay lens

**Status:** todo

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
