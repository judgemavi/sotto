# T064 — The signed recording acceptance gate

**Status:** todo

**Wave:** M4 — recording

**Depends on:** nothing further. T062's recording seam and T061's audio amplitude repair have both
landed; T062's own outstanding items are cosmetic and do not affect these cases.

**Owns:** `docs/experiments/recording-acceptance.md` and this task. **No production code.** Every
defect found here is filed against the owning task.

**Run by:** the maintainer, at a real machine, against a signed bundle. An agent cannot do this —
it needs a granted TCC permission, a real meeting, and a human judging whether the transcript is
right.

## Why this exists

Five acceptance cases across T058, T059 and T061 all need the same thing: one signed capture run on
real hardware. They were being carried separately, which meant each task closed with its own NOT RUN
residual and no single place recorded whether the recording feature actually works. This is that
place.

Prerequisite, once: create a self-signed Code Signing certificate in Keychain Access and export
`SOTTO_DEV_IDENTITY` to its name. Without it every rebuild is ad-hoc signed, the code hash changes,
and macOS demands a fresh Screen Recording grant for each of the runs below.

## The cases

### From T061 — durability

Build and bundle the soak harness (`cargo build --release -p capture --example soak`, then
`scripts/dev-bundle.sh target/release/examples/soak`). Note this replaces `target/Sotto.app`, so do
the product cases either side of it, never interleaved — a previous "verified" run was invalidated
exactly that way.

1. **Normal capture.** A 60-second run produces a playable recording whose probe decodes both the
   first video frame and a frame after seeking near the end.
2. **Abrupt loss.** `SOTTO_CAPTURE_CRASH_AFTER_SECONDS=30`, then `SOTTO_RECORDING_PROBE_ONLY=1`.
   The retained prefix must be playable and seekable, losing under six seconds of tail.
3. **Simulated disk full.** `SOTTO_RECORDING_MAX_BYTES=2000000`. The session must stop with a
   terminal failure naming the byte cap, not "the disk is full", and leave no corrupt file.

### From T058 — transcription

4. **Media-timestamp tolerance.** Utterance timestamps match media timestamps within a stated
   tolerance, checked against the real recording rather than asserted.
5. **Overloaded catch-up.** A deliberately loaded run loses no speech, only time. **Measure the
   realtime factor**: note the last transcript timestamp against the true meeting length. A
   2026-08-13 observation suggested the transcript may have been running roughly two minutes behind
   a ten-second lag, which if real means unbounded backlog rather than bounded lag. Settle this
   with a number.
6. **Transcript quality.** Against a meeting whose platform transcript is available, judge whether
   the record is usable. This is a human judgement and it is the actual product test.

### From T059 — frame extraction

7. **Known-content decode.** Request a moment whose on-screen content is known, and confirm the
   returned frame shows it and reports its actual decoded timestamp alongside the requested one.
8. **Missing media.** Delete the recording and confirm the transcript stays fully reviewable while
   inspection returns the explicit missing state.

### From T062 — the seam

9. Recording survives a failed finalization; the meeting lists while live; re-transcription is
   reachable and works.

## Acceptance

- Every case above is recorded as PASS or FAIL with the observed evidence — not "ran successfully".
- Each FAIL is filed against its owning task with the observation attached.
- The recording feature is declared shippable, or not, in one sentence with reasons.

## Out of scope

Fixing anything found here. This gate observes and files; the owning tasks fix.

## Added case — 2026-08-13

10. **The recording is audible.** Play a cleanly finalized recording and confirm **both** channels
    at normal volume: the meeting audio on the left, the microphone on the right. This was the
    defect that nearly shipped — whisper's normalization recovered a near-silent meeting channel, so
    the transcript looked correct while the recording was unlistenable, and no automated test could
    have caught it. A human listening is the only check that would have.
