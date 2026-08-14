# T071 — Import an audio or video file as a session

**Status:** todo

**Wave:** M4 — recording

**Depends on:** ADR-0019. T058's file-backed transcription already reads a recording rather than a
live capture, which is what makes this small.

**Owns:** an import path under `crates/app/src/session/**`, the imported-session provenance in
`crates/core/src/types.rs` (planner-amended core ownership), the import entry point in
`crates/app/src/workspace/library.rs`, and this task

## Why this exists

Sotto can only reason about audio it captured itself. A user with an existing recording — a voice
memo, a downloaded call, a lecture video — has no way in, even though every stage after capture
already works on exactly this input.

ADR-0018 made transcription a consumer of a retained recording rather than of a live stream. So the
whole pipeline downstream of capture is already file-driven, and importing is mostly a matter of
producing a session whose recording came from somewhere else.

## The honest part

An imported file has **no capture-time provenance**. There is no capture target, no scoped-audio
claim, no screen frames, and no guarantee about channel layout — T061's convention that channel 0
is meeting audio and channel 1 is the microphone is a fact about Sotto's writer, not about an
arbitrary MP3.

Those absences must be explicit in the session record rather than defaulted, or an imported session
will masquerade as a captured one — claiming a scope nobody verified. This is the part of the task
that is actually load-bearing; the file handling is routine.

## Plan

1. Accept common audio and video containers. Reuse the existing decode path rather than adding a
   second one; state plainly which formats are supported and reject the rest with a clear reason
   rather than failing obscurely mid-transcription.
2. Create a session whose recording reference points at the imported media, marked as **imported**
   rather than captured. Copy the file into the recording directory so retention, the disk budget,
   pruning and deletion apply on exactly the same terms — an import the user cannot delete from
   Settings would break ADR-0018's promise.
3. Record the absent provenance explicitly: no capture target, no audio-scope claim, unknown channel
   layout. Surfaces that display capture scope must show what is actually known, not a plausible
   default.
4. Transcribe through the same path as a captured recording. There should be no second transcription
   implementation; if the reader needs a mono or unknown-layout mode, that is a small addition to it
   rather than a parallel path.
5. Handle a file with no video track, and a video file with no audio track. The first is normal; the
   second cannot be transcribed and must say so before doing work.
6. Give it a plain entry point in the library rail, next to starting a capture.

## Acceptance

- Importing an audio file produces a session with a transcript, and notes can be generated from it.
- Importing a video file additionally supports screen inspection at a cited moment.
- An imported session is visibly imported, and displays no capture target or audio-scope claim it
  cannot support.
- The imported media counts against the retention budget and is deletable exactly like a captured
  recording, asserted by test.
- A video with no audio track is rejected with a stated reason before transcription begins.
- An unsupported container is rejected with a stated reason.
- Per the verification rule, a real audio file and a real video file are imported and transcribed to
  known text.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Editing or trimming media, batch import, importing an existing transcript without media, streaming
URLs, and the notes taxonomy (T070).
