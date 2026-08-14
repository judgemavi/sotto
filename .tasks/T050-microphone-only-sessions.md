# T050 — Microphone-only sessions

**Status:** in-review

**Wave:** M3 — capture

**Depends on:** T061. T057 closed on 2026-08-13 and released `crates/app/src/session/**`, but
`crates/capture/**` passed to T061 for the fragmentation and durability work. T021 and T032 are accepted history.

**Note (planner, 2026-08-13):** under ADR-0018 a microphone-only session still produces a recording,
audio-only with no video track. Fold that into this task rather than treating recording as
scoped-capture-only.

**Owns:** `crates/capture/**`, the capture-target type in `crates/core/src/types.rs`
(planner-amended core ownership — see below), `crates/app/src/session/**`, the capture copy in
`crates/app/src/settings/mod.rs`, `docs/adr/0002-capture-architecture.md` for an amendment section,
and this task

## Why it is blocked

T061 holds `crates/capture/**` while it settles fragmentation and the durability guarantee.
Splitting one capture change across two owners is exactly what the parallelism rule exists to
prevent. Wait for that release, then take the whole path — and inherit the recording writer rather
than adding a second one.

## Why this exists

Every session today begins with `SCContentSharingPicker`, because every session was assumed to be a
call. A user recording only themselves — a voice note after a meeting, a thought before one — has to
pick a window they do not care about, and then gets a session whose audio scope carries the
system-wide caveat for no reason.

Microphone-only is a strictly smaller capture path, not a larger one: no `SCContentSharingPicker`,
no `SCContentFilter`, no ScreenCaptureKit at all. `cpal` mic frames enter the existing pipeline
unchanged. It removes code from the hot path rather than adding it, and it makes the honest-scope
story *stronger*, because in this mode nothing but the microphone is captured and the UI can say so
without qualification.

## Core amendment

`core` is frozen by T001 and a task must normally stop rather than edit it. The planner grants this
task ownership of the capture-target type specifically, on the same basis as T018: the type cannot
express a real product state. Widen the recorded target to distinguish a chosen application or
window (bundle id, window title) from a microphone-only session, keep it exhaustively matched, and
report back rather than widening anything else in `core`.

## Plan

1. Replace the capture target on the session record with a closed enum covering the application or
   window case and the microphone-only case. A session record without a scope is not reproducible
   (AGENTS.md), so microphone-only is an explicit recorded scope, never an absent one.
2. Add the microphone-only start path to the capture trait and its macOS implementation: acquire the
   mic through the existing `cpal` route, skip picker presentation and content-filter construction
   entirely, and emit exactly one audio stream.
3. Make the single-stream case explicit downstream rather than accidental. The two-stream assumption
   (mic = local participant, target = remote meeting audio) is load-bearing for speaker attribution;
   in this mode there is one speaker label and no remote stream. Audit every consumer that assumes
   two streams — prosody talk-time ratios and interruption deltas in particular — and make each
   either handle one stream or state that it produced nothing and why.
4. Surface the mode in `session/**` as a distinct start command, and make the running-session
   indicator state the real scope: screen off, application audio not captured, microphone on. Do not
   reuse the system-wide-audio caution copy here; it is false in this mode.
5. Permissions: this path needs microphone access and must not request or trip Screen & System Audio
   Recording. Verify that a fresh profile with screen recording denied can still run a
   microphone-only session end to end.
6. Notes and reasoning consumers receive a session whose capture target names no application. Check
   the prompt construction path treats that as a fact to state rather than a field to omit.

## Contract

- `crates/app/src/workspace/library.rs` (T049) renders the control; this task provides the session
  controller method it calls.
- The widened capture-target type is the one every later consumer matches on. T053 and T054 read it
  when describing a session's provenance.

## Acceptance

- Starting a microphone-only session presents no system picker and constructs no `SCContentFilter`.
- With screen recording permission denied, a microphone-only session still records, transcribes, and
  persists a complete timeline.
- The session record round-trips through persistence with the microphone-only scope intact and
  replays to the same timeline.
- Per the verification rule, at least one test runs real recorded speech through the microphone-only
  path and asserts known transcript content — not synthetic frames through a structural assertion.
- The running-session indicator states the real scope, and no consumer that assumed two streams
  panics, silently drops output, or reports an empty result without a cause on stderr.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Windows capture, mixing microphone-only with an application target mid-session, speaker diarization
within the single stream, and any UI beyond the session controls and indicator copy.

## Unblocked — planner, 2026-08-13

`crates/capture/**` is released. T061's implementation work is finished; its only remaining
obligations are the abrupt-loss and disk-full runs, which are manual and now held by T064. T058 has
closed, so `crates/app/src/session/**` is free as well. T062 is `in-review` and holds
`crates/app/src/workspace/**` plus `crates/app/src/settings/mod.rs` — coordinate with the planner
before touching those, and stop rather than editing them.

Under ADR-0018 a microphone-only session still produces a recording, audio-only with no video track.
Two consequences to handle rather than discover: the recording probe decodes a video frame to
establish playability, so it must tolerate a recording that legitimately has no video; and T061's
stereo layout puts meeting audio on channel 0 and the microphone on channel 1, so a mic-only session
must decide explicitly what channel 0 carries rather than leaving it silent by accident.

## Implementation and evidence — 2026-08-13

The implementation slice is complete:

- `TargetKind::Microphone` plus the canonical `CaptureTarget::microphone_only()` records the scope
  explicitly. Invalid microphone-scope field combinations are rejected before persistence. SQLite
  schema v10 admits the new closed kind; the v9 → v10 migration preserves existing sessions and
  foreign-key-linked records. The task's core path had drifted from `types.rs` to
  `timeline/session.rs`; the planner approved that exact target-type edit plus the coupled core/RAG
  schema and persistence work.
- `MacCapture::microphone_only()` implements `CaptureBackend` through CPAL while the native bridge
  constructs no picker, `SCContentFilter`, or `SCStream`. It does not query Screen & System Audio
  Recording permission. The same recording writer produces an audio-only MP4.
- The audio-only layout is explicit: microphone samples occupy channel 0 and channel 1 is silence.
  A narrow planner-approved `RecordingConfig::microphone_only()` seam reads channel 0 once as
  `Source::Mic` and discards channel 1, preventing duplicate System/Mic transcript rows while
  preserving the normal scoped-session left=meeting/right=mic contract.
- The finalized recording probe now requires playable audio but represents both video timestamps
  as absent when there is legitimately no video track. The native regression exercises the
  production probe on a real audio-only MP4.
- `SessionController::start_microphone_only()` enters the existing lifecycle without presenting the
  picker. Its non-disableable indicator says `mic: microphone · screen: off · application audio:
  off`. The planner granted one exhaustive handoff in T062-owned `workspace/layout.rs` for the new
  `screen off` target kind; no other workspace/settings code was changed.
- Single-stream prosody needs no special fabricated event: only mic frames enter the pipeline,
  talk-time ratio becomes 1.0 once speech exists, and interruption requires another observed
  source, so no empty/false remote result is produced.

Automated evidence:

- `swift test --package-path crates/capture/bridge-macos`: **6 passed**, including explicit
  microphone-only channel 0 and audio-only production-probe coverage. Existing Swift deprecation
  warnings remain; there were no test failures.
- Elevated `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test -p core -p rag -p asr --all-targets
  --locked`: **core 39 passed; RAG 30 passed (one ignored performance test); ASR 23 passed (one
  ignored 148 MB model test)**. The first sandboxed ASR run had eight loopback tests fail with
  `Operation not permitted`; the required elevated rerun passed.
- Elevated `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test -p app --lib session:: --locked`: **28
  passed**, 93 filtered out.
- Elevated strict focused Clippy for core/capture/ASR/RAG/app over all targets with `-D warnings`:
  **passed**.
- `cargo fmt --all -- --check` and the T050-owned scoped `git diff --check`: **passed**.
- Full `cargo test --workspace --all-targets --locked`: **NOT COMPLETED**. The elevated command
  produced no captured test output for 444.8 seconds and was intentionally interrupted rather than
  allowed to block indefinitely. A subsequent process audit found no remaining matching Cargo,
  test-binary, or Swift process; no unrelated process was stopped. Focused gates above are the
  completed evidence, not a claim that the full workspace gate passed.

Still **NOT RUN** and required before acceptance can move from `in-review` to `done`:

- a signed/fresh-profile microphone capture with Screen & System Audio Recording denied, proving
  no picker or screen permission prompt appears and the audio-only recording, transcript, and
  persisted timeline complete end to end;
- known real speech recorded through the physical microphone-only path with an asserted transcript.
  The automated environment cannot supply known physical-microphone speech or authorize/deny TCC
  on a fresh signed profile, so fixture audio would not truthfully satisfy this gate;
- manual verification of the mounted start control supplied by T049/T062 and its running copy.
