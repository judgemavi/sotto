# T002 — Spike A: dual-stream macOS audio capture + low-rate screen frames

**Status:** in-progress

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
   `excludesCurrentProcessAudio = true`, against the content filter built in step 2a.
   *Note for the implementer:* macOS 14.4+ also exposes `AudioHardwareTapCreate`
   (Core Audio taps) as an alternative to SCK for audio-only capture. That tradeoff has
   shifted — see step 2b: we now want frames from the same session, which argues for
   staying on SCK. If you still choose taps, you own a second capture mechanism for
   video. Record the decision as ADR-0002; either backend sits behind the same trait.

2a. **Scoped capture — this reverses earlier guidance.** The brief previously said to
   prefer *"`SCContentSharingPicker`-free programmatic config"*. That is now wrong.
   `AGENTS.md` makes capture scope structural: a session begins with the user picking an
   application or window through **`SCContentSharingPicker`**, and that choice builds the
   `SCContentFilter` used for both audio and video.

   Use the system picker rather than building our own. It is the affordance users already
   know from screen sharing, and macOS draws its own indicator around the captured window —
   a consent signal we neither have to build nor have to be trusted to honour.

   Three things this spike must answer, because the product design depends on them:
   - **Can audio be scoped to the chosen application** on our minimum macOS version, or is
     SCK audio system-wide regardless of the filter? If it is system-wide, the scope
     guarantee is video-only and Slack pings and Spotify are in the recording. Report the
     honest answer in ADR-0002 — the UI has to tell the truth about this.
   - **What happens when the chosen window closes mid-session?** Quitting Zoom mid-call is
     ordinary. Define the behaviour (error, pause, fall back to the application) and make
     sure it is not a silent dead stream.
   - **What does the picker cost in the flow?** It is per-session UI on the critical path
     to recording; note how it behaves on re-start of a second session.

   Browser *tabs* are out of scope: ScreenCaptureKit sees windows, not tabs. Capturing a
   Meet tab means capturing the browser window. Do not attempt tab granularity.

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
   Zoom or Meet call, **with the target picked through the picker** — and confirm the
   captured frames contain only that target, not the whole desktop.

8. **Signing.** Capture bugs on unsigned builds waste days — the soak binary must be
   signed and notarized. Coordinate with T010, which owns the CI signing pipeline;
   for local runs an ad-hoc signed build with the right entitlements is enough.

## Contract for downstream tasks

`capture::macos::MacCapture: CaptureBackend` emitting 16 kHz mono `f32` `AudioFrame`s
on two `Source` values, plus raw frames handed to T015. T005/T006/T009 code against the
trait and their own WAV
fixtures, so they are not blocked by this task's completion.

## Acceptance

- 60-minute soak on a real call, target chosen via `SCContentSharingPicker`: no dropouts,
  drift within budget, both WAVs correct and in sync, and frame PNGs on disk at the
  expected cadence and timestamps, containing only the chosen target.
- The three scoping questions in step 2a answered in ADR-0002 — especially whether audio
  can be scoped per-application.
- Frame delivery demonstrably does not perturb audio continuity.
- Permission revoked mid-call produces a `CaptureError` within one second.
- `cargo build -p capture` works from a clean clone with only Xcode CLT + Swift installed.
- Measured drift rate and the SCK-vs-CoreAudio-tap decision written up in ADR-0002.

## Out of scope

Windows/WASAPI (Phase 5 — but keep the trait boundary honest so it drops in later),
OCR and everything downstream of frame delivery (T015), video *recording*.

## Notes

- Built the static Swift ScreenCaptureKit bridge, CPAL microphone path, preallocated
  lock-free audio/frame handoffs, permission/error reporting, drift-reporting soak
  harness, and the T015 frame boundary agreed through the planner.
- Chose one SCK session over Core Audio taps because screen frames are now required from
  the same permission/session; ADR-0002 records the tradeoff and ownership contract.
- Swift release build, `cargo test -p capture --all-targets`, scoped strict Clippy, and
  formatting pass on this machine.
- No drift rate is claimed yet. The required 60+ minute signed Zoom/Meet soak needs an
  interactive real call, TCC grant, audible separated participants, and signing via
  `MACOS_SIGNING_IDENTITY=- scripts/sign.sh target/release/examples/soak`. The harness
  prints mic/system drift in samples per second and frame-drop count. Acceptance remains
  gated on recording those results here and adding correction if either clock exceeds
  one sample per second.
- The public macOS preflight API distinguishes authorized from non-authorized. A local
  probe conservatively maps non-authorized to denied; the real `SCShareableContent`
  acquisition is authoritative and reports typed denial. macOS does not expose
  not-determined or restricted independently.

## Review round 1 — changes requested

The Rust side is careful work: preallocated pools, lock-free `ArrayQueue` handoff, no
allocation in the audio callback, resampling moved to a worker thread that parks rather
than spins, `SAFETY` comments on every `unsafe` block, and video backpressure isolated
from audio exactly as the agreed FFI boundary specifies. Strict clippy is clean.

The problems are all on the Swift side of the boundary, and two of them are the kind that
present as a rare hang or a rare crash rather than a test failure — which is why they need
fixing before the 60-minute soak rather than after.

### R1. `sottoCaptureStop` blocks a thread waiting on an async Task

```swift
let stopped = DispatchSemaphore(value: 0)
Task { await box.session.stop(); stopped.signal() }
stopped.wait()
```

Blocking a thread on a semaphore until a `Task` completes is the documented way to
deadlock Swift concurrency: the `Task` needs a cooperative-pool thread to make progress,
and the blocked caller may be holding one. Under pool saturation this hangs rather than
stalls.

It matters here because `MacCapture::stop()` is called from `Drop`, so the hang lands on
app quit or on any error path that tears capture down — and quitting an app that is
recording a live call is the single most common thing a user will do with it.

Make the C entry point non-blocking and signal completion through the existing error/status
callback, or drive the stop on a dedicated non-cooperative queue. Do not keep the
semaphore.

### R2. Use-after-free window between failed start and stop

`sotto_capture_start` returns a handle immediately while a detached `Task` runs
`session.start()`. On failure that Task calls `config.error(config.context, -2)`. But
`config.context` is the Rust `Box<CallbackState>`, which `MacCapture::stop()` drops as
soon as `sotto_capture_stop` returns — and `stop` awaits `session.stop()`, not the
in-flight *start* Task. Sequence: start returns → caller stops immediately (user cancels,
error path, `Drop`) → Rust frees `CallbackState` → the start Task's `catch` fires → the
error callback writes through a dangling pointer.

Either have `stop()` await the start Task as well, or keep the context alive on the Swift
side until every Task that can touch it has completed. `error_callback` already null-checks
the context, which does not help — the pointer is non-null and freed.

### R3. `start()` returns `Ok(())` before capture is actually running

Same root cause: the real `session.start()` is asynchronous and its failure arrives later
on the error channel. So `MacCapture::start` reporting success means "the bridge accepted
the configuration", not "audio is being captured".

That is defensible as an internal detail, but it has a consent consequence that is not:
T012's recording indicator is driven by capture state, and `AGENTS.md` makes the visible
indicator a first-class consent feature. An indicator that says *recording* when
ScreenCaptureKit silently failed to start tells the user the opposite of the truth.

Give the trait a way to observe *running* as distinct from *started* — either have
`start()` await the real result, or expose a status the UI can subscribe to. Document
which, because T011 and T012 both key off it.

### R4. Permission status is inferred from a `UserDefaults` flag

`sottoCapturePermissionStatus` returns `Denied` only when a `SottoCapturePermissionRequested`
key is set, so a TCC reset, a new user account, or a cleared defaults domain reports
`NotDetermined` for a genuinely denied permission — and the re-grant flow the task
requires will guide the user nowhere. Prefer deriving the state from the API
(`CGPreflightScreenCaptureAccess` plus an actual `SCShareableContent` probe) over
remembering that we once asked.

## Still outstanding

The 60-minute signed soak on a real call, the measured drift rate, and ADR-0002's
SCK-vs-CoreAudio-tap decision are all still manual and still required. Do not run the soak
until R1–R3 are fixed — a hang or a dangling-pointer crash 40 minutes in is indistinguishable
from the drift and dropout bugs the soak exists to find.

## Environment update — Xcode installed (2026-07-29)

Xcode 26.6 (17F113) is now installed, so `xcrun notarytool` and `xcrun stapler` resolve
and the signed-and-notarized soak binary this task requires can actually be produced
locally via T010's `scripts/sign.sh` and `scripts/notarize.sh`.

This removes the last environmental excuse for deferring the soak — but the ordering in
the review above stands: **fix R1–R3 first.** The Swift-side deadlock and the
use-after-free window both surface as intermittent hangs and crashes, which is precisely
what a 60-minute soak looks like when it fails, and you will not be able to tell them
apart from the drift and dropout bugs the soak exists to find.

## Review round 1 resolution

- R1: removed the semaphore entirely. The C stop entry point now schedules actor-owned
  asynchronous shutdown and returns immediately.
- R2: the actor records stop intent during suspended start and orders shutdown after it.
  Rust transfers callback-context ownership and reclaims it only on the single stopped
  callback after `stopCapture` completes, including failed-start teardown.
- R3: added observable `CaptureStatus` transitions. `start()` means accepted/starting;
  `Running` is emitted only after ScreenCaptureKit confirms capture. T011/T012 must drive
  pipeline and consent UI from that status.
- R4: removed `UserDefaults` inference. Preflight is conservative; actual shareable-
  content acquisition is authoritative, permission failure is typed, and a System
  Settings re-grant function is exposed.
- Validated with Xcode 26.6 Swift debug/release builds, scoped strict Clippy, capture
  tests, formatting, and `cargo build -p capture`. The real-call soak was not run during
  this review round, as instructed.

## Review round 2 — fixes approved; scope work now added

All four review items verified fixed: the `DispatchSemaphore` is gone, a `CaptureLifecycle`
actor serialises start/stop so the callback context cannot be freed underneath an in-flight
Task, capture reports `Running` only after ScreenCaptureKit actually succeeds, and
permission state comes from `CGPreflightScreenCaptureAccess` rather than a `UserDefaults`
guess. Good work — those were the hard ones.

**But this task is not done, and it just grew.** `AGENTS.md` has changed: capture is now
explicitly scoped to a user-chosen application or window, which reverses this brief's
earlier guidance to avoid `SCContentSharingPicker`. See the new **step 2a** above. The
soak requirement is unchanged and still pending, but it should now run against a picked
target rather than the whole desktop — so do the scope work first and soak once.

The single most important thing you can tell us: **can ScreenCaptureKit scope audio to the
chosen application, or is it system-wide?** The product's consent story, T012's indicator,
T014's `audio_scoped` flag and T015's scope all hang off that answer. Report it plainly
either way; "video-only scoping" is a fine answer, and silently implying more is not.

## Field findings — 2026-07-29

Two defects found while trying to actually run the soak, plus first real drift data.

### F1. The soak binary would not launch (fixed)

```
dyld: Library not loaded: @rpath/libswift_Concurrency.dylib
      Reason: no LC_RPATH's found
```

`Package.swift` declares `.macOS(.v14)`, but rustc's link step does not inherit that
deployment target. The linker assumed an older minimum, resolved Swift concurrency
against the Swift 5.5 back-deployment stub, and emitted a reference to a dylib that does
not exist on macOS 14+ — where the concurrency runtime is folded into `libswiftCore`.

Fixed by adding `.cargo/config.toml` with `MACOSX_DEPLOYMENT_TARGET = "14.0"`, which
applies to every cargo-invoked link in the workspace, so `cli` and `app` are covered too.
**Keep it in step with `Package.swift`'s platforms declaration** — if one moves and the
other does not, this returns as a runtime failure that no test catches, because the
workspace compiles and lints perfectly clean either way. Worth a line in ADR-0002.

### F2. Ctrl-C loses the entire run

There is no signal handling. An interrupt kills the process before `mic.finalize()` and
`system.finalize()`, so the WAV headers never get their data length and both files are
unreadable — and the drift summary never prints. On a 65-minute soak that means one
mistimed keystroke costs the whole session. Add a handler that stops capture and
finalizes on SIGINT.

### F3. First drift measurements

| Window | Frames | Dropped | Mic drift | System drift |
|---|---|---|---|---|
| 3 s | 1 | 0 | 85.756 samples/s | 609.935 samples/s |
| 60 s | 4 | 0 | −1.035 samples/s | −4.191 samples/s |

The 3-second figures were startup transient — they fell ~100× as the window grew, so
short runs cannot be used to characterise drift at all. Zero dropped frames in both.

At 60 s the mic sits at the ~1 sample/s budget and system is 4× over it. The figure that
matters is **relative** drift, since cross-stream comparison is what T006's interruption
detection depends on:

```
4.191 − 1.035 = 3.16 samples/s → × 3900 s ÷ 16 kHz ≈ 0.77 s of slip per hour
```

Roughly 65 ppm on mic (normal for a crystal) and 262 ppm on system (high). If those rates
hold over the full soak, cross-stream timestamps cannot be compared directly and the
correction step this brief already anticipates is required, not optional. If instead the
rate keeps falling as the window grows, it was all startup transient.

**The 65-minute soak decides which.** Report the two rates, the dropped-frame count, and
whether a spoken marker at ~60 minutes still audibly lines up against continuous system
audio — the audible check is worth more than the printed number.

### F4. Frames were captured at 2×2 pixels (fixed)

`streamConfig.width/height` were hardcoded to `2`. ScreenCaptureKit requires a video
output even for audio-only capture, and 2×2 was the cheapest way to satisfy that while
this spike was audio-only — but screen context became a Phase 1 producer and the
placeholder was never revisited. Every captured PNG was 83 bytes of nothing.

Now sized from the display's backing resolution (`SCDisplay` points ×
`NSScreen.backingScaleFactor`), capped so a frame stays inside `MAX_FRAME_BYTES` so an
oversized display degrades resolution rather than being dropped. Verified: 3456×2234,
~1.8 MiB per frame.

**What this invalidated:** every prior "0 frames dropped" result. The frame path had never
been exercised — stride handling, pool pressure, backpressure isolation — because each
frame was 16 bytes. Treat all frame-related results before this as meaningless.

### F5. PNG encoding on the audio path starved the capture (fixed)

With real frames, `write_png` converts ~7.7M pixels inline on the same loop that drains
the audio broadcast. The receiver fell behind, and `while let Ok(..) = try_recv()`
silently swallowed `Lagged` — so dropped audio looked like drift. Measured rates blew out
to +12,805 ppm and nothing reported a problem.

Frame writing now runs on its own thread, and lag is counted and printed rather than
discarded. The bridge already isolates video backpressure from audio; the harness has to
do the same or it measures itself.

### F6. The resampler lost samples at every packet boundary (fixed — root cause of the drift)

`resample_linear` computed `input.len() * output_rate / input_rate` with **integer
division, per packet, with no phase carried across boundaries**. Any packet length that
was not an exact multiple of the ratio discarded its remainder. Mic packets are smaller
than system-audio packets, so the mic lost proportionally more — which is exactly the
asymmetry the numbers showed.

Replaced with a stateful `Resampler` that keeps the fractional read position and the
previous packet's trailing sample, making packet boundaries invisible. Two tests added:
ragged packet lengths must not lose samples, and one ramp split into ragged packets must
resample identically to the whole ramp.

| | before | after |
|---|---|---|
| mic delivered rate | 15936.71 Hz (−3,956 ppm) | 16000.45 Hz (**+28 ppm**) |

+28 ppm is ordinary crystal drift. The mic stream is now essentially exact.

### F7. Drift instrumentation was measuring the wrong thing (fixed)

The mic's `stream_offset` is derived from the host clock (`captured.duration_since(origin)`),
so `DriftTracker` was comparing the host clock against itself and reported ≈0 no matter
what the device did. It could not have detected F6 — a −3,956 ppm defect — and gave false
confidence instead.

Added `RateTracker`, which counts samples actually delivered per second of wall clock.
That works identically for both paths, and the **difference between the two streams** is
the number the product cares about, since cross-stream timestamp comparison is what
speaker attribution and interruption detection rest on. The soak now prints per-stream
ppm and the resulting seconds-of-slip-per-hour.

### Still open: the system stream's rate

Mic is settled. System audio reads between −110 and −442 ppm across short runs, which is
too noisy to characterise — and short runs cannot do it anyway, as F3 showed. It also only
means something when audio is genuinely playing for the whole window; SCK delivery with no
audio source is not a measurement.

**The 65-minute soak is still required**, now against a build where the numbers mean
something. Run it with continuous audio playing throughout and report per-stream ppm, the
relative figure, lag count, dropped frames, and whether a spoken marker at ~60 minutes
still audibly lines up.

### Deferred: long-run validation (open risk, deliberately accepted)

The 65-minute soak is **deferred, not cancelled.** Two questions remain unanswered and
they are now explicitly carried as risk rather than silently dropped:

1. **System-stream rate.** Reads −110 to −442 ppm across 30-second runs — too noisy to
   characterise, and short windows cannot do it. If real at ~400 ppm that is ~1.4 s of
   slip per hour, which breaks the cross-stream timestamp comparison T006's interruption
   detection and speaker attribution depend on.
2. **Stability over a call's length.** Dropout, thermal behaviour, memory growth and
   frame-pool pressure across an hour are all untested. Frames only became real in F4, so
   nothing has ever run long at full resolution.

**Cheaper substitute, do when convenient:** a **10-minute** run with audio playing
continuously start to finish. That is enough to separate real drift from measurement
noise (400 ppm over 600 s accumulates 0.24 s — unmistakable), at a sixth of the cost. Run
the full 65 later, unattended, when the machine is free.

Downstream tasks may proceed on fixtures meanwhile — T004, T005, T006, T008 and T009 do
not touch live capture. What they must **not** do is assume cross-stream timestamps are
directly comparable. Until the system rate is characterised, treat that as unproven:
T006 in particular should keep its interruption detection tolerant of a drift correction
being introduced later.

## Automated residual audit — 2026-08-11

Two deterministic gaps described earlier in this task were still present and are now fixed:

- System-audio extraction no longer uses `try?` or synthesizes a non-interleaved format. The
  bridge uses the buffer's `CMAudioFormatDescription`, converts both Float32 interleaved and
  non-interleaved layouts, and reports an unsupported/unreadable buffer as bridge code `-7`.
  Rust maps that to `CaptureStatus::Failed` plus a typed `CaptureError::StreamFailed`, with a
  regression proving it clears Running rather than writing a healthy-looking silent track.
- The soak harness now installs a SIGINT handler. A single interrupt exits through the ordinary
  drain/stop path, joins and drains the PNG writer, finalizes both WAV headers, and labels the
  evidence interrupted. Deterministic regressions cover the handler and independently readable
  finalized mic/system WAVs.

The task remains `in-progress`: no automated test establishes OS picker behavior, real-device
scope, signing/notarization identity, permission revocation latency, hour-long drift/dropout,
thermal/frame-pool stability, or audible alignment. ADR-0002 now contains the concrete signed
acceptance runbook and the 62.5 ppm relative-drift threshold. None of its manual steps ran in
this audit.

Focused verification:

- `swift build -c debug`: passed against Xcode 26.6/SDK 26.4; the existing AppKit main-actor
  diagnostics in the C run-loop pump remain warnings.
- `CARGO_TARGET_DIR=/tmp/sotto-t002-capture cargo test -p capture --all-targets
  --all-features --locked`: passed, capture library 5/5 and soak example 2/2.
- `CARGO_TARGET_DIR=/tmp/sotto-t002-capture cargo check -p capture --all-targets
  --all-features --locked`: passed.
- `CARGO_TARGET_DIR=/tmp/sotto-t002-capture cargo clippy -p capture --all-targets
  --all-features --locked -- -D warnings`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

## Independent deterministic-fix review — 2026-08-12

The automated residual slice is accepted after two additional fail-closed corrections:

- Audio output now reports bridge code `-7` for an invalid sample, missing actual format
  description, unsupported layout, zero channels, or an actual sample rate other than 48 kHz.
  The compact FFI does not carry sample-rate metadata, so accepting a different rate while Rust
  labels it 48 kHz would corrupt resampling and drift evidence. Invalid screen samples remain a
  permitted dropped frame. The C and Rust sides now document this boundary explicitly.
- Shutdown now requests capture stop before draining evidence. It continues bounded audio drains
  while waiting up to two seconds for `Stopped`, performs a final audio drain, then signals and
  joins the PNG writer for its final drain, and finally closes both WAV headers. This closes the
  prior producer-before-final-drain ordering bug. Because Swift stop is asynchronous, the bounded
  wait is intentionally not presented as proof of callback quiescence; the signed manual SIGINT
  run remains authoritative.

Independent verification passed: Swift debug build (with the already-recorded AppKit actor
warnings), capture library tests 5/5, soak example tests 2/2, strict all-target/all-feature capture
Clippy, package-scoped formatting, and scoped diff-check. No picker, real call, permission mutation,
signing/notarization, or long soak ran. Status remains `in-progress` pending the manual acceptance
contract above.
