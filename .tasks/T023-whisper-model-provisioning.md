# T023 — Whisper model provisioning

**Status:** open

**Wave:** Phase 2 — blocks anyone using Sotto who did not build it

**Depends on:** T005 (`asr::Config`, `ModelSize`)

**Owns:** `crates/asr/src/model/**`

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
