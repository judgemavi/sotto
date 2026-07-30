# T021 — Scoped capture target selection

**Status:** open — unblocks T012's start/stop flow

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
