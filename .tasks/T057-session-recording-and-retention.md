# T057 — Write the session recording, and let the user manage it

**Status:** done

**Wave:** M4 — recording

**Depends on:** satisfied. T002 closed on 2026-08-13 and released `crates/capture/**` and
`docs/adr/0002-capture-architecture.md`. ADR-0018 is accepted.

**Owns:** `crates/capture/**`, the session recording reference in `crates/core/src/types.rs`
(planner-amended core ownership), `crates/app/src/session/**`, the recording retention path in
`crates/rag/**`, and this task.

**Amended 2026-08-13:** T055 released the mounting points this task was blocked on. T057 now also
owns `crates/app/src/settings/mod.rs` for the recording library controls and
`crates/app/src/workspace/layout.rs` for the visible recording copy in the session bar. It owns no
other workspace column; `notes.rs` and `transcript.rs` belong to T056, which runs concurrently.

## Goal

Persist the captured audio and video of a session to a local recording, link it to the session
record, and give the user a visible, measurable, deletable library of recordings under a disk budget.

Per ADR-0018 the recording is the source of truth. T058 transcribes it; T059 extracts frames from
it. Neither can start until it exists.

## Plan

1. Write audio and video for the chosen target to a local recording as capture runs. Writing the
   recording is the only realtime obligation on this path — a dropped frame is a defect, a late
   transcript is not.
2. Record the recording reference on the session: path, container, duration, byte size, and the
   mapping from session-relative time to media presentation time. ADR-0018 makes transcript time and
   media time the same clock; that equivalence must be recorded and asserted, not assumed.
3. Handle the ugly cases explicitly, because they decide whether the record is trustworthy:
   crash or power loss mid-session must leave a playable, seekable prefix rather than an unusable
   file; a full disk must stop the session with a truthful reason instead of silently truncating.
4. Enforce a disk budget with automatic pruning of the oldest recordings. Default 20 GB,
   user-raisable. A pruned recording leaves its timeline intact and its session marked as no longer
   having media, so citations and later inspection report an honest missing state rather than
   failing obscurely.
5. Surface the library in Settings: per-session size, total usage against budget, and delete. Delete
   removes the media and marks the session, never the timeline.
6. Update the running indicator to state that a recording is being kept. The shipped build says
   `screen: <target>` while discarding every frame; that copy is the failure this replaces.

## Contract

- T058 reads the recording and the time mapping. T059 decodes single frames from it on demand.
- A session whose recording was pruned or deleted remains fully reviewable as a transcript.

## Acceptance

- A completed session has a playable recording whose duration matches the session's elapsed time
  within a stated tolerance, and whose media timestamps agree with timeline timestamps.
- Killing the process mid-session leaves a playable, seekable prefix.
- A simulated full disk stops the session with a truthful message and no partial-file corruption.
- Pruning and deletion free the bytes, keep the timeline intact, and mark the session as media-less;
  a citation into a pruned session reports missing media honestly.
- Settings shows per-session size, total against budget, and deletes on request.
- Per the verification rule, at least one test records real capture output and asserts the file is
  decodable at a known timestamp — not that a writer object was constructed.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Transcription changes (T058), frame extraction and OCR (T059), encryption at rest, cloud sync, and
per-session opt-out of recording.

## Notes

Implementation in progress on 2026-08-13:

- The ScreenCaptureKit bridge writes a fragmented MP4 during capture with H.264 video plus separate
  meeting-audio and microphone AAC tracks. Normal stop finalizes the asset; writer failure stops the
  session with a disk-specific reason. `SOTTO_RECORDING_MAX_BYTES` provides a deterministic
  disk-full evidence path.
- The soak harness records `capture-soak-output/session.mp4`, decodes its first video frame through
  `AVAssetReader`, and asserts media duration against elapsed capture. It also supports abrupt
  `_exit` through `SOTTO_CAPTURE_CRASH_AFTER_SECONDS` and a later
  `SOTTO_RECORDING_PROBE_ONLY=1` prefix decode. These real picker/device modes are implemented but
  **NOT RUN** in this handoff.
- Core now defines the explicit identity session-to-media clock and available/deleted/pruned
  recording states. SQLite schema v7 links that reference to the session, persists the 20 GB
  default budget, accounts usage, deletes media without deleting the timeline, and prunes oldest
  media with an honest missing state.
- `RecordingLibrary` under `crates/app/src/session/**` exposes per-session rows, usage/budget,
  delete, and budget mutation for the Settings surface.
- Automated evidence: capture 8/8 across library/example targets; RAG 24/25 with the existing
  fastembed performance check ignored; app 98/98; full locked workspace passed with its declared
  manual/model tests ignored; strict Clippy passed for core/capture/RAG/app over all targets and
  features; formatting passed. Scoped diff check passed. Repository-wide `git diff --check` is
  blocked only by the pre-existing blank EOF line in `.tasks/T048-top-down-meeting-workspace.md`,
  outside T057 ownership.
- After planner-amended ownership, the mounted Settings view now shows local recording usage against
  budget, accepts a whole-GB budget raise, reports any automatic pruning, lists each available or
  missing recording with its size/duration/state, and deletes media without implying that the
  transcript was deleted. The running session bar now states `Recording · kept on this Mac` beside
  the independently truthful screen/audio/microphone scope.
- Post-mount automated evidence: app library 103/103; RAG library 11/11 with the declared fastembed
  performance check ignored; capture library/example targets 8/8; strict app Clippy over all targets
  and features passed; formatting and scoped diff checks passed. The first sandboxed app attempt
  failed only because Metal and Swift could not write `~/.cache/clang/ModuleCache`; the identical
  approved-environment rerun passed.
- The inherited completed-annotation concern remains intentionally documented rather than papered
  over: `TimelineBuilder` has no API to restore persisted event envelopes or seed its next id, while
  this path must allocate that id inside the same SQLite transaction as the insert. Reconstructing a
  parallel builder would either renumber prior events or move allocation outside the transaction.
  Centralizing the checked annotation constructor therefore requires planner-granted ownership of
  `crates/core/src/timeline/{builder,event}.rs`; T057's amended core ownership covers only the
  recording reference in `types.rs`.
- T057 remains `in-progress`: the real signed picker/device capture, abrupt-process prefix decode,
  and simulated full-disk runs are implemented but still **NOT RUN**. Automated fixture/UI evidence
  does not satisfy those real-capture acceptance cases.

## Inherited from T056 — 2026-08-13

T056 closed; two items transfer here so `layout.rs` and `crates/rag/**` have one owner again.

1. **Route the completed-annotation append through the checked builder.**
   `crates/rag/src/store/annotations.rs` constructs the event with `serde_json::from_value` and
   re-implements the payload invariants locally. `TimelineBuilder::append_user_annotation` and
   `supersede_user_annotation` exist at `core/src/timeline/builder.rs:148,161`, and the live path
   uses them; T051 established that only checked builder methods construct `UserAnnotation`
   payloads. Duplicate invariants drift, and JSON construction fails at runtime on a `core` field
   rename that compiles clean. Route through the constructor, extract the shared validation, or
   record why duplication is unavoidable given ids must be allocated inside the SQL transaction.
2. **`crates/app/src/workspace/layout.rs` is wholly owned by this task again**, including the
   past-meeting banner block T056 edited. Re-read the file before editing; it changed after this
   task's last read.

## Planner grant — 2026-08-13

Core ownership granted for the annotation-constructor cleanup: `crates/core/src/timeline/builder.rs`
and `crates/core/src/timeline/event.rs`, on the same basis as T018 and T051. Scope is limited to
exposing one checked `UserAnnotation` construction path usable by both the live pipeline and the
store's post-completion append, where the store allocates ids inside its SQL transaction. Do not
widen `UserAnnotation` itself or touch other payloads; stop and report if the fix appears to need
either.

## Planner-made edits and defect handoff — 2026-08-13

The recording writer fails roughly one second into every session on the maintainer's machine.
Capture auto-stops and the user is told the disk is full; the disk is fine. Diagnosed jointly with
the maintainer in a live debug session. **Edits were made by the planner in this task's owned file**
`crates/capture/bridge-macos/Sources/SottoCaptureBridge/CaptureBridge.swift`; keep or replace them
deliberately.

Edits made:
- `reportFailure()` became `reportFailure(_ cause:)`. It names which of the five failure paths fired
  and writes `writer.error`, its `NSUnderlyingError`, and `localizedFailureReason` to stderr before
  the FFI collapses everything to status -8.
- `append()` takes a `label`; call sites pass `video`, `system audio`, and `microphone` with the
  mic's rate and channel count.
- Microphone samples are now retimed to a `microphoneOrigin`, as video and system audio already
  were. This was a genuine bug — absolute `streamTimeNs` appended against
  `startSession(atSourceTime: .zero)` — but it was not the cause of this failure.

Evidence:

    sotto: recording failed: system audio: input.append returned false, pts=1.02s
      | AVFoundationErrorDomain code=-11800
      | underlying: NSOSStatusErrorDomain code=-16341

The same failure is reported by the microphone and video inputs on other runs. `AVAssetWriter` fails
as a unit, so the reporting input is whichever appends next; this is one defect, not three.

Ruled out: disk space, the `SOTTO_RECORDING_MAX_BYTES` cap (unset), and the microphone PTS bug above.

Hypothesis to verify, not to assume: a PCM/encoder format mismatch. Both audio inputs are hardcoded
to `AAC / 48000 / 1 channel` at `CaptureBridge.swift:248`, while microphone PCM carries the device's
real rate and channel count from `default_input_config()` (`macos.rs:415`) and ScreenCaptureKit
system audio is stereo by default. The encoder accepts appends until it converts, which matches the
consistent ~1s delay.

Fix, pick one and record why:
- (a) configure each audio input from the actual incoming format, decided before the writer is built;
- (b) convert to mono 48 kHz before appending, reusing the downmix the ASR path already performs at
  `macos.rs:466`.

Planner leans (b): the recording exists to be re-transcribed and inspected, not to be a master.

**Also required, independent of the fix:** status -8 must stop asserting "the disk is full". Five
distinct causes collapse into that one message, and it sent this debugging session in the wrong
direction for several rounds. Carry the real cause through the FFI, or state that recording stopped
without claiming a reason the code does not know.

## Defect implementation handoff — 2026-08-13

Implemented after the planner findings:

- Chose option **(b)**. Both retained audio tracks now enter `AVAssetWriter` as canonical mono
  48 kHz Float32 PCM with an explicit source-format hint before AAC encoding. ScreenCaptureKit audio
  reuses the already validated/downmixed samples delivered to Rust. Microphone audio is downmixed
  and statefully linearly resampled on the serial recording queue, preserving fractional phase and
  the previous sample across CPAL packet boundaries. The original input rate/channel count remains
  in diagnostics.
- Kept the planner's detailed `writer.error` diagnostics. The error callback now also carries the
  callback-scoped native detail across Swift/C/Rust. The app retains that cause in its recording
  terminal state and no longer labels an unknown encoder/writer failure as disk-full. The explicit
  `SOTTO_RECORDING_MAX_BYTES` cause remains distinguishable in the detail.
- Used the planner-granted core seam: `checked_user_annotation` is now the one public payload
  constructor used by both `TimelineBuilder` and the transactional post-meeting writer. SQLite still
  allocates the envelope id inside its transaction; annotation trimming and validation are no
  longer duplicated in RAG.

Automated evidence after the defect fix:

- Capture library/example targets: 8/8 passed, including native-cause propagation before terminal
  status publication.
- Full locked workspace: passed. App 104/104; core 26/26; RAG library 11/11 plus persistence 14/14;
  declared live/model/performance tests remained ignored.
- Strict Clippy passed over all targets/features for core, capture, RAG, and app. Formatting and
  scoped diff checks passed.

Still **NOT RUN**: a real picker/device recording after this conversion. The code compiles through
the Swift bridge and the hypothesis has an implemented regression boundary, but only the maintainer's
original live setup can prove that the one-second `NSOSStatusErrorDomain -16341` failure is gone.
The signed real-capture, abrupt-prefix, and simulated-full-disk acceptance runs therefore remain
open and T057 remains `in-progress`.

## Defect update after the mono-48k fix — 2026-08-13

The audio canonicalization worked and audio is ruled out. Probing the surviving 0.54s prefix with
AVFoundation:

    video  1328x1406  avc1
    audio  48000Hz  1ch  aac
    audio  48000Hz  1ch  aac

Both audio tracks are exactly mono 48 kHz AAC, and all three tracks encode cleanly for the prefix.

The failure still occurs, and the improved instrumentation changes what it means:

    video: writer status is 3, expected .writing | AVFoundationErrorDomain -11800
      | underlying NSOSStatusErrorDomain -16341

Status 3 is `.failed`. **No append was rejected.** The writer failed asynchronously while encoding
buffers it had already accepted, and the next append merely observed the corpse. Every "which input
is malformed" theory, including the planner's, is therefore dead.

Also ruled out: frame-size changes mid-stream. `streamConfig.width/height` are pinned at
`CaptureBridge.swift:661-662`, so ScreenCaptureKit delivers a constant size.

What is left is the encoder itself. Note the capture size: **1328x1406** — portrait, non-standard,
not macroblock-friendly. The video input declares fixed `AVVideoWidthKey`/`AVVideoHeightKey` from it
with a 0.5s max keyframe duration, into a fragmented MP4.

**Stop hypothesising and bisect.** Two mechanical experiments, in this order:

1. Force a standard capture size (1280x720) in both the stream config and the video input. If the
   session survives, the defect is an encoder constraint on the window-derived dimensions, and the
   fix is to scale to a sane encode size rather than encode whatever the window happens to be.
2. If it still fails, disable tracks one at a time — audio-only, then video-only — and see which one
   kills the writer. The writer failing asynchronously makes this the only reliable isolation.

The maintainer has spent several rounds on this. Do not send back another single-hypothesis patch:
run the bisect, report which configuration survives, and fix from the answer.

## Fragmentation confirmed as a cause — 2026-08-13

Disabling `movieFragmentInterval` moved the failure from ~0.5-1.6s to **8.16s**, and changed the
underlying OSStatus from -16341 to -16122.

    SOTTO_RECORDING_FRAGMENT_SECONDS=0
    video: input.append returned false, pts=8.157062459s
      | AVFoundationErrorDomain -11800 | underlying NSOSStatusErrorDomain -16122

Evidence trail: every earlier failure landed on a 0.5s fragment boundary (0.54, 1.02, 1.09, 1.6),
which is why the surviving prefixes were always clean, probe-able files ending near a multiple of
the interval. The planner added a switchable interval at `CaptureBridge.swift:241` behind
`SOTTO_RECORDING_FRAGMENT_SECONDS` to test this; keep or replace it deliberately.

Two things follow.

1. **Decide fragmentation deliberately, do not just delete it.** This task's acceptance requires that
   killing the process mid-session leaves a playable, seekable prefix. Fragmented MP4 is what makes
   that true — an unfragmented MP4 loses its `moov` atom on abrupt termination and is unplayable. If
   fragmentation goes, that acceptance criterion has to change with it, and the crash-prefix
   guarantee must be delivered some other way or withdrawn. Do not let it fail silently later.
2. **The 8.16s failure is a separate defect.** Different OSStatus, different timing, and it appears
   only once fragmentation is off. Bisect it as previously instructed: video-only, then audio-only,
   now that the run survives long enough for the isolation to mean something. Report which
   configuration survives past 8s before proposing a fix.

Neither -16341 nor -16122 appears in the SDK headers; both are undocumented MediaToolbox-range
statuses. Treat the timing correlation as the evidence, not the numeric code.

## Root cause for the post-fragmentation failure — 2026-08-13

With fragmentation off, capture now fails at random times — 8.16s on one run, 3.71s on the next,
both `NSOSStatusErrorDomain -16122`, both with the writer still `.writing` and the video `append`
itself rejected.

`AVAssetWriterInput.append` returns false when a sample's presentation timestamp is not strictly
greater than the previous sample's for that input, while the writer remains `.writing`. That matches
the observed signature exactly, and the randomness matches the cause: ScreenCaptureKit repeats
frames when the captured content is static, so it depends on when the window stops changing.

Two gaps confirmed by reading the bridge:

1. **`SCStreamFrameInfo.status` is never checked.** `didOutputSampleBuffer` at
   `CaptureBridge.swift:731` guards only `sampleBuffer.isValid` before calling `appendVideo`.
   ScreenCaptureKit delivers `.idle`, `.blank`, `.suspended`, and `.started` frames alongside
   `.complete` ones. Only `.complete` frames carry new content and a usefully advancing timestamp.
2. **No presentation-timestamp monotonicity guard on any input.** `appendVideo` retimes against
   `videoOrigin` and appends blind, and the audio paths do the same.

Fix, in this order:

1. Filter on `SCStreamFrameInfo` and append only `.complete` frames. This is the documented
   contract, not a workaround, and it should remove most repeated timestamps at the source.
2. Keep a per-input last-appended timestamp and drop, or nudge forward, any sample whose retimed
   timestamp is not strictly greater. Defence in depth: the writer must never be handed a
   non-monotonic sample regardless of what the source does.
3. Only then revisit fragmentation, which remains an open decision above.

Prove it with a test that feeds a repeated or out-of-order timestamp through the append path and
asserts the writer never sees it. Do not verify this one by launching alone: the failure timing is
content-dependent, so a clean run proves nothing on its own.

## Post-fragmentation timestamp fix — 2026-08-13

Implemented both confirmed ScreenCaptureKit/AVAssetWriter contract gaps:

- The screen callback now reads `SCStreamFrameInfo.status` and admits only `.complete` frames to
  both the recording writer and retained-frame delivery. Idle, blank, suspended, started, and
  stopped buffers never reach the writer.
- The writer now owns independent last-successfully-appended PTS state for video, system audio,
  and microphone inputs. Immediately before each `AVAssetWriterInput.append`, a numeric PTS at or
  behind that input's prior appended PTS is shifted to one tick after it. All timing entries in the
  sample buffer move by the same offset, and state advances only after `append` returns true.
- Added a native append-gate probe and a Rust regression that forces repeated and out-of-order PTS:
  `[0, 0, -1ns, 1s, 0.5s]`. The timestamps presented to the writer boundary are asserted to be
  `[0, 1ns, 2ns, 1s, 1s+1ns]` and strictly increasing.

Automated evidence:

- `cargo test -p capture --all-targets --locked`: capture library 7/7 and soak example 2/2 passed,
  including `repeated_and_out_of_order_pts_are_nudged_before_writer_append`.
- `cargo clippy -p capture --all-targets --all-features --locked -- -D warnings`: passed.
- `cargo fmt --all -- --check` and scoped diff checks: passed.

Fragmentation was deliberately left unchanged. The 0.5-second default and diagnostic environment
switch remain while crash-prefix acceptance is unresolved. A clean live run is not claimed as proof
for this content-dependent defect; signed real-capture and crash-prefix acceptance remain open.

## Fix confirmed, and a new defect — 2026-08-13

**The recording fix works.** A live session ran 1:27 and counting without a writer failure, with the
transcript flowing normally. The two changes that mattered were the `.complete`-frame filter with
per-input PTS monotonicity, and disabling `movieFragmentInterval` (now defaulted off at
`CaptureBridge.swift:241`, since the 0.5s path is known-broken while the fragmentation decision is
open).

**New defect, high severity: Stop is unreachable while recording.** The session bar overflows its
window. At the maintainer's window width the bar renders capture state, target, three scope chips,
the recording chip, and the elapsed clock, then clips — `Pause` is cut mid-word and `Stop` is
entirely off-screen. The Ask rail is reduced to a single character in the same overflow.

AGENTS.md requires that Stop is always one obvious action away while a session runs. A capture the
user cannot stop from the visible UI fails that outright, and the only remaining exit is closing the
window and relying on the close guard.

This is the same failure mode as the session-rail title: a row of `gpui_component` controls, each
defaulting to `flex_shrink_0`, with no shrink budget and nothing marked as the element that must
never be clipped. Fixing the width bounding alone will not be enough — the terminal controls need
priority over the chips, which are the expendable content.

Suggested shape, not prescriptive: give Stop and the clock fixed placement that cannot be
compressed, let the scope chips collapse or wrap first, and verify at a deliberately narrow window
rather than at whatever width the developer happens to use.

## Reachable terminal controls fix — 2026-08-13

Implemented an explicit shrink priority in the running session bar:

- Capture identity, the target title, and all four scope/recording chips now occupy one bounded,
  shrinkable region. The target ellipsizes and chips clip inside that expendable region as width is
  removed.
- The elapsed clock, disabled Pause control, and Stop/Stopping control now occupy a separate
  `flex_none` terminal group. Scope metadata cannot consume their width.
- The three resizable meeting columns now sit in a `flex_1`, `min_w_0`, overflow-bounded wrapper;
  the collapsed Ask rail is fixed at its reserved 42px instead of being compressed by the columns.

Deterministic UI evidence:

- `narrow_running_workspace_keeps_stop_and_ask_rail_inside_the_window` renders a running session in
  a 680px-wide GPUI test window with a deliberately long target title. Painted bounds prove the
  clock, Pause, and Stop all retain positive width wholly inside the terminal group, the terminal
  group remains inside the window, and the Ask rail retains at least 42px inside the window.
- Full app suite: 105/105 passed.
- `cargo clippy -p app --all-targets --all-features --locked -- -D warnings`: passed.
- `cargo fmt --all -- --check` and scoped diff checks: passed.

Browser/manual visual acceptance remains **NOT RUN**. The regression establishes layout priority
at the reported narrow-width failure boundary; the maintainer still owns final visual acceptance.

## Highest-severity defect: mixed clocks in the timeline — 2026-08-13

Reported as "transcription is lost in between; I saw correct text live, then parts got cut off."
It is not a rendering fault. The record itself is being written with two different clocks.

A 41.2-second session produced events spanning 1.0s to 223,586.9s:

    utterance.partial  system  72   1.0      -> 223586.9
    utterance.partial  mic     11   26.1     -> 223585.6
    utterance.final    system   4   19.9     -> 223577.5
    vad                system  12   223547.5 -> 223582.3

The high values span 223,547 -> 223,587, a 39-second window that matches the session length offset
by roughly 223,547 seconds — about 62 hours, consistent with host uptime. Some events are stamped
session-relative and others absolute, **within the same stream**. VAD is entirely on the absolute
clock.

Two consequences, both observed:

1. **The transcript appears to lose text.** Rows sort by start time, so a final at 19.9s and one at
   223,555s are placed impossibly far apart. Text the user watched arrive live is not where the
   reader looks for it.
2. **Most speech never finalizes.** Four finals from 83 partials across 41 seconds of continuous
   speech. A commit policy that reasons over windows and agreement cannot behave sanely when
   timestamps jump by 62 hours, so the durable record is a fraction of what was said.

This is the canonical product artifact being corrupted, and it blocks T035: no readability verdict
means anything while the timeline carries two clocks.

Fix the origin at the capture seam so every event on every stream shares one monotonic
session-relative clock, and assert it: no event's timestamp may exceed the session's elapsed time.
That invariant is cheap, and it would have failed instantly here.

Note for T058: ADR-0018 makes transcript time equal media time, which subsumes this. That is the
durable fix, but it is blocked behind this task and the record is wrong today, so repair the origin
here rather than waiting.

## Mixed-clock repair — 2026-08-13

Root cause confirmed in the macOS capture seam: microphone `stream_offset` subtracted a CPAL-local
origin, while ScreenCaptureKit system audio forwarded its raw presentation timestamp directly.
That SCK value is host-uptime time, explaining the observed ~223,547-second offset. Screen frames
also exposed the native stream/host clocks.

Implemented one Rust-owned monotonic `SessionClock`, created exactly once at `MacCapture::start`:

- CPAL microphone packets, SCK system-audio packets, and captured screen frames all derive their
  timeline coordinate from `Instant::now() - session_origin`.
- Both public screen-frame time fields are session-relative at the Rust seam. Native SCK PTS and
  host time cannot enter downstream timeline producers.
- Raw native SCK PTS remains inside the Swift bridge for AVAssetWriter retiming; this change does
  not replace the media writer's per-input monotonicity guard.

Added two enforcement layers:

- Every live event is checked before app timeline ingress. The check covers the event envelope and
  all timestamp-bearing payload coordinates: utterance start/end, VAD start/end, and screen visible
  intervals. A coordinate beyond current session elapsed stops the session before the corrupt event
  is shown.
- After pipeline shutdown and persistence drain, the complete reloaded session is checked again
  before the recording/session is accepted as complete. A violation has its own truthful Timeline
  failure category rather than being mislabeled as capture permissions or storage access.

Forced regressions:

- `native_uptime_timestamps_cannot_enter_session_timeline_time` injects the observed
  `223,547s` native timestamp through the real SCK audio and frame callbacks and proves the queued
  coordinates remain bounded by the shared session elapsed time.
- `session_elapsed_invariant_rejects_the_observed_uptime_clock` builds the reported shape directly:
  41.2 seconds elapsed, a valid 19.9-second final, and a 223,547.5-second system VAD event. The
  session is rejected with the exact clock-invariant detail.

Automated evidence:

- Capture library: 8/8 passed; capture soak example: 2/2 passed.
- App: 106/106 passed.
- Strict Clippy passed for capture and app over all targets/features.
- Formatting and scoped diff checks passed.

A new signed live session after this clock repair is **NOT RUN**. T035 remains blocked until a live
record proves timestamps stay within elapsed time and continuous speech again reaches durable
finals; a UI readability verdict from the earlier mixed-clock record is invalid evidence.

## Live invariant rejection and ASR timestamp bound — 2026-08-13

The first signed live attempt after the mixed-clock repair did not establish acceptance. It stopped
after roughly 1.15 seconds with:

    Timeline clock invariant failed: event 1 (utterance.partial) reaches 8s,
    beyond session elapsed 1.153605791s.

This is distinct from the repaired host-uptime clock. The invariant caught a Whisper hypothesis
whose segment timestamp extended to 8 seconds even though only about 1.15 seconds of captured audio
existed. whisper.cpp decodes short input inside a padded context; its timestamp tokens were being
copied directly into the timeline without being bounded to the real samples supplied.

Implemented a narrow guard at the ASR decode seam:

- Each decode pass derives its exact available media duration from the supplied 16 kHz sample
  count.
- A hypothesis that begins inside real audio has its end capped to that duration.
- A hypothesis beginning wholly in Whisper's padded horizon, or becoming zero-length after the
  cap, is discarded.
- The app-level session-elapsed invariant remains strict and unchanged.

Forced regression:

- `padded_decode_timestamps_cannot_exceed_supplied_audio` feeds an 8-second model hypothesis over
  1.15 seconds of supplied audio and proves the retained segment ends at 1.15 seconds; a 7-to-8
  second padded-only segment is rejected.

Automated evidence:

- ASR library: 18/18 active tests passed; the existing 148 MB live-download/inference test remains
  ignored.
- App: 106/106 passed, including the session elapsed invariant.
- Strict Clippy passed for ASR and app over all targets/features.
- Formatting passed.

A signed live rerun after this ASR boundary fix is **NOT RUN**. The failed 1.15-second attempt is
valuable rejection evidence, not proof that the mixed-clock repair or transcript durability is
accepted. T035 remains blocked.

## Closure — 2026-08-13 (planner, narrowed)

Accepted. The recording writer, the session-to-media time mapping, SQLite v7 recording references,
the 20 GB budget with oldest-first pruning and safe deletion, the mounted Settings library, the
session-bar recording copy, the checked annotation constructor inherited from T056, and the repairs
found during live debugging — `.complete`-frame filtering, per-input PTS monotonicity, mono-48k
audio canonicalization, real failure causes carried across the FFI, and the mixed-clock fix — are
all delivered.

Two acceptance items are recorded as **observed** rather than re-run, on the maintainer's live
signed bundle on 2026-08-13:

- **Live signed capture.** A 31.7s session produced a playable 30.97s, 8.8 MB recording with
  session-relative timestamps and an intact time mapping.
- **Session-bar visual acceptance.** The maintainer confirmed the terminal controls no longer
  overflow at their window width. Narrow-width proof remains T060's.

Residual transferred to **T061**: the fragmentation decision, the crash-prefix acceptance that
depends on it, and the simulated disk-full acceptance. `crates/capture/**` releases to T061.

Planner-made edits during debugging are itemised above and are inherited by T061 with the crate,
including the `SOTTO_RECORDING_FRAGMENT_SECONDS` switch, which T061 must resolve rather than leave
as a debugging leftover.
