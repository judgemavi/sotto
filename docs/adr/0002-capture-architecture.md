# ADR-0002: One ScreenCaptureKit session for system audio and screen frames

- Status: Accepted for spike validation
- Date: 2026-07-29
- Decision owners: Sotto maintainers

## Context

Sotto needs customer/system audio and low-rate screen context under one macOS permission
grant, while microphone audio remains a distinct speaker stream. Core Audio process taps
are a viable system-audio alternative on macOS 14.4+, but they would require a second
ScreenCaptureKit session for frames. Two sessions add permission, lifecycle, battery, and
timestamp-alignment failure modes to the highest-risk product boundary.

Native callbacks must never allocate or wait. Video is expendable under load; audio is
not. Swift/CoreVideo object ownership also must not escape across the C ABI.

## Decision

Use one `SCStream` for system audio and screen output. Exclude Sotto's own audio for
application/window filters, request 48 kHz mono system audio, and sample BGRA frames
approximately every ten seconds. Display-filter exclusion is still awaiting an isolated
signed-device experiment: current code permits self-audio there because excluding it coincided
with silent display buffers. Capture the microphone independently through CPAL so speaker
identity remains intrinsic.

Both audio callbacks copy into a shared bounded, preallocated lock-free packet pool.
Downmixing, 16 kHz resampling, `Arc` creation, and broadcast happen on a Rust worker.
Each packet carries the callback-time `Instant`, device/stream offset, source, and
sequence. The soak harness reports delivered sample rate for both streams and their relative
drift. Its original timestamp-only metric could not detect packet-boundary loss on the
host-derived mic clock. A stateful resampler now carries fractional phase across packets;
short runs measured the corrected mic near its expected rate, but only the pending signed
60-minute run can decide whether system-audio correction is required. Correction will be added
if relative drift exceeds one sample per second (62.5 ppm at 16 kHz); no short-run result is
accepted as that verdict.

The raw-frame C callback exposes callback-lifetime-only BGRA8888 bytes plus width,
height, authoritative row stride, FourCC pixel format, SCStream nanoseconds, and host
nanoseconds. Rust immediately copies into one of three preallocated slots. A full pool
drops the newest frame and increments a counter. Consumers explicitly recycle slots.
Frame delivery uses a separate native queue and never shares or blocks audio queues.
T015 owns change detection, OCR, encoding/storage, and timeline event production.

Swift owns an actor-serialized lifecycle. The C stop entry point is non-blocking: a stop
requested while asynchronous start is suspended is remembered, and the actor stops the
stream immediately when start resumes. Swift emits an explicit stopped status only after
`stopCapture` completes. Rust transfers its callback context to this lifecycle and frees
it only on that stopped callback, so neither a start failure nor an immediate stop can
callback through freed memory.

Rust exposes `Starting`, `Running`, `Stopping`, `Stopped`, and `Failed` as a broadcast
status plus an atomic snapshot. `Running` is emitted only after `SCStream.startCapture`
succeeds; consent UI must use this status rather than the synchronous trait return.
The independent error/status callback maps unexpected stream stop to permission
revocation so the application can surface it immediately.

Permission status uses `CGPreflightScreenCaptureAccess` without persisted inference:
authorized maps to authorized and every non-authorized result conservatively maps to
denied because macOS exposes neither not-determined nor restricted separately. The real
`SCShareableContent` acquisition during start is the authoritative probe and reports a
typed permission error if preflight remains false. Re-grant opens the Screen & System
Audio Recording System Settings pane.

Every stream starts from an `SCContentSharingPicker`-produced filter. A window run established
that another process's audible probe was excluded while the selected application's audio was
present, so application scoping exists. Window-level audio granularity is not established:
another window or browser tab in the same application may still be audible. Application and
display picks, and the current self-audio behavior for display capture, still require the
signed acceptance run before product copy may make stronger claims.

System audio is decoded using the `CMAudioFormatDescription` supplied with each sample buffer.
Float32 interleaved and non-interleaved layouts are handled explicitly. The metadata-free audio
callback is invoked only for the requested actual 48 kHz rate; another rate cannot be mislabeled
48 kHz on the Rust side. An invalid sample, absent description, unsupported or unreadable layout,
or unexpected rate terminates with `Failed` plus a typed stream error; it cannot masquerade as a
healthy silent customer track.

## Amendment: microphone-only sessions (2026-08-13)

T050 adds one explicit exception to “every stream starts from the system picker.” A
microphone-only session records `TargetKind::Microphone` as its durable capture scope and starts
the existing CPAL input directly. It does not present `SCContentSharingPicker`, construct an
`SCContentFilter` or `SCStream`, query Screen & System Audio Recording permission, capture
application audio, or emit screen frames. Microphone permission remains owned by CPAL/macOS and a
denial fails the start as a microphone-device error.

The retained recording is an audio-only MP4. Its stereo layout is deliberate: channel 0 contains
the microphone and channel 1 is silence. The microphone-only recording reader consumes channel 0
once and labels it `Source::Mic`; the normal scoped-session contract remains channel 0 = meeting
audio and channel 1 = microphone. This avoids both an accidentally silent primary channel and a
duplicate System/Mic transcript. The finalized-recording probe requires playable audio and reports
video timestamps as absent when the file legitimately has no video track.

Single-stream prosody is well-defined rather than synthesized: talk-time ratio becomes 1.0 after
speech is observed, and interruption output requires an observed other source, so none is emitted
for microphone-only input. The running indicator states `screen: off` and `application audio: off`;
it never reuses the system-wide-audio warning from a display-scoped session.

## Consequences

One capture session minimizes TCC and battery complexity and aligns screen timestamps
with system audio. CPAL and ScreenCaptureKit still use separate clock domains, so the
signed real-call soak remains the authority on drift and correction. The frame pool
reserves up to 120 MiB to guarantee callback-time copies do not allocate; a later task
may right-size slots from the selected display dimensions before capture.

The C header is the ownership contract. No Apple framework object crosses into Rust,
and a slow OCR consumer loses frames rather than degrading the product-critical audio.

## Revisit if

- The 60-minute signed soak shows ScreenCaptureKit audio dropout or drift beyond the
  correction budget.
- Core Audio taps materially improve continuity or power enough to justify a second
  frame session.
- Measured display sizes make the fixed frame-slot reservation unacceptable.
- Apple adds a stable permission API that distinguishes not-determined/restricted from
  denied.

## Pending signed-device evidence runbook

This runbook is an acceptance contract, not a record of execution. No picker, recording,
permission mutation, signing, notarization, or release action was performed in the 2026-08-11
automated audit.

1. Build the release soak example, assemble `Sotto.app`, and sign with a stable Developer ID
   identity using T010's `scripts/dev-bundle.sh`. Archive and notarize it with
   `scripts/notarize.sh`; retain the binary hash plus `codesign`, `stapler validate`, and `spctl`
   output. An ad-hoc signature is useful for development but does not satisfy this gate.
2. Start the notarized bundle with the soak duration set to 3,900 seconds. Pick the real call
   window through the system picker, keep target audio active for the full run, place a spoken
   alignment marker near the start and again after 60 minutes, and keep a visibly unrelated
   window outside the selected scope. Do not attach raw audio or unredacted frames to the task.
3. Require two independently playable finalized WAVs, zero harness-lagged audio packets, no
   callback dropout range, relative drift at or below 62.5 ppm, audible marker alignment, and
   full-resolution frames at the expected roughly ten-second cadence with no out-of-scope pixels.
   Record dropped-frame count and memory/thermal observations. If drift exceeds the threshold,
   T002 remains open for host-clock correction and a repeated soak.
4. In short separate signed runs, verify picker cancellation, a selected window that is not
   frontmost, application and display picks, a same-application second-window audio probe, an
   unrelated-process audio probe, and whether a display pick captures Sotto's own output. These
   observations decide truthful `audio_scoped` copy; never reconstruct or post-filter the OS
   picker scope to force a desired answer.
5. In separate runs, close the selected target, invoke macOS Stop Sharing, and revoke Screen &
   System Audio Recording permission while Running. Require distinct `TargetEnded`, `UserStopped`,
   and typed permission failure outcomes; revocation must be visible within one second and none
   may leave the app reporting Running.
6. Send SIGINT once during a disposable run and verify the harness reports interruption while
   finalizing both WAV headers and draining the PNG writer. This is a manual confirmation of the
   deterministic shutdown tests, not a substitute for the uninterrupted long soak.
