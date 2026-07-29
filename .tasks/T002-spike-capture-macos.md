# T002 — Spike A: dual-stream macOS audio capture + low-rate screen frames

**Status:** todo (unblocked — T001 approved)

**Wave:** 1 — start this one first, it has the longest tail and the highest technical risk

**Depends on:** T001 (needs `AudioFrame`, `CaptureBackend`, `CaptureError`)

**Owns:** `crates/capture/**` (including `crates/capture/bridge-macos/**`),
`docs/adr/0002-capture-architecture.md`

## Goal

Prove we can capture the rep (mic) and the customer (system audio) as two separate,
sample-accurate streams for a full sales call without drift or dropout. This is the
highest-risk item in the whole project: if dual-stream capture is unreliable, the
product does not exist. `AGENTS.md` Phase 0 gates on it.

## Plan

1. **Swift bridge package** at `crates/capture/bridge-macos/` — a Swift package built
   as a **static** library, exposing a C ABI (`@_cdecl`) surface:
   `sotto_capture_start(config, callback, ctx) -> handle`, `sotto_capture_stop(handle)`,
   `sotto_capture_permission_status() -> i32`, `sotto_capture_request_permission()`.
   Audio is delivered by callback into a lock-free ring, never allocating on the audio
   thread. Keep the FFI surface this small — everything else stays in Rust.

2. **System audio** via `SCStream` (ScreenCaptureKit) with `capturesAudio = true` and
   `excludesCurrentProcessAudio = true`. On macOS 15+ prefer
   `SCContentSharingPicker`-free programmatic config against
   `SCShareableContent.current` displays.
   *Note for the implementer:* macOS 14.4+ also exposes `AudioHardwareTapCreate`
   (Core Audio taps) as an alternative to SCK for audio-only capture. That tradeoff has
   shifted — see step 2b: we now want frames from the same session, which argues for
   staying on SCK. If you still choose taps, you own a second capture mechanism for
   video. Record the decision as ADR-0002; either backend sits behind the same trait.

2b. **Screen frames from the same session.** The `AGENTS.md` reframe makes screen context
   a Phase 1 timeline producer, so this spike must now prove frames too: attach a video
   output to the *same* `SCStream` and deliver a frame every ~10s. One session, one
   permission prompt, one battery cost — a second session for video is the thing to
   avoid. Expose frames over the FFI boundary as raw buffers with timestamps; you deliver
   them, **T015 owns everything downstream** (change detection, OCR, storage). Agree that
   boundary with T015 before writing the bridge.

   Frame delivery must not perturb audio: audio continuity is the product-critical
   stream, and a stalled video consumer must never stall it. Verify in the soak.

3. **Mic** via `cpal` in Rust — do not route the mic through the Swift bridge unless the
   two clocks force it. Two independent devices means two independent clocks; see
   step 5.

4. **Rust FFI layer** at `crates/capture/src/macos.rs`: `build.rs` invokes
   `swift build -c release` (or `xcodebuild`) and emits `cargo:rustc-link-lib=static=…`
   plus the framework links (`ScreenCaptureKit`, `CoreMedia`, `AVFoundation`). Bridge
   the C callback into a `broadcast::Sender<AudioFrame>`. Implement `CaptureBackend`.
   The build must be reproducible from a clean checkout with no manual Xcode steps.

5. **Clock alignment.** Mic and system audio come from different clock domains and
   *will* drift over an hour. Stamp every frame with its device's own timestamp,
   resample both to a common 16 kHz mono (what whisper wants) and measure drift
   explicitly rather than assuming. If drift exceeds ~1 sample per second, implement
   correction against the monotonic host clock. **Report the measured drift rate in the
   task notes** — the ASR and prosody stages need to know whether cross-stream
   timestamps can be compared directly.

6. **Permission flow.** `SCShareableContent` throws when Screen & System Audio Recording
   permission is missing. Handle: never granted, granted-then-revoked mid-call, and TCC
   reset. Expose `PermissionStatus` and a re-grant path that opens the right System
   Settings pane. Revocation mid-call must surface a `CaptureError`, not a silent
   dead stream.

7. **Soak test.** A binary (`crates/capture/examples/soak.rs`) that captures both audio
   streams to two WAV files **and frames to PNGs** for 60+ minutes. Verify afterwards: no
   dropped callback ranges, no drift beyond the step-5 budget, streams aligned, files
   independently playable and containing the expected voices, and frames landing at the
   expected cadence with timestamps that line up against the audio. Run it against a real
   Zoom or Meet call.

8. **Signing.** Capture bugs on unsigned builds waste days — the soak binary must be
   signed and notarized. Coordinate with T010, which owns the CI signing pipeline;
   for local runs an ad-hoc signed build with the right entitlements is enough.

## Contract for downstream tasks

`capture::macos::MacCapture: CaptureBackend` emitting 16 kHz mono `f32` `AudioFrame`s
on two `Source` values, plus raw frames handed to T015. T005/T006/T009 code against the
trait and their own WAV
fixtures, so they are not blocked by this task's completion.

## Acceptance

- 60-minute soak on a real call: no dropouts, drift within budget, both WAVs correct and
  in sync, and frame PNGs on disk at the expected cadence and timestamps.
- Frame delivery demonstrably does not perturb audio continuity.
- Permission revoked mid-call produces a `CaptureError` within one second.
- `cargo build -p capture` works from a clean clone with only Xcode CLT + Swift installed.
- Measured drift rate and the SCK-vs-CoreAudio-tap decision written up in ADR-0002.

## Out of scope

Windows/WASAPI (Phase 5 — but keep the trait boundary honest so it drops in later),
OCR and everything downstream of frame delivery (T015), video *recording*.
