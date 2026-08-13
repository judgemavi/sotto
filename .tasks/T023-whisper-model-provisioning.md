# T023 — Whisper model provisioning

**Status:** done

**Wave:** Phase 2 — blocks anyone using Sotto who did not build it

**Depends on:** T005 (`asr::Config`, `ModelSize`)

**Owns:** `crates/asr/src/model/**`, `crates/asr/src/lib.rs`, `crates/asr/Cargo.toml`,
`Cargo.lock`

## Why this exists

`AGENTS.md` says Whisper weights download on first run, defaulting to `base.en`. That was
settled as a product decision and never implemented — nothing in the workspace fetches a model.
T022 consequently requires `SOTTO_WHISPER_MODEL` to point at weights the user obtained by
themselves, which means the app cannot transcribe for anyone who did not build it.

## Plan

1. **Resolve, then download.** Look for an existing model in the application support directory
   first; fetch only if absent. A second launch must not re-download.
2. **`base.en` by default**, with the other sizes selectable — `ModelSize` already exists.
3. **Verify what was downloaded.** Check the expected digest before use. A truncated or
   corrupted model that loads and transcribes noise is worse than a failed download, because the
   failure surfaces as bad transcription rather than as an error.
4. **Show progress and let it be cancelled.** This is a multi-hundred-megabyte download on first
   launch; silence for several minutes reads as a hang.
5. **Fail honestly offline.** No network and no cached model is a clear message with an action,
   not a session that starts and produces nothing.
6. **Keep `SOTTO_WHISPER_MODEL` as an override** for development and for users who bring their
   own weights.

## Acceptance

- First run with no model and no configuration downloads `base.en` and transcribes.
- Second run uses the cached model with no network access.
- A corrupted download is detected and reported, not used.
- Cancelling mid-download leaves no partial file that a later run mistakes for a model.
- Offline with no cached model produces a clear message, not a silent empty transcript.

## Out of scope

Model selection UI (T012), diarization models, the VAD model (`crates/vad` bundles its own).

## Notes — implementation pass 1 (2026-08-11)

- Added a managed model provisioner under `asr::model`. It resolves the macOS Application
  Support model directory, pins the official whisper.cpp `base.en`, `small.en`, and `medium.en`
  artifacts to immutable revision `c521a4b02f422512d734391fdf08bb08c0862f68`, and verifies
  both exact byte length and SHA-256 before atomically renaming a `.partial` file into place.
  `base.en` is now the actual `ModelSize` default.
- Downloads report resolving/downloading/verifying/ready progress, can be cancelled during
  network transfer or SHA-256 verification, and resume retained `.partial` data with an HTTP
  Range request. A cancelled or corrupt artifact is never installed under the final filename.
- `SOTTO_WHISPER_MODEL` remains an explicit user/developer override. Override files are checked
  to be non-empty regular files and are never replaced or copied; arbitrary user-owned weights
  cannot be compared to Sotto's managed digest table, so whisper.cpp validates compatibility
  when the transcriber loads them.
- Automated verification: `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test -p asr` passed with 12
  passed and 1 ignored; the ignored test is the network/hardware canary. Focused tests cover a
  cached no-network launch, same-sized corruption, explicit override precedence, mid-download
  cancellation, cancellation while hashing, and resumable Range download. Strict
  `cargo clippy -p asr --all-targets -- -D warnings` passed with the same bindings setting.
- The real canary was run manually. It downloaded the official 147,964,211-byte `base.en`,
  verified SHA-256, loaded it through whisper.cpp with Metal on an Apple M1 Pro, and recovered
  the known `pricing`/`enterprise` concept from `fixtures/call-01-mic.wav`.
- `whisper-rs-sys` 0.14.1's fresh bindgen path generates an opaque one-byte
  `whisper_full_params` on the current Rust 1.97/Xcode toolchain and fails its own 296-byte size
  assertion. Its bundled bindings compile and pass when
  `WHISPER_DONT_GENERATE_BINDINGS=1`; this pre-existing build-environment requirement remains a
  CI/build follow-up rather than being hidden as T023 success.
- Product wiring is deliberately still open: `crates/app/src/session/mod.rs` remains owned by
  T022 and still refuses to start without `SOTTO_WHISPER_MODEL`. T022 (or a planner-approved
  follow-up) must call `resolve_configured_or_download`, surface progress/cancel/error state,
  and pass the resulting path into `AsrConfig` before the first-run acceptance criteria are met.

## Review — initial findings

The real artifact/inference canary and integrity checks are valid, but the task cannot close yet:

- Product acceptance is unwired: app session startup still accepts only an explicit
  `SOTTO_WHISPER_MODEL`; managed resolution, progress, cancellation, and actionable offline state
  are unreachable from the product.
- A corrupt managed final cache is reported but never quarantined or replaced, permanently
  wedging later launches even when the network is available.
- Missing-cache/offline failure is an opaque network string rather than an actionable typed state,
  and has no focused acceptance test.
- Recheck cancellation after verification and before atomic installation so a late cancel cannot
  install and report Ready.
- Harden resume behavior for malformed/unsatisfiable Content-Range responses and add coverage for
  a server that ignores Range. Final size/digest validation already protects integrity.
- Serialize provisioning of one artifact, or otherwise make concurrent callers share/recover the
  same `.partial` without racing its truncate/remove/install lifecycle.

The internal recovery/cancellation issues remain T023 work. App wiring requires an explicit
handoff from T022/T012-owned app surfaces or a follow-up task; until that lands, first-run product
acceptance is not met.

## Notes — implementation pass 2 (2026-08-11)

- Corrupt managed final files are now moved to the sibling `.corrupt` quarantine and provisioning
  continues through a fresh verified download. The user-owned override path is still never moved,
  copied, deleted, or replaced.
- A missing verified cache plus an unreachable download service now returns the typed,
  actionable `OfflineNoCache` state, directing the user to reconnect and retry or set
  `SOTTO_WHISPER_MODEL`. The wording remains accurate when only a resumable partial exists.
- Both verified-partial installation paths recheck cancellation immediately before their atomic
  rename. A focused test proves a late cancellation retains the verified partial and never creates
  the ready filename.
- Resume responses now validate the complete `Content-Range` start/end/total and optional
  `Content-Length`. HTTP 416 and malformed 206 responses discard the unusable partial and retry
  exactly once as a full request; a server that ignores Range with HTTP 200 truncates before
  writing. Focused tests cover all three recovery paths.
- Provisioning is serialized per canonical managed destination across independent provisioner
  instances in the process. A concurrent-call test proves two callers resolve the same verified
  file while the server receives exactly one download request.
- Verification: `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test -p asr --locked` passes 18 automated
  tests with the real 148 MB artifact/inference canary ignored. Strict locked ASR clippy and
  formatting checks pass. Loopback HTTP tests require the approved unsandboxed run.
- The internal changes-requested items are addressed, but product acceptance remains open exactly
  as recorded above: T022/T012-owned app/session wiring has not been changed by T023.

## Review — implementation accepted, product wiring blocked

The reviewer reran the full locked ASR suite: 18 automated tests passed and the previously run real
artifact/Metal canary remains valid. Internal recovery, cancellation, resume, concurrency, and
offline-state findings are resolved. T023 remains blocked solely because the app still bypasses the
provisioner; it cannot be marked done until the T022/T012 ownership handoff lands session progress,
cancellation, and resolved-model startup behavior.

## Product-integration handoff

The provisioner library slice is accepted and this task is now `done`. T032 explicitly owns the
remaining product acceptance: invoke managed resolution before capture, show progress, permit
cancellation, surface typed offline/corrupt states, and pass the resolved model path into the live
session. This narrows T023 rather than rewriting its historical review, and prevents an accepted
ASR library task from remaining indefinitely `blocked` on app files it does not own.
