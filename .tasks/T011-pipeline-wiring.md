# T011 — Core pipeline: wire the conveyor belt end to end

**Status:** changes-requested (implementation good; collapse the duplicate CLI wiring)

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

## Review round 1 — changes requested (one structural item; the code itself is good)

Verified independently: fmt clean, strict **full-workspace** clippy clean (your report cited
core/prosody/rag only — the whole workspace does pass), 67 passed / 0 failed / 6 ignored.

The implementation is careful work and several things are exactly right:

- **Layering holds.** `PersistenceSink` is a `BoxFuture`-returning trait in `core`, implemented
  in `rag` as `TimelinePersistence` via `spawn_blocking` — so blocking SQLite never occupies a
  runtime worker, and `core` gained no dependency on `rag` or `providers`. The tier boundary in
  `AGENTS.md` survives, and the dyn-compatible trait shape carries the T007 lesson forward.
- **Differentiated drop policy, documented per channel.** Audio drops oldest because stale
  frames must never stall capture; finals await capacity and are never discarded. That is the
  right asymmetry and the module doc states it plainly rather than leaving it to be inferred.
- **Bounded under sustained failure, not just under load.** `pending_persistence_events`
  caps retained payloads while storage is failing — the case that actually leaks in production.
- **Shutdown is ordered producer-to-consumer** so each consumer sees EOF only after its
  producers drain. Easy to get wrong, and the reasoning is written down.
- Counters are exposed per stage rather than aggregated, so a lagging stage is attributable.

### R1. There are now two pipelines, and the wrong one is the tested one

`crates/cli/src/pipeline.rs` (411 lines) wires its own `Session`, `TimelineBuilder`,
`source_order` and event ordering, and `crates/cli` does not reference `core::pipeline` at all.
So:

- **`core::pipeline` has never run with real components.** Its fixture test feeds genuine WAVs
  through *test doubles* for VAD and ASR. That is a fine unit boundary in isolation, but it
  means the production pipeline has never seen Silero, Whisper, or Vision.
- **The path that has been exercised end to end is the CLI's** — that is what produced the 10
  VAD events and `fixtures/timelines/call-01.jsonl`. So the committed reference timeline
  validates the CLI's ordering, not `core`'s.
- **The two orderings already differ.** The CLI sorts frames by
  `(stream_offset, source_order, seq)`; `core` merges utterances by start, end, final-before-partial,
  system-before-mic, event id. Two implementations of "ordering" that no test compares will
  drift, and a fix applied to one will silently not apply to the other.
- The concurrency properties this task exists to guarantee — no stage stalling another — are
  not exercised by the CLI at all, since it runs sequentially.

By the verification rule in `.tasks/README.md` this is the fifth instance of the pattern: an
artifact with a green suite that has not been run against real inputs.

**Fix:** make `cli::run_files` a thin adapter over `core::pipeline`. The CLI keeps what is
genuinely its own — WAV decoding, frame loading, JSONL emission, latency percentiles, stderr
warnings — and delegates stage wiring, session/timeline construction, merge ordering and
persistence to `core`. Then regenerate `fixtures/timelines/call-01.jsonl` from that path, so the
reference timeline validates the pipeline we actually ship.

**This one is my fault, not yours.** T009's brief explicitly told it to build stub-tolerant
wiring of its own so the harness could exist before the stage crates landed — and I never wrote
the follow-up step to collapse it into `core::pipeline` once T011 arrived. The duplication is a
planning gap, not an implementation error.

### Re-review

CLI delegating to `core::pipeline`, one ordering implementation, reference timeline regenerated
from the real path, and `sotto-cli run` still emitting a correct timeline against the fixtures.
