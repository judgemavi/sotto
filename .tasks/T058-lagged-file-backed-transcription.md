# T058 — Transcribe the recording, behind the capture

**Status:** done

**Wave:** M4 — recording

**Depends on:** satisfied in practice. T057's recording writer works — a 31s session produced a
30.97s playable recording with an explicit session-to-media time mapping — so this task has a file
to read. T057 remains `in-progress` on fragmentation and its NOT RUN real-capture acceptance.
ADR-0018 is accepted.

**Owns:** `crates/asr/**`, the transcriber contract in `crates/core/src/traits.rs`
(planner-amended core ownership), `crates/cli/src/pipeline.rs`, `crates/app/src/session/**`
including `recordings.rs`, the re-transcription persistence path in `crates/rag/**`, and this task

**Ownership amended (planner, 2026-08-13):** the earlier boundary forbidding
`crates/app/src/session/**` was written while T057 held those files. **T057 is now `done`, so that
claim is released and this task takes them.** T061 holds only `crates/capture/**` — do not edit the
Swift capture bridge or the writer; report against T061 instead. T050 also lists
`crates/app/src/session/**` in its `Owns`; it is `blocked` and must not be dispatched while this
task runs. Still off-limits: `crates/app/src/workspace/**` (T059 and T060), and
`crates/app/src/settings/mod.rs` unless the lag N needs a settings surface — if it does, say so and
stop rather than editing it.

## Goal

Make transcription a consumer of the session recording, running a configurable N seconds behind the
write head, emitting finals stamped in media time.

## Why this is a simplification

The streaming design exists to decide when a rolling hypothesis is safe to keep: a LocalAgreement
stabilizer, a 2.5 second unstable tail, agreement passes, and partials that supersede each other
every ~500 ms. Reading from a recording removes the question. With committed audio already on disk,
each window has full context and every emitted word is final on first emission.

Several defects this week came from that machinery rather than from the features built on it: the
stranded tail at capture stop, annotation anchors dissolving when their partial was superseded, and
a completed session rendering a live "Listening…" strip. Removing the cause is worth more than
continuing to handle the symptoms.

## Plan

1. Add a file-backed transcription path that follows a growing recording: transcribe up to N
   seconds behind the write head, and to end-of-file once capture stops, so the last utterance
   always settles. N is configurable with a stated default.
2. Stamp utterances in media time taken from the recording, not wall-clock arrival time. This is
   what makes T059's extraction land on the cited moment.
3. Emit finals. The `TranscriptUpdate` partial/final split from T018 stays — a lagged pass may still
   emit a partial for an in-flight window — but the steady state is finals, and superseded rolling
   hypotheses are no longer the normal case.
4. Converge the two ASR paths. The CLI harness already transcribes files; the product should run
   that path rather than a second streaming implementation that no test covers. Where they must
   differ, the difference is following a growing file versus a complete one, and nothing else.
5. Re-benchmark the default model. `base.en` was chosen under a realtime constraint that no longer
   applies; `small.en` or `medium.en` may now be affordable. Report measured accuracy and lag rather
   than assuming.
6. Support re-transcription of a completed recording, replacing that session's derived transcript
   while preserving the append-only log. This is the payoff of retention and it should exist from
   the start, even if the UI for it is minimal.
7. Retire what is no longer needed rather than leaving it in place: the stabilizer's agreement
   machinery and the unstable-tail configuration should go, or be documented as deliberately
   retained with a reason.

## Contract

- Consumers still read `utterance.final` from the timeline; only their timestamps' provenance and
  their arrival latency change.
- T059 relies on utterance timestamps being media timestamps.

## Acceptance

- A session's final utterance settles without special handling; capture stop no longer strands a
  tail.
- Utterance timestamps match media timestamps within a stated tolerance, asserted against a real
  recording.
- Transcription that falls behind catches up without losing audio; a deliberately overloaded run
  loses no speech, only time.
- Re-transcribing a completed recording produces a transcript for that session without mutating the
  original captured events.
- Per the verification rule, real recorded speech is transcribed to known text; the model-default
  benchmark is measured, not asserted.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Writing the recording (T057), frame extraction and OCR (T059), diarization, and any low-latency
supplementary pass for a live surface.

## Why this task is now urgent — 2026-08-13

Measured on a real 31.7s session, after the clock defect was repaired:

    recording          30.97s, 8.8 MB   full audio and video captured
    utterance.partial  66 events, 1.2s -> 31.2s
    utterance.final    1 event at 20.3s

The ASR hypothesised across the whole session and committed **once**. Partials stream to the screen,
so the user watches correct text arrive, and then almost none of it becomes durable. An earlier
session showed the same ratio — 7 finals from 373 partials over three minutes — so this is not a
regression, it is the streaming commit policy's steady state.

This is precisely the mechanism ADR-0018 removes. Reading from the recording with full context makes
every emitted word final on first emission, with no agreement heuristic and no unstable tail. Do not
tune `agreement_passes` or the tail length: that is tuning a mechanism this task deletes.

## Blocked at the growing-recording boundary — 2026-08-13

The finalized recording is usable after stop, but T058 cannot yet follow the product recording
while capture is running. T057 currently defaults `AVAssetWriter.movieFragmentInterval` to
`.invalid`: this avoids the confirmed fragmented-MP4 writer failure, but an unfragmented MP4 does
not publish the finalized metadata needed by a reader until capture stops. The only existing reader
seam, `probe_recording`, opens `AVAssetReader` after stop and returns metadata; it does not expose
committed audio or a write head.

Implementing against the CLI's complete WAV fixtures would prove a different artifact and would not
satisfy the growing-recording, backlog, or product-path acceptance cases. Per the planner ownership
boundary, changing the container, fragmentation policy, or writer outputs belongs to T057. Resume
T058 when T057 provides either a growing recording readable to a stable committed media time, or a
T057-owned committed-audio reader/sidecar contract using the recording's media clock. No ASR or
transcriber-contract code was changed while this dependency remains unresolved.

## Blocked on incremental readability — 2026-08-13

Stopped correctly at the ownership boundary rather than reaching into `crates/capture/**`.

The product currently writes **unfragmented** MP4, which is reader-addressable only after
`finishWriting`. This task's whole premise is transcribing a recording N seconds behind the write
head, so there is nothing to read while a session runs. The existing reader exposes finalized
metadata, not committed audio or a write head.

T061 must provide one of these before this task can proceed honestly:

- a growing recording that is readable up to a committed point while capture continues, or
- a committed-audio sidecar with media timestamps that this task can read incrementally.

Resumes as soon as one exists.

## Reader contract from T061 — 2026-08-13

T061 replaced `movieFragmentInterval` with application-owned segment output: `AVAssetWriter`
delivers an initialization segment and separable media segments to its delegate, which appends them
to one session MP4 and synchronizes after each. A reader may reopen that MP4 and consume through its
last committed segment while capture continues, which is what this task needs.

Two consequences this task must handle:

1. **Audio is now one stereo track, not two mono tracks.** Apple's HLS segment profile permits only
   one audio track. The mapping is Sotto-owned and fixed: **channel 0 / left is selected-target
   meeting audio, channel 1 / right is the local microphone.** This task splits those channels back
   into the two speaker streams. Assert the mapping rather than trusting it — deterministic speaker
   attribution now rides on a channel convention, so a swap would misattribute every utterance in
   the record without failing anything.
2. **Segment granularity is five seconds**, with a six-second durability tolerance on abrupt loss.
   The configurable lag N should account for that: reading closer to the write head than one segment
   yields nothing new.

A missing input channel is committed as silence behind a 500 ms reordering window. Late microphone
audio therefore appears as silence rather than as a gap. If that proves lossy in practice, report it
against T061 rather than compensating for it here.

**Unblocks when T061's normal signed capture run passes** — the writer is implemented but no real
media evidence exists yet, and building on an unverified writer risks churn.

## Unblocked, and what remains — planner, 2026-08-13

The reader was implemented ahead of that gate and it was the right call: a native test now proves an
`AVAssetReader` can reopen the MP4 and consume committed audio **while `AVAssetWriter` is still
active**. That is the single premise T058 was waiting on, and it is now demonstrated rather than
assumed. The evidence T061 still owes — abrupt-loss prefix and disk-full — constrains durability,
not readability, so it no longer gates this task's construction. It does still gate this task's
**acceptance**, which asserts against a real recording.

Delivered and verified by the implementing agent: growing-MP4 reader with a 10s configurable lag,
enforced left/right channel split with the mapping asserted rather than trusted, media-time stamps,
EOF tail flush, final-only FIFO Whisper path with no loss under backlog, and the CLI migrated off
the rolling agreement, VAD timestamp rewriting, and synthetic partial consolidation. Full workspace
tests, strict full-workspace Clippy, Swift tests, formatting, and the scoped diff check passed.

Three items remain, in order:

1. **Wire the reader into live sessions and re-transcription persistence.** Ownership is granted
   above. This is what makes the work reach the product rather than the harness.
2. **Re-benchmark the default model.** Weights are not cached, but provisioning already exists —
   `crates/asr/src/model/` downloads a pinned whisper.cpp revision with checksum verification and
   resumable partials. Use it. Measure `base.en`, `small.en` and `medium.en` for accuracy against
   known text and for lag against the configured N; report numbers, and recommend a default from
   them rather than from the ADR's expectation. Roughly 2 GB of downloads across the three.
3. **Real-recording acceptance**, once T061's normal signed capture run produces a stereo recording.

Do not claim acceptance on items whose evidence is still outstanding. State each of the three
separately in the closing report.

## Owned implementation completed behind the live gate — 2026-08-13

The maintainer explicitly asked T058 to proceed before the remaining signed T061 run. The work was
kept inside T058 ownership and does not upgrade the missing live evidence:

- `core` now distinguishes a growing recording from a complete one through the object-safe
  `RecordingTranscriber` contract. Completion is explicit, and the ordinary audio transcriber has a
  `finish` signal so an incomplete final media window can settle without a special transcript
  heuristic.
- `asr` has a macOS AVFoundation reader for the committed prefix of the fragmented MP4. It rejects
  anything other than the T061 stereo contract, decodes to 16 kHz PCM, and maps channel 0/left to
  `Source::System` and channel 1/right to `Source::Mic` using each decoded buffer's media PTS.
- `LaggedRecordingTranscriber` defaults to ten seconds behind the readable media duration and
  rejects a lag below the writer's five-second commit interval. While growing, it advances only to
  `duration - lag`; at completion it reads to EOF exactly once and flushes the tail.
- `FinalWhisperTranscriber` replaces bounded-ring/rolling-agreement behavior for the file path. It
  retains backlog in an unbounded FIFO, inserts explicit silence for media gaps, removes overlap,
  transcribes non-overlapping ten-second windows, bounds Whisper's padded timestamps to supplied
  media, and emits only `utterance.final` with the window's media-time offset.
- The CLI complete-WAV harness now uses that same final-only engine. Its T018-era wrapper that
  rewrote Whisper timestamps to nearby VAD segments, emitted synthetic partials, selected a longest
  candidate, and consolidated finals at EOF is removed.

The legacy `WhisperTranscriber`, ring, stabilizer, agreement passes, and unstable tail remain only
because the product session currently constructs that type in `crates/app/src/session/mod.rs`, which
this task explicitly does not own. Switching the live app to `LaggedRecordingTranscriber`, scheduling
its growing-file polls, and removing the legacy implementation requires coordinated ownership of the
session runtime (and likely `core` pipeline orchestration). Re-transcription persistence likewise
needs the session/timeline store seam so a new derived transcript can replace the selected view while
the original append-only events remain intact. Those changes were not smuggled across the planner's
ownership boundary.

## Automated evidence — 2026-08-13

- The native Swift reader wrote and decoded a finalized stereo MP4, preserving distinguishable left
  and right signals after AAC and 16 kHz conversion.
- A second native test used the same HLS segment-output profile as T061, appended initialization and
  media segments to disk, kept `AVAssetWriter` open, and successfully read the committed audio and
  duration before finalization. This proves the incremental reader shape rather than only EOF input.
- Rust tests prove fixed channel-to-speaker mapping, ten-second lag, EOF catch-up, idempotent
  completion, FIFO backlog, media timestamps, explicit gap silence, and overlap suppression.
- Focused all-feature tests passed for `core`, `asr`, and `cli`. ASR reported 22 passed and one
  ignored live-model test; the CLI's real-ASR test remained ignored without a configured model.
- Full locked workspace tests passed. Manual/live/provider/performance tests remained ignored.
- Strict full-workspace Clippy passed over all targets and features. Rust formatting and the
  T058-scoped diff check passed.

Still **NOT RUN** and therefore not accepted:

- the current T061 stereo writer in a signed real ScreenCaptureKit session;
- real recorded speech through `LaggedRecordingTranscriber`, including timestamp tolerance and an
  intentionally overloaded catch-up run;
- the `base.en` / `small.en` / `medium.en` accuracy-and-lag benchmark (no verified Whisper model is
  cached in this environment, and fetching all candidates is a substantial download);
- product-session polling, final-tail persistence, and completed-session re-transcription, because
  their files remain outside T058 ownership.

T058 remains **blocked**, now at product integration and real/model acceptance rather than at the
incremental-reader implementation. Do not mark it complete from the synthetic fragmented-media test.

## Product integration completed — 2026-08-13

The remaining owned product path is now implemented:

- Live sessions construct `LiveRecordingTranscriber`, whose pipeline adapter ignores capture-audio
  samples and polls the committed MP4 prefix at the configured cadence. The session worker retains
  the same recording cursor after the pipeline releases its adapter.
- After capture reports recording finalization, that retained handle reads through exact EOF and
  flushes the incomplete final Whisper window. Tail finals receive media timestamps, are appended
  transactionally after the stopped pipeline's last event id, and are forwarded to the live
  workspace.
- Completed-session re-transcription is exposed by `RecordingLibrary::retranscribe`. It reads the
  retained recording through the same final-only engine and atomically replaces a dedicated derived
  transcript projection in SQLite. Original captured timeline events are unchanged.
- The obsolete rolling `WhisperTranscriber`, SPSC ring, agreement stabilizer, unstable-tail fields,
  and their tests were removed. The CLI and product now converge on `FinalWhisperTranscriber`.

Focused evidence from this implementation:

- ASR library tests: 19 passed, one ignored real-model download/inference test.
- RAG persistence tests: 16 passed, including transactional EOF-tail append and derived
  re-transcription replacement without timeline mutation.
- `cargo check` passed for ASR, CLI, RAG, and the app over all targets.
- Strict Clippy passed for ASR, CLI, RAG, and the app over all targets.

Still **NOT RUN** and therefore T058 remains `in-progress`:

- signed real ScreenCaptureKit media through the product session, including timestamp tolerance and
  final-tail observation;
- a deliberately overloaded real-recording catch-up run;
- the `base.en` / `small.en` / `medium.en` accuracy-and-lag benchmark (the three model artifacts are
  not cached, and the ignored real-model test would download only `base.en`);
- browser or real-device acceptance.

Coordinated integration gates completed after handoff: `cargo test --workspace --locked` and strict
`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings` both pass. The
ignored real-model, live-provider, Keychain, and performance tests remain separate from those green
automated gates.

## First real run — 2026-08-13

The maintainer ran the product against a live meeting. Against that platform's own transcript the
output is close to verbatim: "the Canada instance… discover agent is what I can download one of the
existing agent code" and "we will know if it really failing or not" both match the reference, and
proper nouns survive. Errors are ordinary ASR substitutions ("labeled" heard as "a play"), not the
structural loss this task was opened to fix. Compare with the pre-change baseline recorded above —
one final from 66 partials in 31 seconds. **The ADR-0018 premise is validated on real media.**

This is qualitative product evidence, not the acceptance. Still NOT RUN: the media-timestamp
tolerance assertion, the overloaded catch-up case, and the base/small/medium benchmark.

Two defects observed in the same run are **not** this task's to fix; they are filed as T062:
`Show screen` cannot resolve a recording during a live meeting because `save_recording` runs only
after stop, and `RecordingLibrary::retranscribe` has no caller. The second is a planner ownership
error, recorded there.

## Closed with narrowed acceptance — planner, 2026-08-13

Accepted on the owned slice: the growing-MP4 reader, the enforced and asserted left/right channel
split, media-time finals, EOF tail settlement, the final-only FIFO path, the CLI convergence, and
the removal of the stabilizer machinery. Real-media quality is corroborated above.

Two acceptance items are **not** dropped; they are reassigned with explicit owners, per the board's
rule for closing with a residual:

- **The model benchmark** (`base.en` / `small.en` / `medium.en`, accuracy and lag, and the default
  recommendation) moves to **T063**, which takes `crates/asr/**` and `crates/cli/src/pipeline.rs`.
- **The media-timestamp tolerance assertion and the overloaded catch-up case** move to **T064**,
  the signed recording acceptance gate, because both require a real capture run.
- **Product reachability of `retranscribe`** moves to **T062**, which holds both sides of that seam.

An open measurement question for T064: a 3:37 screenshot of a session started at 3:34 showed the
transcript at media time `01:11`, roughly two minutes behind a configured ten-second lag. If the
panel was scrolled rather than at its tail this is nothing. If it was at the tail, transcription is
running slower than realtime and the backlog is unbounded, which would make a one-hour meeting take
hours to settle. Measure the last transcript timestamp against the real meeting length.
