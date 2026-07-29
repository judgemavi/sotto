# T020 — Close out the GPUI canvas verdict

**Status:** todo

**Wave:** blocking — T012 and T016 cannot start without this verdict

**Depends on:** T003 (the canvas and overlay code already exist; only the measurements are
missing)

**Owns:** `crates/app/**`, `docs/adr/0003-ui-framework-decision.md`

## Why this is a separate task

T003 built the canvas, the culling, the objc2 click-through escape hatch, and recorded a
one-minute release smoke: **p50 8.332 ms, p95 13.517 ms, 8.7% CPU, 70.9 MiB RSS.** It then
explicitly declined to claim the checks it had not run, which was the right call.

But the verdict is the deliverable, and it has been outstanding while everything else moved.
The code is done; what is missing is measurement. So this task is scoped to *only* the
unclaimed checks, so it can be picked up cold and finished.

Two spikes gated Phase 0. Capture is effectively settled. This is the other one, and until
it resolves, every UI task in the plan rests on an unconfirmed foundation.

## What is still unclaimed

1. **The 30-minute continuous append at 60fps.** This is the check that decides the spike.
   The one-minute smoke says nothing about the question that matters: **does frame time drift
   as content accumulates?** Report frame time at 1, 10, 20 and 30 minutes as separate
   figures, not an average — an average over 30 minutes hides exactly the degradation we are
   looking for. Confirm no reflow of already-placed content throughout.

2. **Culling at several zoom levels.** Verified, not assumed. Thirty minutes of blocks
   exceeds what fits in a frame or a GPU buffer; only visible objects should cost anything.
   Measure at a few zoom levels, including fully zoomed out where everything is nominally
   on screen.

3. **All three overlay behaviours against a real full-screen Zoom or Meet call:**
   - stays visible over the full-screen meeting window;
   - **never steals keyboard focus** — type in the meeting chat while the panel updates and
     confirm not one keystroke is dropped. This is the one most likely to fail and the one
     users would notice immediately;
   - click-through toggling between passive and interactive.

4. **Both lenses driven from one scene model.** `AGENTS.md` requires the overlay be a
   viewport onto the board's newest edge, not a second UI with separate state. If that proved
   impossible in GPUI, that is a finding about the architecture and needs saying loudly.

5. **Resource measurements with a real call running on the same machine.** Idle CPU, CPU
   while appending, RSS, GPU. The small-footprint claim is a product feature, and we share the
   machine with Zoom.

## The verdict

Write it into `docs/adr/0003-ui-framework-decision.md`:

- **Pass →** GPUI confirmed. The ADR becomes the reference for T012 and T016: record the
  culling and layout approach that worked, every `objc2` escape hatch used, and which
  primitive bridged the tokio↔GPUI seam.
- **Fail →** say precisely which check failed and what was tried, then recommend the thin
  Electron shell with a WebGL/2D canvas over the same headless core. The core is untouched
  either way — `sotto-cli` already drives the full pipeline, so this decides only who draws
  pixels.

**A clean fail is a successful outcome of this task.** What is not acceptable is leaving it
open, because two UI tasks and the whole map tier are queued behind the answer.

Also record the toolchain versions the build depends on: **Xcode 26.6 (17F113)** and **Metal
Toolchain 17F109**, alongside the pinned `gpui = "=0.2.2"` / `gpui-component = "=0.5.1"`.
T010 needs both pinned in CI.

## Acceptance

- Frame time reported at 1/10/20/30 minutes with no degradation, or an ADR explaining what broke.
- Culling measured at multiple zoom levels.
- All three overlay behaviours demonstrated against a real full-screen call, keystroke test included.
- Resource figures captured with a call running.
- **ADR-0003 states a verdict.**

## Out of scope

Real timeline data (T012 wires that), the production board (T016), settings, tray, persistence.
