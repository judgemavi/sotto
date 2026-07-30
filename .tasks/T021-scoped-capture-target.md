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
