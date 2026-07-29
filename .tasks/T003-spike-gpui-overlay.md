# T003 — Spike B: GPUI non-activating always-on-top overlay panel

**Status:** todo (unblocked — T001 approved)

**Wave:** 1 — start early alongside T002; this one is **timeboxed to 5 days**

**Depends on:** T001 (workspace only)

**Owns:** `crates/app/**`, `docs/adr/0003-ui-framework-decision.md`

## Goal

Decide whether GPUI can draw our overlay. Per `AGENTS.md` Phase 0 this spike alone
gates the UI framework choice — pass and GPUI is confirmed for v1; fail and we fall
back to a thin Electron shell over the identical headless core. **Do not exceed the
timebox trying to force it.** A clean "fail" with an ADR is a successful outcome of
this task; a two-week fight with NSPanel is not.

## Plan

1. **Pin the version.** Add `gpui` and `gpui-component` to `crates/app/Cargo.toml` at an
   *exact* version — never `*`, per `AGENTS.md`. Prefer the crates.io release; if a
   needed fix is unreleased, pin an exact git revision of the Zed repo and say why in
   the ADR. Record the resolved version in the task notes so every later UI task
   matches it.

2. **Window that behaves like a HUD.** Build the panel with GPUI's window options:
   always-on-top window level, no activation on show, transparent background, no
   titlebar, and visibility across Spaces including over a full-screen meeting window.
   In AppKit terms the target is a non-activating `NSPanel` at
   `.floating`/`.statusBar` level with `collectionBehavior` including
   `.canJoinAllSpaces` and `.fullScreenAuxiliary`. The real question of this spike is
   how much of that GPUI exposes and how much needs raw `objc2` reaching under it.
   Reaching under GPUI is acceptable; document each place you had to.

3. **Prove the four hard behaviours** — these are the pass/fail criteria, test each
   explicitly against a real full-screen Zoom or Meet window:
   - (a) stays visible over full-screen meeting windows;
   - (b) **never steals keyboard focus** — typing in the meeting chat while the panel
     shows and updates must not drop a single keystroke;
   - (c) streams fake suggestion text token-by-token at ~30 tok/s with no jank or
     layout thrash;
   - (d) click-through toggling — a mode where mouse events pass to the app beneath,
     and a mode where the panel is interactive.

4. **Token streaming realism.** Drive the fake stream from a tokio task through the
   GPUI executor boundary the way real suggestions will arrive — this spike is also
   the first test of the tokio↔GPUI seam that `AGENTS.md` calls out as a single
   well-defined boundary. Note in the ADR exactly which primitive bridged it
   (`cx.spawn` / `AsyncApp` / channel drain on the foreground executor).

5. **Measure.** Idle CPU, CPU while streaming, RSS, and GPU usage while a Zoom call is
   running on the same machine. The small-footprint claim is a product feature; if the
   overlay idles at meaningful CPU, that is spike-relevant information.

6. **Write ADR-0003** with the verdict:
   - **Pass** → GPUI confirmed for v1; the ADR becomes the reference for how the
     production panel (T013 successor task) must be constructed, including every
     objc2 escape hatch used.
   - **Fail** → record precisely which behaviour could not be achieved and what was
     tried, then recommend the Electron-shell fallback. The core architecture is
     unchanged either way — this decides only who draws pixels.

## Acceptance

- All four behaviours in step 3 demonstrated on a real full-screen call, or an ADR
  explaining which failed and why.
- Exact GPUI version pinned and recorded.
- Resource measurements captured.
- Verdict written before the timebox expires.

## Out of scope

Real transcript data, settings screens, tray/menubar, suggestion history, styling
beyond what is needed to test streaming. This is a throwaway spike — the production
panel is a later task that consumes its findings.
