# T059 — Extract a frame from the recording, on demand only

**Status:** done

**Wave:** M4 — recording

**Depends on:** T057 (`done`) for the recording; T058 for media-time transcripts, now implemented.
ADR-0018 is accepted.

**Owns:** `crates/screen/**`, the inspection path in `crates/providers/**` where `inspect_screen`
is served, `crates/app/src/workspace/transcript.rs` for the user-facing gesture, and this task

**Concurrency (planner, 2026-08-13):** T058 and T060 run alongside this.

- `crates/app/src/session/**` is T058's. Read the session's recording reference through the
  existing `crates/core/src/types.rs` type; do not write session code.
- `crates/capture/**` is T061's. The recording is an input you decode, never one you change.
- `crates/app/src/workspace/mod.rs`, `layout.rs` and `library.rs` are T060's. If the transcript
  gesture needs a new workspace module registered, **stop and report** rather than editing `mod.rs`.
  `transcript.rs` itself is yours.
- `crates/providers/src/lib.rs` is shared surface. Add the inspection path as its own module and
  keep the edit to `lib.rs` to module registration only.

**Evidence note:** your acceptance requires decoding a real recording at a known timestamp and
asserting against known on-screen content. T061's signed capture runs have not happened yet, so
that recording does not exist during construction. Build to the point where the only thing missing
is the media, state plainly that the real-decode case is NOT RUN, and do not substitute a synthetic
frame for it — the verification rule forbids exactly that swap.

## Goal

Serve the existing `inspect_screen(timestamp | event_id)` contract by decoding a single frame from
the session recording at the requested moment, and give the user the same capability directly.

Nothing here runs by default. No frame is decoded, no OCR is executed, and no image is prepared
unless a reasoning pass or the user explicitly asks for that moment.

## Why this replaces the change-frame store

`crates/screen` built a bounded content-addressed frame store under ADR-0006. It was never wired
into the product, and ADR-0018 replaces the mechanism: frames come from the recording instead of
being retained separately. The inspection contract, its provenance requirements, and its
transcript-first ruling are unchanged — only the source of the pixels changes.

## Plan

1. Decode one frame from the recording at a requested media timestamp. Return the frame together
   with the provenance the contract already requires: requested moment, actual decoded timestamp,
   the recording it came from, and precision.
2. Never present a decoded frame as more exact than it is. If the nearest decodable frame differs
   from the requested moment, say by how much. A frame silently presented as the cited instant is
   worse than no frame.
3. Report missing media honestly. A pruned or deleted recording yields the explicit missing state
   the contract already defines, not an error the caller must interpret.
4. Run Apple Vision OCR locally only after a frame has been explicitly requested, exactly as
   ADR-0009 requires. OCR output is derived, cited to the frame and its timestamp.
5. Give the user the same gesture: from a transcript row, show what was on screen at that moment.
   This is the capability that motivated retention and it should not be reachable only by the model.
6. Keep the image disclosure boundary intact: an image may proceed toward a reasoning backend only
   under the separate explicit opt-in that already exists. Local extraction and local OCR are not
   that opt-in.

## Contract

- Callers receive frames with provenance and precision, or an explicit missing state. They never
  receive a frame implying an exactness the decode cannot support.

## Acceptance

- Requesting a moment returns the frame at that moment from the recording, with the actual decoded
  timestamp reported alongside the requested one.
- A pruned or deleted recording returns the explicit missing state, and the transcript remains fully
  reviewable.
- OCR runs only after an explicit inspection request, verified by a test asserting no OCR on the
  default path.
- A user can open the screen at a transcript row's moment without any reasoning backend configured.
- No image reaches a reasoning backend without the existing separate opt-in, asserted over the
  serialized request.
- Per the verification rule, a real recording is decoded at a known timestamp and asserted against
  known on-screen content — not a structural assertion over a synthetic frame.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Retaining separate frames, continuous OCR, video playback in the app, object or face detection, and
changing the image disclosure opt-in.

## Implementation evidence (2026-08-13)

- Added a macOS AVFoundation bridge that decodes one PNG from the retained MP4 only when called,
  returning both requested media time and the actual decoded presentation timestamp. The local
  provenance carries the recording session id and a signed decode offset; it never calls the result
  an exact frame.
- Added explicit `Deleted`, `Pruned`, missing-file, invalid-mapping, unsupported-platform, and decode
  failure states. Contract tests cover media-time mapping plus deleted/pruned behavior without
  substituting fixture bytes for real-media evidence.
- Added the provider-owned recording inspection path. Resolving an event selector and constructing
  the inspector perform no decode or OCR. Local OCR runs only for an explicit OCR request. The
  recording path returns a local-only type with no conversion to `AuthorizedReasoningImage`, so the
  existing separately consented provider image boundary remains the only transport input.
- Closed the reasoning seam directly: `RecordingBackedScreenInspector` implements the existing
  `ScreenInspectionSource` consumed by `complete_with_optional_inspection`. Its available result has
  recording-native provenance (requested media time, actual decoded media time, signed offset, and
  recording session id), not fabricated snapshot ids or visibility intervals. The provider-owned
  inspection module re-exports this implementation for runtime assembly.
- A reasoning integration test drives `event_id -> utterance media time -> recording decoder ->
  local OCR -> second reasoning request` and asserts the second request contains the honest
  requested/decoded coordinates and OCR text but no recording path. A completed first pass proves
  that neither decode nor OCR runs by default.
- Recording-backed image inspection remains fail-closed. Denied policy returns
  `image_opt_in_required` before decode; allowed policy still returns the explicit
  `recording_image_authorization_unavailable` state because the existing authorization token is
  snapshot-shaped. No snapshot provenance is invented and no image reaches transport.
- Added `Show screen` on ordinary transcript rows. It works without a reasoning backend, loads the
  selected session's durable recording reference, decodes that row's media timestamp, and opens the
  staged local PNG. Deleted/pruned/missing recordings leave the transcript intact and surface an
  explicit message.
- `cargo test -p screen --lib`: PASS (12 passed).
- `cargo test -p providers --lib inspection::tests`: PASS (2 passed).
- `cargo test -p insight`: PASS (35 passed across unit and integration suites).
- `cargo test -p app workspace::transcript::tests`: PASS (7 passed).
- Strict Clippy passes for `screen --all-targets --all-features`, `providers --lib`, and
  `app --all-targets --all-features`; `cargo fmt --all -- --check` and `cargo check -p app` pass.
- Coordinated integration gates pass: `cargo test --workspace --locked` and strict
  `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`. The full
  providers suite passed in that approved environment; live-provider tests remain ignored/manual.
- Real signed-session recording decode at a known on-screen moment: **NOT RUN**. T061 has not yet
  produced the required real recording, and no synthetic frame is claimed as a substitute.

## Blocked in practice on the live path — 2026-08-13

The first real run could not exercise `Show screen` at all. Every row reported "This meeting has no
retained screen recording", because `show_screen_at_impl` resolves through
`store.load_recording(session_id)` and that row is not written until the session stops
(`crates/app/src/session/mod.rs:1074`). The decoder was never reached.

This is a session-persistence defect, not a decoder defect, and it sits outside this task's
ownership. Filed as **T062**. This task's own real-decode acceptance remains NOT RUN and should be
run against a *stopped* meeting, which is the path that can work today.

## Closed with narrowed acceptance — planner, 2026-08-13

Accepted on the owned slice: the on-demand decoder with honest requested/actual timestamps, the
explicit deleted/pruned/missing/invalid-mapping states, local-only OCR gated behind an explicit
request, the fail-closed image transport boundary, and the `Show screen` gesture.

Residuals with explicit owners:

- **Real-recording decode at a known timestamp against known on-screen content** moves to **T064**.
- **Reaching the decoder at all** moves to **T062**. The gesture cannot resolve a recording today
  because the reference is written only after a successful finalization, so this task's decoder has
  never actually run in the product. That is a session-persistence defect, not a decoder defect.
