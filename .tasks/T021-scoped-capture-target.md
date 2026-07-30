# T021 — Scoped capture target selection

**Status:** in-progress (implementation reviewed and accepted; dev app bundle and the manual
acceptance run remain)

**Wave:** Phase 2 — this is now the critical path to a usable tool

**Depends on:** T002 (the bridge, the lifecycle actor, the frame pool and the resampler all
exist and work; this adds target selection to them)

**Owns:** `crates/capture/**` — including `bridge-macos/Sources/SottoCaptureBridge/CaptureBridge.swift`,
`bridge-macos/include/SottoCaptureBridge.h`, and `src/macos.rs`

T002 keeps only its outstanding manual soak measurement, which edits no files. Ownership of the
crate transfers here to keep the parallelism rule intact.

## Why this exists

T012 stopped rather than wire the start button, and it stopped for the right reason.
`CaptureBridge.swift:73` builds

```swift
let filter = SCContentFilter(display: display, excludingApplications: [], exceptingWindows: [])
```

That is the whole display and all system audio. `sotto_capture_start` accepts no target, and the
crate exposes no way to name one. Wiring "Turn on and choose target…" to that path would put a
button labelled *choose target* in front of unscoped whole-machine capture.

`AGENTS.md` says consent is structural, not a policy — the OS content filter is what makes the
promise true, not a sentence in the settings pane. So the promise cannot be kept until the filter
comes from a user's picker selection. Everything downstream of capture is done and waiting on
this one interface.

## Plan

1. **Present `SCContentSharingPicker`.** macOS 14+, so it is inside the deployment target already
   pinned in `.cargo/config.toml`. Configure it to allow single-window, single-application and
   display selection. The user picks; the OS hands back an `SCContentFilter`. There is no code
   path that constructs a filter without going through the picker.

2. **Retain the chosen filter as an opaque handle.** Add to the C header, roughly:

   ```c
   void sotto_capture_pick_target(sotto_target_callback on_choice, void *context);
   void *sotto_capture_start_with_target(const sotto_capture_config *config, void *target);
   void sotto_capture_release_target(void *target);
   ```

   `on_choice` fires with the handle plus the description below, or with null when the user
   cancels. Cancellation is a normal outcome, not an error.

3. **Return a description the UI can be truthful with.** The callback carries exactly the fields
   `sotto_core::CaptureTarget` already has: `bundle_id`, `display_name`, `window_title`, `kind`,
   and `audio_scoped`. Strings are UTF-8, valid only for the callback, copied on the Rust side.

4. **`audio_scoped` is the bridge's answer, never the app's.** A window or application filter
   scopes audio to that application; a display filter does not. `CaptureIndicator` already has a
   test asserting the indicator does not claim scoping it does not have, and that test is only
   honest if this flag comes from the filter that is actually running. Set it from the filter kind
   at the point the filter is created.

5. **Delete the unscoped path.** `sotto_capture_start` as it stands should stop existing, not be
   left as a fallback that a future caller reaches for when the picker is inconvenient. If a
   display-wide capture is ever wanted, it is a display the user selected in the picker, which is
   the same code path.

6. **Handle revocation and target disappearance.** The window the user picked can close mid
   session. That is a stop with a reason, surfaced through the existing `CaptureStatus` broadcast
   rather than a silent halt — the timeline should record that capture ended and why.

7. **Rust surface.** `MacCapture::pick_target() -> Option<PickedTarget>` and
   `MacCapture::start(&PickedTarget)`. `PickedTarget` owns the handle and releases it on drop.
   Keep the existing `CaptureLifecycle` actor: it already gets the start/stop races right, and
   this changes what it starts, not how.

## Risk worth knowing about before you start

`SCContentSharingPicker` is presented by the system on behalf of a running application, and a
bare `cargo run` binary is not always a well-formed app for that purpose. If the picker will not
present from the dev binary, say so early — the fix is a bundle, and it is better to find that
out on day one than after the API work is done.

## Acceptance

- The picker presents, and picking a specific window captures that window and that
  application's audio — verified by capturing a window that is *not* frontmost and confirming
  what lands in the frames.
- Nothing on screen outside the chosen target appears in any frame. Check with a second window
  visibly on screen showing content that must not be captured.
- Cancelling the picker leaves no session started and no error state.
- `audio_scoped` is false for a display selection and true for a window or application
  selection, proven against a real capture, not asserted.
- Closing the captured window ends the session with a status the app can render.
- No code path can start a capture without a picker-produced filter.

## Out of scope

The settings UI that calls this (T012), the recording indicator (exists), device enumeration
for the microphone (T012), browser-tab-level scoping (`AGENTS.md` non-goal — ScreenCaptureKit
sees windows).

## Review round 1 — two must-fix, then this is ready for the manual run

The shape is right. The picker is the only producer of a filter, `sotto_capture_start` is gone
rather than left as a fallback, `PickedTarget` owns the retained handle and releases it on drop,
frame sizing now comes from `filter.contentRect * pointPixelScale` instead of the display, and
`error_callback` now clears `running` on every terminal path — that last one was a real bug
nobody asked you to find.

The macOS 15.2 availability claim checks out: `includedWindows`, `includedApplications` and
`includedDisplays` are all `API_AVAILABLE(macos(15.2))` in the SDK. But note what that means in
practice — the deployment target is 14.0, this machine runs macOS 26, so `#available` passes at
runtime and real metadata *is* used here. Generic labels only affect users on 14.0–15.1. If the
manual run shows "Selected window", that is a symptom of something else, not the expected result.

### Must fix 1 — the fallback branch fabricates a display target

`describe` maps both `.none` and `@unknown default` to `kind: 3` with `audioScoped: false`, so an
unrecognised filter style is recorded in the timeline as *the user picked a display*, and capture
starts from a filter whose scope we do not know. That is the hole this task exists to close,
reintroduced through the branch nobody looks at.

The Rust side already handles this correctly — an unknown kind releases the handle and returns
`None` — but Swift never sends an unknown kind, so that guard is dead code. Send `0` for `.none`
and `@unknown default` and the existing guard does exactly the right thing. Refusing to start is
the only truthful response to a filter we cannot describe.

### Must fix 2 — the type system should carry the guarantee, not an error string

```rust
impl CaptureBackend for MacCapture {
    fn start(&mut self, sink: …) -> Result<(), CaptureError> {
        drop(sink);
        Err(CaptureError::Unsupported("… requires a target returned by pick_target"))
    }
```

`Pipeline::capture(value: impl CaptureBackend)` will accept `MacCapture` happily, compile, and
fail at runtime with a string. Acceptance here says *no code path can start a capture without a
picker-produced filter*; right now the FFI enforces that and the Rust API does not.

Don't implement `CaptureBackend` for `MacCapture`. Implement it for a value that can only be
constructed from a `PickedTarget` — then "started without a picker selection" is a program that
does not compile, which is the same argument as `PickedTarget` being unconstructible from Rust.
I checked: nothing consumes this today (the CLI uses `FileCapture` and `MergedFileCapture`), so
it is cheap now and expensive once the pipeline is wired.

### Should fix

- **`TargetPicker.finish` races.** `finished` is a plain `var`, and the callbacks are not
  documented as arriving on one queue. Two of them racing gives a double
  `Unmanaged.passUnretained(self).release()`, which is a crash. You already solved this exact
  problem correctly one type over with `terminalLock` in `reportTerminal` — do the same here.
- **`[-3815, -3817, -3821]` as bare integers.** Use `SCStreamError.Code.noCaptureSource`,
  `.userStopped`, `.systemStoppedStream`. And `.userStopped` is the user hitting *Stop Sharing* —
  a deliberate action, not the target disappearing. It probably deserves its own status; a user
  who stopped sharing should not be told their window vanished.
- **`pick_target()` needs a running AppKit main loop.** It awaits a oneshot fed from
  `Task { @MainActor }`. The soak example builds a current-thread tokio runtime and `block_on`s
  it, which is precisely the case where that task may never run — so `cargo run --example soak`
  may hang at the picker instead of presenting it. Check this first; it is the same question as
  the bundle risk and the answer shapes the manual run.
- **`start_with_target` → `start_scoped` → `start_inner`** is three hops for one call, and
  `start_scoped`'s null check is unreachable — `PickedTarget` cannot hold a null handle.
- **`rag::store` maps unknown kind strings to `TargetKind::Window`.** Now that the kind carries a
  claim about scope, an unrecognised value should be an error rather than silently becoming a
  window.

### On the core change

Adding `TargetKind::Display` was the right call — the picker offers display selection and the
enum could not express it, so the alternative was recording a display as a window. It is
additive, it round-trips through SQLite, and the exhaustive matches were updated.

But `crates/core` was frozen at T014 and is not in this task's `Owns` list. Changing it silently
inside a task scoped to `crates/capture/**` is how the parallelism rule stops working. Making the
change: right. Not saying so until the summary: not. Flag a frozen-contract change before you
make it, so the planner can check nothing else in flight depends on the old shape.

### Verification

`--all-features` was omitted again, on both clippy and test. I ran the full form: clean, 78
passed / 0 failed / 7 ignored. Please use it — serde-gated code, including the `CaptureTarget`
serialization you just changed the shape of, is invisible without it.

## Review round 2 — code accepted; only the manual run and the dev bundle remain

Both must-fix items are properly resolved, and the second one is resolved the strong way.
`MacCapture::new` is private, the `Default` impl is gone, `PickedMacCapture` is the only type
implementing `CaptureBackend`, and it can only be reached through
`PickedTarget::into_capture`. `Pipeline::capture(MacCapture)` no longer compiles rather than
failing at runtime — the guarantee now lives in the type system, which is where it belongs.
`.none` and `@unknown default` send kind 0, so the Rust rejection path is live instead of dead.

The should-fix list is done too: `finishLock`, named `SCStreamError.Code` cases, a distinct
`UserStopped` status, one start path instead of three, and `parse_target_kind` rejecting
unknown persisted scopes with a test that says why.

Verified: clippy `--all-targets --all-features` clean, 79 passed / 0 failed / 7 ignored, Swift
release build clean.

### One unannounced change, and it matters for the manual run

The `SCFrameStatus == .stopped` check in `deliverFrame` was deleted. It is not in the review
list and was not mentioned, but it is the **right** deletion — `reportTerminal` latches the
first code it sees, so a `.stopped` frame arriving ahead of `didStopWithError` would report −5
and permanently mask the −6 that was just added, making every deliberate Stop Sharing look like
the target had vanished. Keeping both paths would have defeated the distinction.

The consequence is what to carry into the manual run: **target disappearance now has exactly one
detection path**, `didStopWithError` with `.noCaptureSource` or `.systemStoppedStream`, and that
path has never executed. Closing the picked window and confirming `TargetEnded` actually arrives
is now a load-bearing check, not a formality. If ScreenCaptureKit simply stops delivering frames
without raising an error, capture sits in `Running` forever and the session never ends — which
is the failure mode a user would experience as "it silently stopped recording".

Next time, say when you remove something. The reasoning was good; it should not have taken a
diff read to find it.

### Minor

`MacCapture` is still `pub` with no way to construct one, so its inherent instance methods
(`subscribe_errors`, `status`, `take_frame_receiver`, `dropped_frame_count`) are unreachable
public surface, duplicated on `PickedMacCapture`. Make the type private and keep it as a
namespace for the associated functions, or move those onto `PickedMacCapture`.

### Still open

The dev app bundle. My round-2 brief left it out, so its absence is not on you — but it is what
stands between this code and its acceptance run. Screen-recording permission is granted per code
identity, so an unsigned `cargo run` binary reads as a new app to TCC after every rebuild, and
`SCContentSharingPicker` may not present from one at all. `scripts/Info.plist` already carries
both usage descriptions.

## The dev bundle is done (2026-07-30, by the planner)

`scripts/dev-bundle.sh` assembles and signs `target/Sotto.app` around any built binary,
defaulting to `target/release/app`. It reads `CFBundleExecutable` and `CFBundleIdentifier`
from the existing `scripts/Info.plist` and renames the binary to match, so the plist stays
the single source of identity. Signing is delegated to `scripts/sign.sh`, ad-hoc unless
`SOTTO_DEV_IDENTITY` names a certificate. Verified: `codesign -dv` reports
`Identifier=com.sotto.app`, format `app bundle`, hardened runtime on, and the bundle
satisfies its designated requirement. Documented under "Local development bundle" in
`docs/signing.md`.

Ad-hoc signing means TCC keys on the code hash, so each rebuild is a new identity and needs
a fresh Screen & System Audio Recording grant. A self-signed code-signing certificate in the
login keychain fixes that, and the script says so when it falls back to ad-hoc.

The acceptance run is now unblocked:

```sh
cargo build --release -p capture --example soak
scripts/dev-bundle.sh target/release/examples/soak
./target/Sotto.app/Contents/MacOS/sotto 40
```

Frames land in `./capture-soak-output/` at one per ten seconds, alongside `mic.wav` and
`system.wav`. Everything the acceptance criteria ask for is visible in those artifacts:
whether a non-frontmost window captured cleanly, whether anything outside the target leaked
into a frame, and — by closing the picked window mid-run — whether `TargetEnded` actually
arrives on the single detection path that now carries it.

## The main-run-loop deadlock, found and fixed (2026-07-30, by the planner)

The first bundled run hung with no output. Sampled rather than guessed — the main thread was
parked inside tokio:

```
DispatchQueue_1: com.apple.main-thread
  tokio::runtime::Runtime::block_on … capture::macos::MacCapture::pick_target
    current_thread::Context::park
```

`sotto_capture_pick_target` schedules `Task { @MainActor }` to present the picker, and the soak
example was blocking the main thread awaiting the result. The thread that would deliver the
choice was the thread waiting for it. This is the risk this task listed up front; it turned out
to be about the event loop, not the bundle.

`pick_target()` stays async and is correct for the GPUI app, which runs an AppKit event loop.
Added `sotto_capture_pump_main_loop` and `MacCapture::pick_target_blocking()` for processes with
no loop of their own, and documented on `pick_target` why awaiting it from such a process
deadlocks. The soak example now uses the blocking form.

### What the first successful run proved

```
selected capture target: CaptureTarget { bundle_id: Some("com.apple.Terminal"),
  display_name: "Terminal", window_title: Some("sotto — -zsh — 88×39"),
  kind: Window, audio_scoped: true }
captured 1 frames; 0 frames dropped
```

The picker presents from the bundle, TCC accepts the bundle identity, and the macOS 15.2
metadata path is live on this machine — real bundle id and window title, not the generic
fallback labels. That settles the availability question in the round-1 review.

### What it did not prove, and one number to watch

Eight seconds is not a drift measurement. That run reported system audio at +13374 ppm against
mic at −81 ppm — about 48 s of slip per hour if it were real. It almost certainly is not; a run
that short is dominated by startup transients, and the mic figure looks healthy. But it is the
same quantity T002's deferred long-run check exists to measure, and it is the one that would
silently ruin an hour-long transcript by pulling the two streams apart. Measure it properly on
the real acceptance run rather than dismissing it.

Still outstanding, and all needing a person: capturing a non-frontmost window, confirming
nothing outside the target leaks into a frame, and closing the picked window to prove
`TargetEnded` arrives on its single untested path.

## Capture evidence from the first real runs (2026-07-30)

Measured from `capture-soak-output/` rather than inferred. Chrome playing a YouTube video was
the target; a 1 kHz tone at −26 dBFS played from `afplay`, a separate process, throughout.

**Screen scoping holds.** A captured frame contains the picked Chrome window and nothing else —
no desktop, no other windows, no menu bar — at 3190×2168, the window's backing resolution,
confirming the `contentRect * pointPixelScale` sizing. The BGRA→RGBA swizzle in the soak
harness is correct.

**Audio appears scoped to the picked application.** Window capture: YouTube present at −17 dBFS,
99.8% non-zero samples, and the 1 kHz probe *absent* — 0.5 dB below its neighbouring bins, so no
peak. A different process's audio was excluded while the target application's was captured.

One control still missing before this is proof rather than strong evidence: nothing has shown
the tone is capturable at all. Capturing an application that is itself playing the tone (a
window-bearing one, since the picker cannot select `afplay`) would close it.

**Defect: a display pick captures no audio.** Three runs, all with audio playing, all returning
0.0% non-zero samples — 318,080 samples of exact zero. Packets are delivered, so the stream is
producing silent buffers rather than failing. A user who picks a display gets a silent track,
no error, and a recording indicator saying "audio: system" while nothing is recorded.

`audio_scoped: false` is technically accurate and practically misleading here: it reads as
"audio is not restricted to one app", not "there is no audio". Either make display capture
deliver system audio, or make the absence explicit in the target description so the indicator
can say so. Silently recording zeros is the worst of the three.

**Drift is not yet measurable.** Relative drift across short runs: +13374 ppm (8 s), −728 ppm
(40 s), −221 ppm (40 s), −651 ppm (20 s), −1170 ppm (20 s), −507 ppm (20 s). The sign flips and
the magnitude tracks run length, which is the signature of startup transients rather than a
rate. Nothing under ten minutes will answer this.

Still untested: capturing a window that is not frontmost, and closing the picked window to prove
`TargetEnded` arrives on the single path that now carries it.

### Control closed: the probe was audible

The tone was confirmed audible during playback, so `afplay` was genuinely rendering to the
output device. That removes the one alternative explanation for its absence from the window
capture. Audio scoping for a window pick is established, not merely indicated: a separate
process's audio was excluded while the picked application's was captured.

## Window-close detection verified (2026-07-30)

```
[   0.0s] capture status: Starting
[   0.1s] capture status: Running
[  28.3s] capture error: capture stream failed: selected capture target disappeared
[  28.3s] capture status: TargetEnded
capture ended early after 28.3s: TargetEnded
```

Closing the captured window ends the session with a status the app can render. The single
detection path left after the `SCFrameStatus` removal — `didStopWithError` with
`.noCaptureSource` or `.systemStoppedStream` — does fire. I expected this to be the check that
failed; it is not.

### But a closed window is not an error

`TargetEnded` emits both a `CaptureStatus` and a `CaptureError::StreamFailed`. `UserStopped`,
added in the same round, correctly emits only a status. The two should agree, and `UserStopped`
has it right.

A user closing the window they were capturing is a normal way for a session to end — as normal
as pressing stop. Raising `capture stream failed` for it means T012's settings surface shows an
error banner for an outcome the user deliberately caused, in a UI whose whole job is to
distinguish real failures (bad key, locked keychain, network down) from ordinary states. Drop
the `error_sink` send on `-5` and let the status carry it, exactly as `-6` does.

## Acceptance status

| Criterion | State |
|---|---|
| Picker presents; window captures | verified |
| Nothing outside the target in any frame | verified from captured PNG |
| Cancelling leaves no session and no error | verified |
| `audio_scoped` true for a window pick | **proven** — a separate process's audio excluded while the target's was captured |
| `audio_scoped` false for a display pick | **defect** — no audio at all, not unscoped audio |
| Closing the window ends the session | verified |
| No path starts capture without a picker filter | verified by construction |

Remaining: the display-audio defect, the `TargetEnded` error/status inconsistency, a capture of
a window that is confirmed not frontmost, and the ten-minute drift measurement that also
retires T002's deferred soak.

### Diagnostic added for the display-audio defect (planner, uncoordinated — check before editing)

`describe` now dumps the picker's filter under `SOTTO_CAPTURE_DEBUG=1`: style, contentRect,
scale, and the counts of included applications/windows/displays. Run a display pick and a window
pick and compare.

Hypothesis it tests: ScreenCaptureKit mixes audio from the applications *named in the filter*.
That fits every measurement — a window filter names Chrome, so Chrome's audio arrived and a
separate process's did not. If a picker-produced display filter names zero applications, packets
of silence with no error is exactly what you would expect.

If confirmed, the fix is uncomfortable: naming applications means assembling the filter from
`SCShareableContent` ourselves rather than using the one the picker returned, which erodes the
guarantee this task exists to enforce. Preference order:

1. Report display audio as unavailable in the target description, so the indicator can say
   "audio: none" rather than implying system audio. Keeps the guarantee; costs a capability
   nobody has asked for, since the product case is a call in a window.
2. Fix it within the picker's own output if a configuration knob exists. Best outcome.
3. Reconstruct the filter ourselves. Restores the capability, weakens the guarantee — needs a
   strong reason.

Recommendation is 1 unless the diagnostic points at 2. Silently recording zeros is the only
outcome that is actually unacceptable; the absence of whole-display audio is not.

## Review — display audio fixed, but two changes were tested as one

The audio extraction rewrite is a real correctness fix and almost certainly the one that
mattered. The old path took `length / MemoryLayout<Float>.stride` samples straight out of
`CMSampleBuffer.dataBuffer`, which only works if ScreenCaptureKit hands back contiguous
interleaved Float32. `withAudioBufferList` + `AVAudioPCMBuffer.floatChannelData` reads what the
buffer list actually describes, and the multi-channel downmix is right. Dropping the
`CaptureError` from `TargetEnded` is also correct — it now matches `UserStopped`.

Verified here: fmt clean, clippy `--all-targets --all-features` clean, 79 passed / 0 failed /
7 ignored.

### `excludesCurrentProcessAudio = target.audioScoped` is not established, and it has a cost

Two things changed and one experiment ran, so we cannot say which fixed the silence. The comment
hedges — *"it **can** result in silent audio buffers"* — which is the honest phrasing for
something not tested in isolation. Apple's sample project shows a configuration; it is not
evidence about the failure we hit.

The cost is real. On a display pick this now records **our own process's audio**. Sotto emits
none today, so nothing is visibly wrong. The first time it plays anything — a notification, a
readback, any TTS — it captures itself, feeds that to ASR, and puts its own output on the
timeline as if it were the conversation.

The two concepts are also unrelated. `audio_scoped` describes whether audio is limited to the
target application; `excludesCurrentProcessAudio` describes whether *we* are recorded. Coupling
them means a later change to one silently changes the other.

**Run the one experiment that separates them:** keep the extraction fix, set
`excludesCurrentProcessAudio = true` unconditionally, and do a display capture. If audio still
flows, delete the coupling — we get the fix without recording ourselves. If it goes silent, the
coupling is earned; keep it with a comment saying it was measured, and note the feedback risk
for when Sotto gains a voice.

### `try?` swallows exactly the failure we just spent a day chasing

```swift
try? sampleBuffer.withAudioBufferList { … }
```

If extraction throws, audio stops with no error, no status, no log — silent buffers with a
healthy-looking packet rate, which is the precise shape of the bug just fixed. Report it through
the existing error callback, or at minimum behind `SOTTO_CAPTURE_DEBUG`.

Related: `AVAudioFormat(standardFormatWithSampleRate:channels:)` is non-interleaved by
definition, and `floatChannelData` misreads an interleaved buffer list. Guard on
`kAudioFormatFlagIsNonInterleaved` in the format description so a layout change fails loudly
rather than producing plausible garbage.

### Two evidence gaps

- **The scoping proof was measured on the old extraction path.** Filtering happens in
  ScreenCaptureKit, not in our extraction, so the conclusion almost certainly survives — but the
  code it was measured against no longer exists. Re-run the 1 kHz probe once against a window
  pick; it is a few minutes and it keeps the strongest claim in this task honest.
- **The `SOTTO_CAPTURE_DEBUG` diagnostic was never run.** Its `apps=N` counts would have said
  directly whether display filters name zero applications, which is the mechanism question. Still
  worth one run, together with the untested **application-style** pick — `audio_scoped: true` is
  claimed for that kind on the strength of the window result generalising, which is an assumption.

### Process note (planner's error)

Commit `7c7e726`, titled "Add a filter diagnostic", also contains this audio work and the
`TargetEnded` change. I ran `git add -A` while the agent was editing the same tree and swept its
in-flight changes into my commit. The code is right; the message is wrong. Left as-is rather than
rewriting history under a running agent. Use `git add <path>` in a shared tree.

## Product decision: keep all three picker modes (2026-07-30)

Window, application and display all stay in `allowedPickerModes`. Dropping `.singleDisplay`
would have deleted the self-recording risk and the unscoped-audio question in one line, but
whole-screen is a legitimate member of "turn Sotto on, then pick what it sees". The answer is to
make display capture honest, not to remove it.

Note the vocabulary gap this leaves. Users think in terms of *tab*, app or window; macOS offers
window, application or display. A browser tab is not a window, so picking "a tab" means picking
the browser window currently showing it. Video-wise the illusion mostly holds. If audio is scoped
per application, it breaks precisely where it matters — the user picks the window holding their
call and gets every other Chrome tab's audio too. That is what makes the granularity measurement
the first thing to run.
