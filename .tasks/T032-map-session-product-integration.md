# T032 — Map-tier session controls and Whisper provisioning integration

**Status:** done

**Wave:** M0b — first user-operable no-key session

**Depends on:** T012's explicit `settings/**` handoff; accepted T022 session controller; done T023
provisioner library; T033 reproducible Whisper binding build. Wait for T016's `lib.rs` handoff only
if module registration is required.

**Owns:** `crates/app/src/main.rs`, `crates/app/src/session/**`,
`crates/app/src/settings/**` after T012 hands it off; `crates/app/src/lib.rs` only for minimal
registration after T016 hands it off. No `devwindow/**`, `board/**`, ASR, or capture changes.

## Goal

Make the existing local map tier operable without developer environment variables or reasoning
credentials: the user explicitly presses Start, chooses one system-scoped target, watches managed
Whisper provisioning if needed, sees exactly what audio/screen capture is active, and can always
stop. Launching Sotto never opens the picker and never starts or resumes capture.

## Plan

1. Replace the launch-time `pick_and_run` call with one app-owned lifecycle state machine:
   `Idle`, `ChoosingTarget`, `ProvisioningModel`, `Running`, `Stopping`, and actionable `Error`.
   There is one controller and one timeline ingress, not a settings-side shadow session.
2. Wire the visible Start action to the system picker. Cancellation returns to `Idle` without a
   timeline, error, model download, or retained session. Only the OS-returned target enters T022.
3. Resolve the selected/default Whisper model through T023 before capture begins. Show honest
   resolving/downloading/verifying progress, support cancellation, reuse a verified cache offline,
   quarantine a corrupt managed final before attempting a verified replacement, and surface the
   reachable typed offline-no-cache or override failures with a retry or override action.
4. Pass the resolved model path into the live `AsrConfig`; retain `SOTTO_WHISPER_MODEL` as an
   explicit override, never as the ordinary product requirement. Do not start capture while model
   readiness is unresolved.
5. Show a non-disableable recording indicator while Running, naming the selected target and
   distinguishing screen scope from actual audio scope. Keep Stop one obvious action away during
   capture and provisioning; stopping never auto-restarts or reopens the picker.
6. Map T022 terminal outcomes truthfully (`Stopped`, target ended, system Stop Sharing, failure),
   await its bounded shutdown, persist/reload the tail, and return to `Idle` with the last outcome
   visible. Closing the target cannot leave a stale recording indicator.
7. Keep no reasoning a normal state. These controls, transcription, persistence, and the board
   must work with no Codex installation and no OpenAI API key.
8. On acceptance, record the sequential `crates/app/src/settings/**` handoff to T027.

## Contract for downstream tasks

T016 receives the same `Entity<TimelineState>` and must not learn provisioning state. T035 validates
a persisted real session through this lifecycle; T019/T029 may use only the accepted evidence.
T027 may add reasoning settings after handoff but must not replace or gate this lifecycle.

## Automated implementation acceptance

- Automated lifecycle tests prove construction is cold and idle, picker cancellation has no
  session side effects, capture cannot start before model readiness, and cancellation/terminal
  races converge through one bounded stop path.
- Automated provisioner integration proves measured progress is carried into the UI, a verified
  cache is reusable, and a corrupt managed final is quarantined before replacement is attempted.
  If replacement cannot be fetched offline, the reachable product result is `OfflineNoCache`,
  with quarantine evidence shown when available; there is no invented distinct managed-cache
  runtime error.
- Automated state and persistence tests cover truthful target/audio labels, partial/final ingress,
  target/end outcome mapping, tail reload, and exactly-once recording-indicator cleanup.
- Focused app tests, formatting, strict Clippy, and feasible workspace checks pass without
  inferring OS-picker, real-device, network, signed-build, performance, or readability acceptance.

## Manual acceptance handoff

T035 owns the real signed-app map-tier acceptance that mocks cannot establish: cold launch and
picker cancellation, first managed download cancellation/resume/integrity, cached offline reuse,
scoped live audio/transcription, truthful indicators and terminal paths, persistence/reload, board
readability, the ten-minute no-reasoning session, and overlapping T016 resource/frame evidence.
T035 is a blocking ship gate. Its open status does not reopen this accepted implementation slice.

## Out of scope

Reasoning backend settings (T027), board rendering changes (T016), ASR provisioner internals
(T023), capture bridge changes, advisor behavior, or model-quality benchmarking.

## Implementation notes — focused verification (2026-08-11)

- Cold launch now constructs one idle `SessionController` and one `TimelineState` ingress, mounts
  the real board, and performs no picker, model, capture, or replay action until the visible Start
  control is pressed. Settings reads the controller entity directly; it has no shadow session.
- Start enters the system picker, then managed `base.en` provisioning with measured progress and
  cancellable setup. Phase transitions use a backpressure-aware channel so Verifying/Ready cannot
  be lost behind download updates. The explicit `SOTTO_WHISPER_MODEL` override remains supported.
- A locked start gate holds the same cross-thread mutex across session persistence and the
  synchronous `Pipeline::start` side effect. Stop-before-start creates neither a session record nor
  OS capture; Stop-after-start is treated as recording/finalizing until bounded shutdown. Running
  is reported only after `CaptureStatus::Running`, not merely after pipeline construction.
- The non-disableable indicator explicitly names microphone input, the chosen window/application
  screen scope, and either application/display audio or truthful system-wide audio. Runtime mic or
  capture errors, every terminal capture status, and channel failure all enter bounded stop,
  persist the end record, reload the event tail, and clear the indicator.
- Closing the sole control window is vetoed while provisioning, recording, or stopping, and first
  requests Stop. Normal app quit cancels and owns/joins the worker for the bounded shutdown window;
  terminal lifecycle delivery cannot block that join behind a full UI event channel.
- Managed-cache corruption is quarantined by T023 before a retry. If recovery is then offline, its
  reachable UI outcome is `OfflineNoCache`; a distinct `CorruptModel` state remains only for direct
  size/digest errors that escape provisioning, while invalid user overrides remain separately typed.

Focused verification used a fresh target because this machine has no local GPUI Metal compiler:

- `CARGO_TARGET_DIR=/private/tmp/sotto-t032-app-20260811-b cargo test -p app --locked
  --features gpui/runtime_shaders`: 39 passed, 0 failed; binary/doc tests passed.
- `CARGO_TARGET_DIR=/private/tmp/sotto-t032-app-20260811-b cargo check -p app --locked
  --features gpui/runtime_shaders`: passed.
- `CARGO_TARGET_DIR=/private/tmp/sotto-t032-app-20260811-b cargo clippy -p app --all-targets
  --locked --features gpui/runtime_shaders -- -D warnings`: passed.
- `cargo fmt --all -- --check` and `git diff --check`: passed.

The manual system-picker cancellation, first managed download and cancel/resume, verified cached
offline reuse, real mic/target audio transcript, target/Stop Sharing terminal paths, and ten-minute
no-reasoning conversation were **not run** and are not inferred from unit tests. They are assigned
to T035 as the explicit downstream blocking ship gate.

## Independent review and ownership handoff (2026-08-11)

Independent review accepted the automated implementation slice and its focused verification. The
reachable managed-cache behavior is now stated without a fictional branch: T023 quarantines a
corrupt final and attempts a replacement; corrupt-plus-offline surfaces `OfflineNoCache`, with the
quarantine path available as evidence when present.

T032 is `done`. `crates/app/src/main.rs`, `crates/app/src/session/**`, and
`crates/app/src/settings/**` are released to T027. T027 must preserve this lifecycle and cannot gate
the map tier on reasoning readiness. T016 retains its existing board ownership and manual judgement
contract; T035 gathers the shared real-run evidence without rewriting T016's historical claims.
