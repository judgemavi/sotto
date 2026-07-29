# T012 — GPUI dev window: live transcript + settings screens

**Status:** blocked (on T003, T011)

**Wave:** 2

**Depends on:** T003 (GPUI verdict and pinned version — **do not start until the spike
passes**; if it fails, this task is rewritten against the Electron fallback) ·
T011 (pipeline events) · T007 (provider registry, for the settings screens)

**Owns:** `crates/app/src/transcript/**`, `crates/app/src/settings/**`

## Goal

The first real GPUI code beyond the spike (`AGENTS.md` Phase 1): a dev window rendering
the live transcript, plus the Phase 2 settings screens for keys and model selection.
Both are grouped here because they share the tokio↔GPUI seam and the gpui-component
widget set — splitting them would mean two agents solving the same bridging problem.

## Plan

1. **The seam.** Exactly one place where pipeline events cross into GPUI entities, built
   the way T003's ADR prescribes. Everything else in the UI reads GPUI state. Keep it
   in one module and document it — `AGENTS.md` calls for a single well-defined seam and
   this is where that promise is kept or lost.

2. **Transcript view.** Two-column or interleaved rendering of rep versus customer
   turns, with prosody annotations shown inline. Partials render in a visually distinct
   state and are replaced in place when superseded — using the `(source, start)`
   supersede key from T001, not by appending. Auto-scroll that yields when the user
   scrolls back.

3. **Performance.** Partials arrive several times a second for a call lasting an hour.
   Virtualise the list; do not re-render the whole transcript per event. Measure frame
   time with a long synthetic transcript before calling this done.

4. **Settings screens** with `gpui-component` at the version pinned by T003:
   - Provider/key management — add, validate (a real cheap test call), and delete keys
     per provider. Keys go to the keychain via T007; the UI never persists them itself
     and never renders them back after entry.
    - Model selection per role (watcher vs suggester), including the Ollama
      fully-local path with no key at all.
   - Audio device selection and a level meter per stream.
   - Speculation aggressiveness — `AGENTS.md` makes this a user setting because it is
     their tokens and their tradeoff. Present the cost implication honestly in the UI.

5. **Error surfacing.** Bad key, rate limit, revoked capture permission, model not
   downloaded — each with a clear message and an action. Distinguish *your key is bad*
   from *the network is down*, using T007's error taxonomy.

6. **Recording indicator.** A visible, always-present indicator whenever capture is
   live. This is a consent feature and a core differentiator, not decoration — it
   cannot be hidden, and no setting may disable it.

## Acceptance

- Live transcript from a fixture run renders smoothly with partials superseding correctly.
- Frame time stable over a one-hour synthetic transcript.
- Keys round-trip through the keychain and never render back.
- Recording indicator provably visible whenever capture is active.

## Out of scope

The production overlay panel (Phase 3), suggestion rendering (T013), tray/menubar,
auto-updater.
