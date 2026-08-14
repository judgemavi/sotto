# T061 — Decide fragmentation, and prove the recording survives interruption

**Status:** in-progress

**Wave:** M4 — recording

**Depends on:** T057 accepted, which releases `crates/capture/**`.

**Owns:** `crates/capture/**`, `docs/adr/0018-recording-as-source-of-truth.md` for an amendment
section if the durability guarantee changes, and this task

## Why this exists

T057 delivered a working recording writer, but left one architectural question open and two
acceptance scenarios unrun. They are separated here so T057 could close on what it actually proved.

`movieFragmentInterval` is currently **disabled by default**, behind the
`SOTTO_RECORDING_FRAGMENT_SECONDS` switch the planner added while debugging. That is a diagnostic
state, not a decision, and it must not survive as a leftover environment variable.

It matters because the two states trade against each other:

- **Fragmented MP4** writes periodic fragments, so an abruptly terminated file is still playable up
  to the last fragment. That is what makes T057's crash-prefix guarantee true.
- **Unfragmented MP4** writes its `moov` atom at `finishWriting`. Kill the process and the file is
  unplayable — the whole recording is lost, not just the tail.

At a 0.5s interval, fragmentation also caused every capture to fail on a fragment boundary with
`NSOSStatusErrorDomain -16341`. So the guarantee and the working configuration are currently in
direct conflict, and the product is shipping without crash resilience while nobody has decided that.

## T058 raised the stakes — read this before deciding

T058 stopped on 2026-08-13 because it has nothing to read. Unfragmented MP4 becomes
reader-addressable only at `finishWriting`, and ADR-0018's architecture requires transcribing a
recording **while it grows**. So incremental readability is now a hard product requirement, not only
a crash-resilience nicety, and it narrows the options below: **withdrawing the guarantee is no longer
available**, because it would leave T058 with no input at all.

A third option is now on the table and may be the cleanest: **a committed-audio sidecar.** Write raw
PCM alongside the MP4. A growing PCM file is trivially readable incrementally, gives T058 exactly the
input it needs with media timestamps, and leaves the MP4 to do what only it can — hold video for
T059's frame extraction and be played back. It decouples "readable while growing" from "durable
container", instead of asking one format to be both.

## What to decide

Pick one, and record the reasoning where a future reader will find it:

1. **Fragmentation returns, working.** Find why the flush failed — a longer interval, a different
   file type, or a track that cannot satisfy a boundary — and restore the crash-prefix guarantee.
2. **Fragmentation goes, guarantee replaced.** Deliver crash resilience another way: periodic
   `finishWriting` into segment files, a sidecar index, or an explicit recovery pass on next launch.
3. **A committed-audio PCM sidecar**, with the MP4 unfragmented and finalized at stop. The sidecar
   satisfies T058's incremental read and the crash guarantee for audio; decide explicitly what
   happens to video on an abrupt kill, and say so rather than implying durability that is not there.

Whichever is chosen, amend ADR-0018: it currently assumes a growing readable recording without
saying what makes it readable. Withdrawing incremental readability entirely is not available while
T058 exists.

## Plan

1. Make the decision above and record it in the ADR.
2. Remove `SOTTO_RECORDING_FRAGMENT_SECONDS` or promote it to real configuration. It must not remain
   a debugging leftover in shipped code.
3. Run the crash-prefix acceptance: kill the process mid-session, then prove the file is playable and
   seekable, and that its duration matches the elapsed capture within tolerance.
4. Run the simulated disk-full acceptance: fill the budget mid-session and prove the session stops
   with a truthful reason, the committed prefix survives, and no partial file is left corrupt.
5. Confirm the failure copy still names the real cause. Both scenarios previously reported "the disk
   is full" for causes that were not the disk.

## Acceptance

- The fragmentation decision is recorded in ADR-0018 with its reasoning, and the code matches it.
- No debugging environment switch remains in the shipped recording path.
- Killing the process mid-session leaves a playable, seekable file whose duration matches the
  elapsed capture within a stated tolerance — or, under option 3, the loss is explicit in the ADR,
  the acceptance, and the UI.
- A simulated disk-full stops the session with a truthful reason and leaves no corrupt file.
- Per the verification rule, both scenarios are exercised against a real recording, not simulated at
  the type level.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Transcription (T058), frame extraction (T059), microphone-only capture (T050), and any change to the
retention budget or library UI.

## Implementation — 2026-08-13

Chose application-owned fragmented segment append after the live 1s/10s result rejected the initial
implementation. `AVAssetWriter` now has no output URL; it emits an initialization segment and
five-second separable media segments through `AVAssetWriterDelegate`. Sotto appends those deliveries
to the single retained MP4 and synchronizes every segment before it is considered committed. This
uses Apple's documented live-segment path instead of the confirmed-broken output-URL
`movieFragmentInterval` mode. The `SOTTO_RECORDING_FRAGMENT_SECONDS` diagnostic switch is removed.

The recording probe now proves seekability by decoding both the first video frame and a frame after
seeking to one second before the reported media end. The abrupt-loss harness synchronously records
its elapsed time immediately before `_exit`; probe-only mode rejects a playable prefix whose lost
tail exceeds six seconds. The disk-full harness retains the observed native error and requires
both terminal `Failed` status and the exact `SOTTO_RECORDING_MAX_BYTES` cause before accepting the
run.

The delegate-segment implementation still requires the same real ScreenCaptureKit crash and
disk-full evidence. Compilation cannot establish media durability.

## Verification before delegate-segment replacement — 2026-08-13

- Focused capture tests passed: 8 library tests and 2 soak-example tests.
- Strict capture Clippy passed over all targets and features.
- Full locked workspace tests passed. Declared model/live/performance tests remained ignored.
- Strict full-workspace Clippy passed over all targets and features. The first sandboxed attempt was
  blocked only because Metal could not write its user module cache; the approved-environment rerun
  passed.
- Formatting and the T061-scoped diff check passed.
- The release soak example built and was wrapped in an ad-hoc signed `com.sotto.app` bundle.

Real crash-prefix and simulated disk-full acceptance remain **NOT RUN**. Both attempts to start the
newly signed bundle stopped before the picker because macOS reported Screen & System Audio Recording
permission denied. The app requested permission, but the grant was not active on retry. Grant the
new bundle in System Settings, restart it, then run the abrupt-loss/probe pair and disk-full case;
do not treat compilation or the permission rejection as media durability evidence.

## Real 1s/10s fragmentation result — rejected, 2026-08-13

The maintainer's live signed run rejects the fixed-interval decision above. The writer failed at
`11.113269417s`, exactly the first ten-second steady-state boundary after the one-second initial
fragment:

    video: input.append returned false, pts=11.113269417s
      | AVFoundationErrorDomain code=-11800
      | underlying: NSOSStatusErrorDomain code=-16341

A second run observed writer status `.failed` with the same underlying `-16341`. This reproduces
the earlier 0.5-second failure at a longer interval and establishes that the current output-URL
fragment-writing mode is broken, not merely over-flushed. Do not tune the interval again.

The simulated 2,000,000-byte limit did retain its truthful native cause:

    system audio 48000Hz/1ch: SOTTO_RECORDING_MAX_BYTES cap of 2000000 reached

That proves error attribution only. The commands ran the normal app bundle after `scripts/run.sh`
replaced the soak harness, so neither an abrupt `_exit` nor the independent seek/duration probe ran.
Crash-prefix and disk-full prefix durability remain **NOT RUN**.

The 1s/10s ADR amendment is superseded before acceptance. Next investigation is AVAssetWriter's
delegate-based segment output, which emits initialization/media segment bytes for application-owned
durable append instead of asking the failing output-URL writer to fragment in place.

## Delegate-segment automated evidence — 2026-08-13

- The Swift bridge compiles with the no-output-URL writer, Apple HLS MPEG-4 profile, delegate
  segment delivery, application-owned append, and per-segment file synchronization.
- Focused capture tests passed: 8 library tests and 2 soak-example tests.
- Strict capture Clippy passed over all targets and features.
- Full workspace tests, full workspace Clippy, and the real capture cases have not yet been rerun
  after this replacement. Earlier green results above apply to the now-rejected 1s/10s code.

The permission retry exposed a separate capture-seam defect: the normal app checked permission only
after picker selection and stream startup, but never called `CGRequestScreenCaptureAccess`. Resetting
TCC therefore produced no prompt and startup immediately reported `Denied`. Both async product and
blocking soak picker entry points now ensure permission before presenting the system picker. Focused
capture tests and strict capture Clippy remained green after the repair.

The first permitted delegate-segment live start then exposed an AVFoundation profile constraint,
not a ScreenCaptureKit failure. `AVAssetWriter.startWriting()` rejected the existing video plus two
mono audio inputs with `AVFoundationErrorDomain -11875`: “More than one audio track is not allowed
for file type profile MPEG4AppleHLS.” A local native configuration probe reproduced that exact HLS
failure and established that `.mpeg4CMAFCompliant` is not an alternative: it rejects more than one
track total.

The segment writer now uses video plus one 48 kHz stereo AAC track. Channel 0/left is selected-target
meeting audio and channel 1/right is the microphone. A tested 100 ms chunk mixer holds 500 ms for
callback reordering, fills a genuinely missing channel with silence, and flushes the partial tail on
normal finalization. This preserves deterministic channel attribution while satisfying the only
multi-media topology accepted by Apple's live HLS segment profile.

Post-change automated evidence:

- Swift bridge build passed; its two mixer tests passed.
- Focused capture tests passed: 8 library tests and 2 soak-example tests.
- Strict capture Clippy passed over all targets and features.
- Rust formatting passed.

The replacement still needs the signed real normal, abrupt-loss/probe, and disk-full runs. Those
media acceptance cases remain **NOT RUN** for the stereo delegate-segment writer.

## Independent corroboration from T058 — 2026-08-13

T058 built a reader against this writer and its native tests demonstrate that an `AVAssetReader` can
reopen the session MP4 and consume committed audio **while the writer is still active**. That
confirms the incremental-readability half of this decision — the half T058 was blocked on — without
the signed run. It does **not** confirm durability. The abrupt-loss prefix and the disk-full case
are still the only evidence that the six-second tolerance and the truthful failure cause are real,
and both remain NOT RUN. Do not let the readability result be reported as durability.

## Suspected: finalized recordings may lose their audio track — 2026-08-13

The maintainer played a retained MP4 and heard **no audio**, while the same session had produced a
full, accurate transcript.

That combination is diagnostic rather than ambiguous. `LiveRecordingTranscriber::push`
(`crates/asr/src/recording.rs:125`) is a deliberate no-op — capture audio is explicitly *not* an ASR
input, and committed recording media is the only transcription source. So the transcript proves the
audio was present in the growing file and readable from it. The absence is introduced somewhere
between the growing form and the file on disk, not at capture.

**Confound to eliminate first.** Both examined recordings had finalization fail with the pre-fix
probe error (`recording is not playable`), so their final segments may never have been appended.
A failed finalize losing the tail is not the same defect as a healthy finalize losing a track. A
fresh capture with the corrected probe distinguishes them, and that run is the next step.

**If a cleanly finalized recording also has no audio**, this is the most serious open defect in the
project and takes priority over everything else in flight: re-transcription becomes impossible,
T065 has no input, and ADR-0018's central promise — that the recording is the source of truth and
today's misrecognitions stay re-derivable — is unfulfilled. The likely area is the application-owned
append of the initialization and media segments into one MP4, where the audio track's presence in
the finalized container is Sotto's responsibility rather than AVAssetWriter's.

Note also that Sotto's own reader accepting the file is not evidence the container is correct. The
reader consumes what it expects; QuickTime reads the container as written. Where they disagree, the
container is wrong.

## Confirmed defect: the meeting channel is written at near-zero amplitude — 2026-08-13

Supersedes the "no audio track" suspicion above; that framing was wrong.

Evidence from a cleanly finalized 2:01 recording (`afinfo`): one stereo AAC track, 121.17s against
a 2:01 video, 48 kHz, stereo L R, ~188 kbps, no structural fault. The container is fine.

Playback **with the maintainer wearing headphones**:

- microphone channel — audible at `afplay -v 20`
- meeting channel — inaudible
- whisper transcribes the meeting content from the same file accurately

Headphones eliminate the alternative explanation. The microphone could not have picked up the
meeting audio, so the meeting signal must be present in the file and merely tiny; whisper recovers
it by normalizing, a player cannot. The two inputs are therefore converted asymmetrically somewhere
between each source's 48 kHz mono normalization and the interleaved stereo buffer handed to the AAC
input — ScreenCaptureKit delivers Float32, and the CPAL microphone path may not.

Diagnose by measurement before editing: peak and RMS per channel for the finalized recording, and
the same at each capture callback before mixing. The per-channel ratio between capture and file is
the bug, and it should be reported as a number.

The regression test must assert **amplitude**, not identity. The existing mixer tests assert channel
identity and pass happily on inaudible samples, which is how this survived this task's automated
evidence, T058's full implementation, and every workspace test run since.

### Why this was nearly shipped

The transcript was good, so the recording looked good. Whisper's normalization concealed a defect
that would have produced a product whose recordings nobody can listen to — and because
re-transcription reads the same normalized path, it would have kept working, so no automated test
would ever have failed. The lesson is narrow and worth keeping: **a derived artifact succeeding is
not evidence its source is correct.** The same mistake appeared twice today, when Sotto's own reader
accepted a recording whose probe rejected it.

## Channel amplitude diagnosis and repair — 2026-08-13

Measurement preceded the functional change. A deterministic reproduction of the actual startup
order gave the microphone a 600 ms lead, beyond the mixer's 500 ms reorder window. Before the fix,
meeting callback RMS was `0.25` and meeting mixed-buffer RMS was `0.0`: callback-to-mix ratio
`0.000000000`. The global commit frontier was advanced by CPAL before ScreenCaptureKit delivered its
first audio callback, so later zero-based meeting chunks were discarded as already committed.

The maintainer's next 43.497 s retained MP4 did not reproduce low meeting amplitude in the encoded
file. Independent AVFoundation decoding measured:

- meeting/left: peak `0.945038140`, RMS `0.075202144`;
- microphone/right: peak `0.007665577`, RMS `0.001820400`;
- meeting-to-microphone ratio: `123.283361101` peak and `41.310787891` RMS.

`afinfo` reports one 48 kHz stereo AAC track at about 188 kbps with the expected Stereo `(L R)`
layout. Therefore that run's reported silent playback is not an absent or low-amplitude meeting
signal; channel 0 was also independently decoded to a mono WAV. Playback/container consumption
remains a separate real-device observation to resolve, and is not papered over as an amplitude fix.

The mixer now advances its live commit frontier from the minimum observed end of the meeting and
microphone channels. One source cannot commit the other source's pending interval as silence merely
because its callback arrived first. Missing packets inside a progressing stream remain silence, and
the longer partial tail still flushes at finalization.

The regression uses the same 600 ms microphone startup lead and asserts meaningful amplitude on
both mixed channels: meeting peak/RMS at least `0.25`/`0.20`, microphone peak/RMS at least
`0.5`/`0.40`. It failed after the first partial repair with meeting RMS `0.0490290338`, then passed
after the commit frontier became per-channel.

Focused evidence after the repair: all four Swift bridge tests passed, including the amplitude
regression; all eight capture library tests and both soak-example tests passed; strict capture
Clippy over all targets passed. A fresh signed recording against the repaired mixer and audible
playback acceptance remain **NOT RUN**.

## Post-repair signed amplitude evidence — 2026-08-13

The fresh signed 44.649 s run (`1786657142000312000.mp4`) closed as an `available` recording and
measured every requested boundary in the same process:

- meeting: callback peak/RMS `0.774512649`/`0.069313391`, mixed
  `0.774512649`/`0.069157994`, decoded AAC `0.774228811`/`0.069106854`;
- microphone: callback peak/RMS `0.013793945`/`0.002088385`, mixed
  `0.013793945`/`0.002087583`, decoded AAC `0.013717992`/`0.002086099`;
- callback-to-file RMS preservation: meeting `0.997020243`, microphone `0.998905083`.

The writer therefore preserves both channels through the mixer and AAC encoder. `afinfo` reports
48 kHz stereo AAC, about 188 kbps, with Stereo `(L R)` layout. The maintainer still reports no
audible playback, so playback acceptance remains failed, but the measured failure boundary is now
strictly downstream of the stored AAC samples. Do not relabel it as capture attenuation or add gain:
the meeting channel already peaks at `0.774` and artificial gain would clip it.

Routine whisper.cpp/GGML backend inventory is now suppressed in normal product runs by installing
whisper-rs logging hooks before the first context is loaded. `SOTTO_WHISPER_LOGS=1` preserves the
native stderr output when Metal/backend diagnosis is explicitly needed; typed model-load and
inference failures remain visible either way.

## Clean-stop playback finalization — 2026-08-13

The maintainer confirmed that a mono WAV decoded from the latest file's meeting/left channel is the
correct audible meeting audio. This closes the remaining ambiguity: capture, channel mixing, AAC
encoding, and channel assignment are correct, while direct playback of the retained HLS-fragmented
MP4 is not.

The writer continues appending and synchronizing Apple-HLS fMP4 segments during capture so abrupt
loss retains a readable committed prefix. Only after a clean stop, successful `finishWriting`, and
the segment sink's final synchronization does it use AVFoundation passthrough export to construct a
conventional MP4. The fragmented source remains canonical throughout export; the completed output
is synchronized and atomically renamed over it. Export failure is a typed recording failure and
does not replace the durable source.

The native regression creates a real segmented stereo AAC fixture, proves its input contains
`moof`, runs the production clean-stop finalizer, then proves the result contains `moov` and `mdat`
with no `moof`. AVAssetReader decoding also asserts meaningful amplitude in both channels (meeting
peak/RMS above `0.15`/`0.10`, microphone above `0.30`/`0.20`). All five Swift bridge tests pass;
all eight capture library tests and both soak-example tests pass; strict capture Clippy over all
targets passes. Fresh signed-app playback of a newly finalized recording remains **NOT RUN**.

### Playback routing correction

That first finalizer still failed maintainer playback: YouTube was absent while the microphone was
audible. The new file's own diagnostic ruled out signal loss again: decoded meeting RMS was
`0.112190147` versus microphone RMS `0.002068320`, so the supposedly absent source was about 54x
stronger in the file. `afinfo` showed the final stereo AAC track had no explicit channel layout, but
the deeper defect was relying on hard-panned storage (meeting only left, microphone only right) as
the default human playback surface. A player or output route rendering one side makes one participant
disappear even though sample-oriented tests and transcription remain green.

Clean-stop finalization now writes two audio tracks. The first/default track is a centered playback
mix containing meeting plus microphone identically on left and right. The second retains the exact
isolated convention, left=meeting and right=microphone, and the recording ASR bridge deliberately
selects that last track when present; one-track growing/crash-readable recordings retain the old
selection behavior. Video remains compressed passthrough while audio is decoded and re-encoded once
to create these two products.

The capture regression now proves the default track has equal non-trivial RMS on both sides and the
second track retains distinct non-trivial source amplitudes. The ASR native fixture contains a
centered first track and isolated second track, and proves the reader returns the latter. Five native
capture tests, two native recording-reader tests, ten focused Rust capture tests, nineteen focused
Rust ASR tests plus two example tests pass (one network/model test ignored), and strict combined
capture/ASR Clippy passes. Fresh signed-app playback remains **NOT RUN**.

Maintainer acceptance on 2026-08-13: a newly recorded MP4 produced by the two-track finalizer plays
the meeting audio successfully. This closes the signed-app playback failure. It does not substitute
for the still-separate abrupt-process-loss and disk-full durability scenarios.
