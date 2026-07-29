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

Use one `SCStream` for system audio and screen output. Exclude Sotto's own audio, request
48 kHz mono system audio, and sample BGRA frames approximately every ten seconds. Capture
the microphone independently through CPAL so speaker identity remains intrinsic.

Both audio callbacks copy into a shared bounded, preallocated lock-free packet pool.
Downmixing, 16 kHz resampling, `Arc` creation, and broadcast happen on a Rust worker.
Each packet carries the callback-time `Instant`, device/stream offset, source, and
sequence. The soak harness reports each clock's drift against monotonic host time in
samples per second. Drift correction will be enabled only if the real 60-minute result
exceeds one sample per second; no measurement has yet justified silently altering audio.

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
