# T005 — ASR crate: whisper.cpp sliding-window streaming transcription

**Status:** todo (unblocked — T001 approved)

**Wave:** 1 — fully parallel; develop against WAV fixtures, not live capture

**Depends on:** T001 (`AudioFrame`, `Utterance`, `Transcriber`, `AsrError`)

**Owns:** `crates/asr/**`

## Goal

Turn audio into a stream of partial and final `Utterance`s with low enough latency that
the intelligence loop can start speculating while the customer is still speaking. This
is the heaviest compute in the product and shares a laptop with Zoom — efficiency is
a feature, not an optimisation.

## Plan

1. Add `whisper-rs` with the Metal feature to `crates/asr/Cargo.toml`. Confirm Metal is
   actually engaged at runtime (whisper.cpp logs the backend) — a silent CPU fallback
   will look like a mysterious latency regression later.

2. **Ring buffer** (`crates/asr/src/ring.rs`): fixed-capacity 16 kHz mono f32 buffer
   holding ~30 s, lock-free single-producer/single-consumer, no allocation in `push`.
   The audio callback pushes; the transcription task reads windows.

3. **Sliding window** (`AGENTS.md`): re-transcribe the last ~10 s every ~500 ms. Both
   values are config, not constants. Emit non-final `Utterance`s from each pass.

4. **Partial stabilisation** — the core difficulty of this task. Consecutive passes over
   overlapping audio produce differing text; naive emission makes the UI flicker and
   makes the watcher model re-fire on unchanged content. Implement:
   - a **commit point**: text older than the last ~2–3 s of the window that has agreed
     across N consecutive passes is emitted as final and dropped from re-transcription;
   - `is_final` set accordingly, with `(source, start)` as the supersede key defined in
     T001 so downstream stages can replace a partial rather than append;
   - a diff so identical consecutive partials are not re-emitted at all.

   Document the chosen agreement policy — T011 and the watcher loop depend on how often
   partials churn.

5. **VAD gating.** Do not run inference on silence. Accept `SpeechState` hints so the
   transcriber idles when neither party is talking. This is the main lever on average
   CPU. Keep it optional so the crate is testable standalone.

6. **Model lifecycle.** Lazy load, unload when idle past a timeout (`AGENTS.md` memory
   discipline). Do **not** bundle weights — the model file path is injected, and the
   download UI is a later Phase 3 task. Support base/small/medium selection; default to
   whatever hits the latency budget on Apple Silicon (likely `small.en` or `base.en`).

7. **Two streams, one model.** Mic and system audio both need transcription. Decide and
   document: one model instance with interleaved windows, or two instances. One
   instance is the memory-honest choice but needs a scheduling policy — and the
   customer's stream should win contention, since suggestions fire on customer speech.

8. Fixtures + tests under `crates/asr/tests/`: a short sales-call-like WAV with known
   transcript. Assert WER within tolerance and, more importantly, assert the
   **partial-stability property** — that finalised text never changes after commit.
   Bench: window latency and real-time factor on Apple Silicon; record both.

## Contract for downstream tasks

`asr::WhisperTranscriber: Transcriber`. T011 relies on: finals never retract, partials
supersede by `(source, start)`, and `poll()` never blocks.

## Acceptance

- Streaming a fixture end to end produces stable finals and non-retracting text.
- First partial for a phrase available within ~1 s of it being spoken.
- Real-time factor comfortably < 1.0 for both streams on Apple Silicon with Metal.
- Idle unload verified by RSS measurement.

## Out of scope

Speaker diarization, punctuation post-processing beyond what Whisper emits, translation,
the model-download UI (Phase 3), prosody annotations (T006).
