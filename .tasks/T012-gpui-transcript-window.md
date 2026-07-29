# T012 — GPUI dev window: live timeline + settings screens

**Status:** blocked (on T020 verdict — T011 done)

**Wave:** 2

**Depends on:** T003 (GPUI verdict and pinned version — **do not start until the spike
passes**; if it fails, this task is rewritten against the Electron fallback) ·
T011 (timeline events) · T007 (provider registry, for the settings screens)

**Owns:** `crates/app/src/devwindow/**`, `crates/app/src/settings/**`

## Goal

The first real GPUI code beyond the spike (`AGENTS.md` Phase 1): a dev window rendering
the raw live timeline, plus the settings screens for keys and model selection. Both are
grouped here because they share the tokio↔GPUI seam and the gpui-component widget set —
splitting them would mean two agents solving the same bridging problem.

This is deliberately **not** the board. It is the debugging view — a flat, honest,
scrolling dump of timeline events as they arrive, which is what you want when diagnosing
why the board looks wrong. T016 builds the spatial canvas on top of the same seam.

Settings ship here rather than later because Phase 2's summarizer (T017) is BYOK and
needs keys configurable before the note-taker gate can be dogfooded at all.

## Plan

1. **The seam.** Exactly one place where timeline events cross into GPUI entities, built
   the way T003's ADR prescribes. Everything else in the UI reads GPUI state. Keep it
   in one module and document it — `AGENTS.md` calls for a single well-defined seam and
   this is where that promise is kept or lost. **T016's board consumes this same seam**,
   so treat its shape as an interface, not an internal detail.

2. **Timeline view.** A flat chronological list of every event kind — utterances per
   speaker with inline prosody, VAD transitions, screen snapshots, errors. Show `id`,
   `ts` and `supersedes` so append-only behaviour is directly observable; a partial being
   superseded should be visibly a *new event referencing an old one*, not a mutation.
   Include a kind filter. Auto-scroll that yields when the user scrolls back.

3. **Performance.** Partials arrive several times a second for a call lasting an hour.
   Virtualise the list; do not re-render the whole transcript per event. Measure frame
   time with a long synthetic transcript before calling this done.

4. **Settings screens** with `gpui-component` at the version pinned by T003:
   - Provider/key management — add, validate (a real cheap test call), and delete keys
     per provider. Keys go to the keychain via T007; the UI never persists them itself
     and never renders them back after entry.
    - Model selection per role (watcher, suggester, summarizer), including the Ollama
     fully-local path with no key at all.
   - Audio device selection and a level meter per stream.
   - Session start: the target picker flow. Starting a session is turn-on-then-pick, using
     `SCContentSharingPicker` rather than a chooser of our own. Stopping is always one
     obvious action away, and the app never resumes a session by itself. There is no
     app-exclusion list any more — scope replaced it.
   - Speculation aggressiveness — `AGENTS.md` makes this a user setting because it is
     their tokens and their tradeoff. Present the cost implication honestly in the UI.

5. **Error surfacing.** Bad key, rate limit, revoked capture permission, model not
   downloaded — each with a clear message and an action. Distinguish *your key is bad*
   from *the network is down*, using T007's error taxonomy.

6. **Recording indicator — show *what*, not just *that*.** A visible, always-present
   indicator whenever capture is live, naming the captured target and distinguishing audio
   from screen. A user who knows their audio is recorded may not realise their screen is,
   and a user who picked one window should be able to confirm at a glance that it is still
   the only thing being captured. This is a consent feature and a core differentiator, not
   decoration: it cannot be hidden, and no setting may disable it.

   If T002 reports that audio cannot be scoped per-application, the indicator must say so
   — "screen: Zoom · audio: system" is honest; implying both are scoped is not.

## Acceptance

- Live timeline from a fixture run renders smoothly, with supersessions visibly arriving
  as new events rather than in-place edits.
- Frame time stable over a one-hour synthetic session.
- Keys round-trip through the keychain and never render back.
- Recording indicator provably visible whenever capture is active, naming the target and
  distinguishing audio from screen — and truthful about which of them is actually scoped.
- The seam is documented well enough for T016 to build the board on it without changes.

## Out of scope

The board canvas (T016), the production overlay lens (Phase 4), suggestion rendering
(T013), tray/menubar, auto-updater.
