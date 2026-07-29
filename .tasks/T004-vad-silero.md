# T004 — VAD crate: Silero via ONNX Runtime

**Status:** todo (unblocked — T014 frozen)

**Wave:** 1 — fully parallel, no platform dependencies

**Depends on:** T001 (`AudioFrame`, `VadSegment`) · T014 (`TimelineEvent`, `EventPayload::Vad`)

**Owns:** `crates/vad/**`

## Goal

Per-frame speech/silence detection on both streams independently. VAD is what makes the
rest of the pipeline cheap: it gates ASR work, and its speech-end events are the primary
trigger for "the customer stopped talking, suggest something now" — which sits directly
on the ~1s latency budget.

## Plan

1. Add `ort` (ONNX Runtime) to `crates/vad/Cargo.toml`. Prefer the bundled/downloaded
   runtime over a system dependency so a clean clone builds. Note the binary size cost
   in the task notes — footprint is a product claim.

2. Vendor the Silero VAD ONNX model. It is small (~1–2 MB), so bundling it in the
   binary via `include_bytes!` is acceptable and avoids a first-run download — unlike
   the Whisper weights, which must be downloaded. Record the model version and source
   URL in the crate docs.

3. Implement `SileroVad: VoiceActivityDetector`. Silero expects 16 kHz mono in fixed
   chunks (512 samples / 32 ms). Own an internal accumulator so callers can push
   arbitrary `AudioFrame` sizes — never require the caller to pre-chunk.

4. **One instance per stream.** Silero is stateful (recurrent); mic and system audio
   each need their own instance and their own hidden state. Make this impossible to get
   wrong in the API — the constructor takes a `Source`, and `reset()` clears state.

5. **Hysteresis.** Raw per-frame probability is noisy. Implement configurable
   `speech_threshold`, `silence_threshold` (lower, for hysteresis),
   `min_speech_duration`, and `min_silence_duration` before emitting
   `SpeechStart`/`SpeechEnd`. Defaults tuned for conversational speech: roughly
   250 ms min speech, 400–700 ms min silence. **Expose the silence threshold as
   config** — it is the single knob that trades suggestion latency against firing on
   a mid-sentence breath, and later tuning tasks will want it.

6. **Latency.** Report the frame-to-decision delay. `min_silence_duration` is a direct
   tax on the end-to-end budget, so measure it rather than estimating.

7. Tests against short WAV fixtures committed under `crates/vad/tests/fixtures/`
   (keep them small — a few seconds each): clean speech, speech with pauses, silence,
   music/hold-tone, and overlapping crosstalk. Assert segment boundaries within
   tolerance. Add a criterion bench for per-frame cost; VAD runs on every frame of two
   streams continuously, so per-frame cost matters more than it looks.

## Contract for downstream tasks

`vad::SileroVad::new(source, VadConfig)` implementing `VoiceActivityDetector`.
Consumers (T011 pipeline wiring) rely on `SpeechEnd` being emitted at most
`min_silence_duration` after true speech end.

## Acceptance

- Segment boundaries within ±100 ms of hand-labelled fixtures.
- Per-frame processing well under real-time on one core for both streams.
- No allocation in the steady-state `push()` path.
- `cargo test -p vad` green, clippy clean.

## Out of scope

Speaker diarization (we get speaker identity for free from stream separation — mic is
the rep, system is the customer), noise suppression, ASR.
