# ADR-0018: Retain the session recording and transcribe it behind the capture

- Status: Accepted
- Date: 2026-08-13
- Decision owners: Sotto maintainers

## Context

Until now the pipeline was a pure conveyor belt. Audio frames flowed through VAD into a
sliding-window transcriber and were discarded; screen frames were captured by the bridge and
dropped entirely, since `crates/screen` was never wired into the product. Nothing but the timeline
survived a session.

Three problems came from that, all observed in a real signed-bundle session on 2026-08-13:

1. **The record ends mid-sentence.** Capture stops while the last audio window is still
   un-inferred, so the final utterance never settles. `WhisperTranscriber::drain()` exists for
   exactly this and is called only by the CLI harness.
2. **Back-pressure destroys speech.** The transcriber consumes a ring buffer. If inference falls
   behind, audio is overwritten and those words are unrecoverable.
3. **Screen evidence does not exist.** The session bar states `screen: <target>` while every frame
   is discarded. The `screen.snapshot` events and content-addressed frame store described in
   AGENTS.md and ADR-0006/0009 were designed, built as a crate, and never connected. The reasoning
   layer's `inspect_screen(timestamp | event_id)` contract has nothing to read.

The realtime sliding-window design also carries permanent complexity: a LocalAgreement stabilizer,
a 2.5s unstable tail, rolling hypotheses that supersede each other, and every downstream consumer
that must cope with an anchor dissolving underneath it. Several defects this week were that
machinery, not the features built on it.

The product does not need instant text. Notes have no realtime latency budget, and the maintainer
has stated a preference for lag over immediacy in exchange for accuracy and exact timestamps.

## Decision

**The session recording is the source of truth. Transcription is a consumer of it, running behind
capture. Screen evidence is extracted from it on demand, never by default.**

- Capture writes audio and video for the chosen target to a local session recording. Writing the
  recording is the only realtime obligation on the capture path.
- Transcription reads the recording N seconds behind the write head, N configurable. It emits
  finals with media-relative timestamps. Because it is no longer racing the speaker, it does not
  need to guess: the LocalAgreement stabilizer, the unstable tail, and rolling partial supersession
  are no longer required to produce the record.
- **Transcript time is media time.** There is one clock. A citation at 02:18 addresses 02:18 of the
  recording exactly, which is what makes on-demand extraction trustworthy.
- Screen frames are **not** retained separately and **not** OCR'd by default. When the reasoning
  layer invokes the existing `inspect_screen` contract, a frame is decoded from the recording at
  the requested moment, and only then may OCR run locally.
- Recordings are **visible, measurable, and deletable** by the user, under a disk budget with
  automatic pruning of the oldest recordings. Default budget 20 GB, user-raisable. Recording is on
  by default for a captured session; pruning and deletion are the user's controls.
- The indicator must state that a recording is being kept while capture runs. Describing screen
  capture while discarding it, as the shipped build does, is the failure this replaces.

This authorizes retaining audio and video **on the user's device only**. It does not weaken any
disclosure rule: audio and image bytes still never leave the machine except through the existing
explicit opt-in paths, and a reasoning request still carries transcript text unless the user has
separately authorized an inspected image.

## Consequences

- **Recoverable transcription.** The recording can be re-transcribed with a larger model or a fixed
  commit policy. Today's misrecognitions and stranded tails become re-derivable rather than
  permanent.
- **Back-pressure becomes latency, not loss.** A loaded machine falls further behind and catches up.
  No speech is dropped.
- **A bigger model becomes affordable.** `small.en` or `medium.en` is viable where `base.en` was a
  latency compromise. The default should be re-benchmarked once lag is in place.
- **One ASR path.** The CLI harness already transcribes files. The product converges on the code
  path the harness exercises, instead of running a second streaming implementation that tests do not
  cover.
- **The change-frame design is superseded.** `crates/screen`'s bounded content-addressed frame store
  is no longer the retention mechanism. Removing it is a later cleanup, not a ship gate. ADR-0006
  and ADR-0009 keep their transcript-first and explicit-inspection rulings; only the frame retention
  mechanism changes.
- **T052's unstable-strip machinery largely retires.** It was correct for the streaming design. The
  pacing buffer remains useful for readable delivery of batched finals.
- **Disk becomes a real cost.** Roughly 1-3 GB per video hour. The budget, pruning, visible size
  accounting, and deletion are part of the feature, not follow-up polish.
- **The privacy claim changes and must be restated honestly.** "Audio is never written to disk" is
  no longer true. What remains true, and what the UI must say, is that the recording stays on this
  Mac, is visible, and can be deleted. This is a deliberate trade and it is the strongest claim the
  product may now make.
- **Live proposals are foreclosed while the lag stands.** A watcher reading transcripts N seconds
  late cannot meet the sub-second post-pause budget. Proposals are the explicitly optional third
  tier, so this is deferred, not lost — and a fast supplementary pass can be added later purely for
  the live surface.

## Revisit if

- Live proposals become a ship requirement, which would need a second low-latency pass.
- Measured disk consumption or pruning behaviour makes default-on recording untenable for real
  users.
- Frame extraction from the recording proves materially less accurate at a cited moment than a
  retained change frame would have been.
- A user-facing requirement appears for a session that is transcribed but explicitly never recorded,
  which would make recording per-session opt-in rather than default.

## References

- ADR-0002, ADR-0005, ADR-0006, ADR-0009, ADR-0012
- T057, T058, T059

## Amendment: fragmented recording durability (T061, 2026-08-13)

The retained MP4 is written from application-owned fragmented-media segments. Sotto constructs
`AVAssetWriter` with a content type and no output URL, selects the Apple HLS MPEG-4 profile, and asks
for five-second segments. Its delegate appends the initialization segment and each complete media
segment to the one session MP4, synchronizing the file after every delivery.

The Apple HLS segment profile permits video plus only one audio track. A signed live start with the
previous two-mono-track layout was rejected before capture with `AVFoundationErrorDomain -11875`:
“More than one audio track is not allowed.” A local AVFoundation configuration probe confirmed the
same HLS restriction and also rejected the CMAF-compliant profile because that profile permits only
one track total. The recording therefore carries one stereo AAC track with a fixed, Sotto-owned
mapping: channel 0/left is selected-target meeting audio; channel 1/right is the local microphone.
Both inputs are normalized to 48 kHz mono and combined into 100 ms stereo chunks behind a 500 ms
callback-reordering window. A missing channel is committed as silence, preserving channel identity
without allowing callback skew to stall the recording. T058 must split those fixed channels when
it consumes the growing recording; deterministic speaker attribution is unchanged.

This resolves the previously implicit growing-file assumption. A reader may reopen the MP4 and
consume through its last committed fragment while capture continues; after stop it consumes through
the finalized end. An abrupt process or power loss preserves a playable, seekable prefix through the
last segment. The durability tolerance is therefore six seconds: the five-second interval plus one
second for boundary and timestamp rounding. Normal stop still finalizes the tail, so this tolerance
applies only to abrupt loss.

The output-URL writer's `movieFragmentInterval` mode is rejected. Real capture failed at the first
fragment boundary with `NSOSStatusErrorDomain -16341` at both 0.5 seconds and the later 1s-initial /
10s-steady configuration. Apple documents a separate segment-output mode for live material: the
writer delivers an initialization segment and separable media segments to its delegate, and those
bytes may be stored together in one file. Owning the append and synchronization boundary avoids
asking the confirmed-broken writer mode to rewrite the destination in place. No fragmentation
environment switch remains.

A raw PCM sidecar was rejected. It would make incremental audio simple, but it would create a second
privacy-sensitive recording outside the existing session reference, byte accounting, pruning, and
deletion contract. A sidecar cannot be introduced honestly without widening those contracts, and it
is unnecessary if the delegate-segmented MP4 survives the required real-capture evidence.

The capture evidence probe must decode the first video frame and a frame after seeking near the
committed end. For an abrupt-loss run it also compares the decoded duration with a synchronously
persisted elapsed-time witness. Simulated disk-full evidence must retain the explicit injected-byte
limit in the surfaced failure instead of inferring that every writer failure is a full disk.

## Amendment: the segment interval is the transcript's latency floor (2026-08-14)

The segment interval was chosen in T061 purely as a durability parameter. It is also, and more
visibly, a latency parameter: a growing recording exposes only whole committed segments, so no
reader can ever be closer to live than one interval. With T058's ten-second read lag and a
ten-second Whisper window stacked on top, the first transcript line appeared twenty to twenty-five
seconds after the words were spoken, and the product read as broken rather than lagged.

The interval is now **two seconds**, the read lag sits exactly on it, and the Whisper window is five
seconds. First text lands seven to nine seconds in. Both remaining knobs are named and commented
where they are defined, because the delay is a product decision and was previously discoverable only
by summing three unrelated constants.

Shortening the interval **tightens** durability rather than trading it away: the abrupt-loss window
is the interval, so the tolerance stated above falls from six seconds to three. Nothing about the
delegate-segmented design changes; only its period does. The rejection of `movieFragmentInterval`
above still stands and is unrelated — that failure was in the output-URL writer mode, not in
segment output, and is not evidence about interval length.

The Whisper window is where further reductions get expensive. whisper.cpp encodes a mel padded to
thirty seconds regardless of how much audio it is handed, so inference cost per window is
near-constant and halving the window roughly doubles cost per second of audio. The windows are also
non-overlapping and carry no acoustic context across their boundary, so a sentence spanning two
windows is decoded twice, blind each time. Below five seconds this buys latency with accuracy at a
steeply worsening rate. If the delay must fall further, the answer is an overlapped window that
infers a longer span than it commits — not a shorter one.
