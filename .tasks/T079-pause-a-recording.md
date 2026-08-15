# T079 — Decide whether a recording can be paused

**Status:** done

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

## Decision (2026-08-15): option 3 — pause is not worth building

**Pause is not built. No Pause control exists in the product or the mock.**
Stop-and-start-a-new-recording remains the supported way to break up a session.

### Media-time consequence

None — because nothing changed. `crates/asr` already emits finals with media-relative
timestamps read directly from the recording (`ASR coordinates are media time`, per the comment
at `crates/app/src/session/mod.rs:1229`), and `MediaTimeMapping` is `IDENTITY` in every
production path (`crates/app/src/session/mod.rs:1238` and every other non-test call site — grep
confirms it). ADR-0018's identity — a citation at 02:18 addresses 02:18 of the recording exactly
— is untouched because it was never conditioned on wall clock in the first place. **ADR-0018 is
not amended**, because its stated invariant does not move.

### What was investigated before deciding

- **The writer has two independent clock domains, not one.** Video and system audio are
  timestamped and written by `RecordingWriter` in `crates/capture/bridge-macos/.../CaptureBridge.swift`
  using native `CMSampleBuffer.presentationTimeStamp` (ScreenCaptureKit's own host-time clock),
  with a per-track origin (`videoOrigin`, `systemAudioOriginNs`) latched once at the first sample
  and never revisited. The microphone path is timestamped by Rust's `SessionClock`
  (`crates/capture/src/macos.rs`, an `Instant`-based epoch) and forwarded across FFI as
  `stream_time_ns`. `screen.snapshot` timeline events also use `SessionClock`. These two domains
  are kept in sync today only because both start within milliseconds of each other and
  `MediaTimeMapping` papers over the residual skew as `IDENTITY`.
- **Option 1 (compress the timeline) is technically reachable without a `core` change**, by
  freezing `SessionClock` for the pause duration and, symmetrically, shifting the Swift-side
  per-track origins forward by the same accumulated pause length at resume, so both domains
  compress identically and `IDENTITY` keeps holding. That avoids touching `MediaTimeMapping`
  (a single affine map that cannot represent a pause on its own, and which is owned by `core` —
  out of this task's Owns list). But it requires new pause/resume behaviour on both sides of the
  FFI boundary, correct handling of `AVAssetWriterInput.expectsMediaDataInRealTime` across an
  arbitrary-length real-time gap, and coordinated resumption of the segment sink
  (`RecordingSegmentSink`) — exactly the territory where this capture path has already produced
  three distinct real-capture-only failures during T061 (`AVFoundationErrorDomain -11875`,
  `NSOSStatusErrorDomain -16341`, non-monotonic PTS rejection). None of those were found by a
  type-level test; all three needed a real signed capture to surface.
- **This implementer session cannot run a real signed capture.** There is no picker interaction,
  no TCC-granted Screen & System Audio Recording permission, and no existing harness in this repo
  that exercises `ScreenCaptureKit` outside a manual maintainer run (confirmed by grep: no
  `#[ignore]`/`SOTTO_REAL_CAPTURE`/signed-bundle test scaffolding exists anywhere under
  `crates/capture` or `crates/app`). The task board's standing pattern (T035, T064) is that real
  capture acceptance is exclusively a manual, signed-bundle, maintainer-run gate — never something
  an implementer agent exercises directly. The task's own verification rule requires pause
  behaviour to be "exercised against a real capture rather than simulated at the type level," and
  bars claiming coverage that doesn't exist. Building pause in this session would mean shipping
  code against exactly the kind of native, OS-adjacent surface the house verification rule exists
  to catch, with no way to satisfy that rule here.
- **The product is already moving toward multiple recordings per logical meeting, not one
  recording with a hole in it.** ADR-0021 (2026-08-14) explicitly frames a recurring meeting as
  linked entries rather than one entry/recording accumulating sessions, and T089 (blocked, N8
  wave) is building "record-again into an entry." Two honest recordings joined at the entry level
  is the direction the product is already headed, not a consolation prize.

### Why not option 2 either

Option 2 (preserve the wall clock with an explicit gap) has the same real-capture verification
problem as option 1 — it still needs the writer to survive an arbitrary real-time gap on
`AVAssetWriterInput` and prove the reader/transcriber/frame-decoder handle it — and it additionally
teaches every downstream reader about gaps for a lesser payoff (a wall-clock intuition, at the cost
of a recording that stores nothing useful for the paused interval). Neither of option 2's
advantages survive contact with "we cannot prove it against a real capture right now."

### What would change this

If a future task builds real-capture test infrastructure this agent doesn't have access to (a
signed dev harness that can drive `SCContentSharingPicker` non-interactively, or a maintainer
willing to run the manual acceptance pass), option 1 above is the buildable path: freeze
`SessionClock`, shift the two Swift-side per-track origins symmetrically at resume, and prove a
citation on either side of a pause boundary against the resulting file.

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

## Notes

Concluded option 3 — see `## Decision (2026-08-15)` above for the full reasoning. Nothing was
built. Changes made:

- `docs/design/workspace-v2-mock.html`: removed the `pauseBtn` button, its click handler, and the
  two shrink-order comments that promised it (capture-bar CSS comment and the HTML comment above
  `#captureBar`), replacing them with a short note pointing at this decision.
- `crates/app/src/workspace/layout.rs`: replaced the stale "T079 owns that decision" comment above
  the capture bar's control list with the resolved reasoning, so a future reader does not think the
  decision is still open. No behavioural change — the capture bar already shipped with no Pause
  control (removed 2026-08-14), and the existing tests
  (`a_wide_capture_bar_shows_every_control_it_can`, `a_narrow_capture_bar_keeps_the_clock_and_a_clickable_stop`)
  already assert `capture-pause-control` is absent at every width, so they continue to pass
  unchanged.
- `crates/capture/**` and ADR-0018: untouched. No pause path exists to suspend/resume, so there is
  nothing to build there and no invariant moved.

What I could not verify: nothing pause-related, because nothing pause-related was built. I did not
run a real capture in this session — see the decision section for why I judged this session
structurally unable to (no picker interaction, no TCC grant, no existing real-capture harness under
`crates/capture` or `crates/app`, consistent with T035/T064's standing manual-gate pattern).

Owns-boundary note: no edit was needed outside this task's Owns list. The one place I considered
touching outside it — `crates/core`'s `MediaTimeMapping`, which would have been necessary for
option 1's timeline compression to be *fully* general (it only handles a single affine segment,
not a pause) — turned out to have a lower-risk path (freezing `SessionClock` symmetrically, see
the decision) that would have stayed inside `crates/capture`. That path is recorded for whoever
picks this up next, but was not built now.

### Verification run

- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: clean.
- `git diff --check`: clean.
- `cargo fmt --all -- --check`: fails, but only on `crates/rag/src/lib.rs` — a file this task
  never touched, mid-edit by the concurrent agent working `crates/rag/**` per this board's
  parallelism rule. Nothing under this task's Owns list is unformatted.
- `cargo test --workspace --locked`: 5 failures, all in `crates/providers` (`codex::tests::*`:
  `dropping_consumer_interrupts_blocked_stdin_and_kills_process_group`,
  `cancellation_interrupts_a_prompt_larger_than_the_stdin_pipe`,
  `oversized_newline_free_event_fails_at_the_reader_limit`,
  `stderr_is_drained_before_stdin_and_to_eof_with_bounded_retention`,
  `timed_out_probe_kills_and_reaps_its_process_group`), all `Error: Elapsed(())` from real
  subprocess/process-group timing tests unrelated to capture or session code. Reproduced in
  isolation (`cargo test -p providers --lib codex::tests::timed_out_probe_kills_and_reaps_its_process_group`)
  — fails standalone too, so it is not cross-test interference. `crates/providers` is outside this
  task's Owns list and was not touched by this change; this reads as this sandbox's process/signal
  handling being unfriendly to real subprocess-group timing, not a regression from T079. Every
  test under this task's Owns list (`capture`, `app::session`, `app::workspace::layout`) passed.
