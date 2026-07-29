# T011 — Core pipeline: wire the conveyor belt end to end

**Status:** blocked (on T009, T015, T018 — T004/T005/T006/T014 done)

**Wave:** 2

**Depends on:** T014 (timeline model) · T004 (VAD) · T005 (ASR) · T006 (prosody) ·
T015 (screen) · T009 (file-backed capture, so this is testable without hardware).
T002 is **not** a blocker — develop against file capture and swap in real capture when
it lands.

**Owns:** `crates/core/src/pipeline/**`, `crates/core/tests/**`

> Note: this task edits `crates/core`, which T014 re-froze. It may add new modules under
> `src/pipeline/` and extend `lib.rs` exports, but must **not** change `types.rs`,
> `traits.rs` or `timeline/`. A needed change there stops the task and comes back to the
> planner.

## Goal

Turn the independent stages into the running conveyor belt of `AGENTS.md`:
`AudioFrame → VadSegment → PartialTranscript → Trigger → Suggestion`, where every stage
is its own tokio task and no stage waits for the previous one to "finish" — with every
stage producing into the **session timeline** as the shared spine. Screen snapshots join
the same log, which is what makes the fused audio+screen artifact the product depends on.

## Plan

1. `Pipeline::builder()` taking a `CaptureBackend`, a `VoiceActivityDetector` per
   source, a `Transcriber`, and an `Annotator`, wiring them over the T001 broadcast bus.
   Stages are constructed from traits so tests substitute fakes freely.

   Note the deliberate split at the capture boundary: `CaptureBackend::start` takes a
   `broadcast::Sender<AudioFrame>`, **not** the timeline bus, so that ~100 frames/s across
   two streams never share a channel with the event log. Per T014, raw audio frames are
   **never** timeline events — they must not reach the log or SQLite. Your bridge task
   consumes frames and emits only the derived events (VAD, utterances, prosody).

2. One tokio task per stage, each subscribing upstream and publishing downstream.
   No stage ever blocks on a downstream consumer. Assert this structurally: a
   deliberately slow test subscriber must not affect throughput on the audio path.

3. **Backpressure and lag.** A `broadcast` receiver that falls behind gets `Lagged`.
   Policy per stage: the audio path drops oldest and increments a counter (a dropped
   frame is recoverable, a stalled pipeline is not); the transcript path must not drop
   finals. Document each stage's policy in the module docs and export the drop counters
   for the UI.

4. **Two sources, one pipeline.** Mic and system audio flow through parallel VAD and
   ASR paths and merge at the annotated-transcript stage into a single time-ordered
   conversation view. That merged view is what prompt assembly consumes — define its
   ordering rule explicitly, including how to order a partial from one speaker against
   a final from the other.

5. **Lifecycle.** Start, pause (consent: per-call on/off is a Phase 3 feature but the
   core hook belongs here), stop, and clean shutdown that drains in-flight work.
   Mid-call capture errors (T002's revoked-permission case) surface as
   `PipelineEvent::Error` and pause cleanly rather than tearing down.

6. **Session state.** A `CallSession` owning event-id allocation, the timeline so far,
   talk-time ratios, and the recent window used for prompt assembly. Bounded memory — a
   two-hour call must not grow without limit. Because the timeline is append-only and
   persisted by T008, in-memory state is a *window* over the log, not the log itself:
   evict old events from memory and let consumers that need history read them back from
   SQLite. Define and document that boundary — T016's board and T017's summarizer both
   depend on knowing what is live versus what must be loaded.

6b. **Timeline persistence wiring.** Feed the event stream to T008's `append_events` on a
   background task. A slow or failed write degrades the recording only — it must never
   apply backpressure to the live pipeline or drop a suggestion. Surface write failures
   as an error event so the UI can tell the user the call is not being recorded.

7. Integration tests over the T009 fixtures: full run produces the expected event
   sequence; latency assertions per `AGENTS.md` ("latency assertions in CI where
   feasible"); a chaos test where a stage errors mid-run and the pipeline recovers.

## Acceptance

- Fixture WAVs in → correct ordered event stream out, headless.
- Slow subscriber demonstrably cannot stall the audio path.
- Latency assertions run in CI.
- Memory bounded over a simulated two-hour session.

## Out of scope

Trigger classification and suggestions (T013), UI (T012).
