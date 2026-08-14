# T079 — Decide whether a recording can be paused

**Status:** todo

**Wave:** M4 — recording

**Depends on:** nothing. `crates/capture/**` is free; T061's remaining obligations are the manual
runs held by T064.

**Owns:** `crates/capture/**`, the pause path in `crates/app/src/session/**`, the capture bar in
`crates/app/src/workspace/layout.rs`, and this task

## Why this exists

The capture bar shipped a **Pause** button that was hardcoded `.disabled(true)`. Nothing in
`crates/capture` or `crates/app/src/session` can suspend a recording — the control was drawn because
the mock draws one, and wired to nothing.

Removed on 2026-08-14 rather than left visible. A permanently disabled control is worse than an
absent one: a person reads it as a capability that is momentarily unavailable and waits for it,
where an absent control simply tells the truth. The Import entry point is the counter-example done
correctly — it is present, inert, and says in place that it is not built yet.

## The decision is not obvious, which is why it is a task

Under ADR-0018 the recording is the source of truth and **transcript time is media time**. A
citation at 02:18 addresses 02:18 of the recording exactly, and that identity is what makes screen
extraction and re-transcription trustworthy.

Pausing puts a gap in wall-clock time that has no counterpart in media time. Two honest options,
and they differ in more than implementation:

1. **Pause compresses the timeline.** The recording contains only captured material, so media time
   stays continuous and every existing citation contract holds unchanged. But the recording no
   longer maps to the wall clock: a person cannot reason from "we started at 2pm" to a position in
   the media, and the session's elapsed time and its media duration diverge permanently.
2. **Pause preserves the wall clock** by writing silence, or by recording an explicit gap the
   reader honours. Media time still matches the clock, at the cost of storing nothing useful — or
   of teaching every reader, the transcriber, and the frame decoder about gaps.

Option 1 is simpler and preserves the invariant that matters most. Option 2 preserves an intuition
users may actually rely on. Pick one, say what it costs, and amend ADR-0018 if the media-time
identity changes at all.

A third answer is legitimate: **pause is not worth it.** Stop-and-start-a-new-recording already
exists, and two recordings may be a more honest model of "I stopped and resumed" than one recording
with a hole in it. If that is the conclusion, remove Pause from the mock too, so the design stops
promising it.

## Plan

1. Decide, with the media-time consequence stated explicitly.
2. If pausing is built: suspend and resume the writer, keep the transcription reader correct across
   the boundary, and prove a citation on either side of a pause still lands on the right moment.
3. Restore the control only once it works. It is `Expendable` in the capture bar's shrink order —
   it must collapse before Stop or the clock lose any width.
4. Update `docs/design/workspace-v2-mock.html` to match whichever way this goes.

## Acceptance

- The decision and its media-time consequence are recorded here and, if the invariant moves, in
  ADR-0018.
- If built: a recording pauses and resumes, and a transcript citation on each side of the pause
  resolves to the correct moment in the media, asserted against a real recording.
- If built: the retained recording is playable across the pause boundary.
- If not built: no Pause control exists in the product or the mock.
- Per the verification rule, any pause behaviour is exercised against a real capture rather than
  simulated at the type level.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Trimming or editing a recording, resuming a *stopped* session, and import (T071).
