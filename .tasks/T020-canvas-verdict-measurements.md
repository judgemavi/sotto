# T020 — Close out the GPUI canvas verdict

**Status:** done

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

## Review round 1 — approved

The verdict is accepted, and more importantly the *epistemics* are right. I came into this
expecting to push back on a premature pass and did not need to.

**What earned the accept.** The culling measurement is the strongest evidence in the spike:

| Zoom | Accumulated | Visible |
|---:|---:|---:|
| 3.00x | 112,500 | 2 |
| 1.00x | 112,500 | 4 |
| 0.15x (min) | 112,500 | 26 |

At 112,500 objects — roughly 30 minutes of appends — visible objects stay bounded by viewport
density rather than accumulated duration. Combined with the one-minute p95 of 13.5 ms, that is
a sound *argument* that frame time will not drift, and the ADR correctly refuses to overstate
it: *"The test does not claim GPU behaviour."* Placement-once with `partition_point` seeking,
one bounded mpsc drained every 16 ms through `AsyncApp::update`, `WindowKind::PopUp` giving a
non-activating panel, and exactly one `objc2` escape hatch for click-through — all documented
well enough for T012 and T016 to build on.

**Why the unrun checks do not block the accept.** They are concentrated in the *overlay*:
full-screen visibility, focus theft, keystroke drop, click-through, colocated GPU use. The
overlay lens is Phase 4 work. T012 (dev window + settings) and T016 (board canvas) do not
depend on any of it, so board work can proceed on canvas evidence while overlay behaviour
stays open. That containment is what makes "retain GPUI" defensible rather than optimistic.

Naming each unrun check, explaining that the environment had no interactive meeting call, and
writing *"residual implementation risk, especially for focus behaviour, but not a demonstrated
GPUI failure"* is exactly the right distinction. Fixing the harness to report non-cumulative
intervals so the outstanding run *can* expose drift — rather than running a cumulative average
that would hide it — is the useful thing to have done with the time.

### The two residual items are now tracked, not just noted

A sentence in an ADR is not a work item. Both have been added as explicit acceptance criteria
on the tasks that actually depend on them:

- **The 30-minute interval run → T016.** Board frame time at scale is the board's problem, and
  T016 cannot ship the map tier without it.
- **Real-call overlay checks → Phase 4's production overlay lens.** Focus theft is the one most
  likely to fail and the one a user notices instantly.
