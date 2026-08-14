# T065 — Re-run the model benchmark on audio that can actually tell the models apart

**Status:** blocked

**Wave:** M4 — recording

**Depends on:** the maintainer supplying one reference transcript. Recordings are now durably
retained from capture start, so real meeting audio exists on disk under
`~/Library/Application Support/Sotto/recordings/`. Extract the meeting channel from a retained MP4
rather than asking for a separate audio file — channel 0/left is the meeting, channel 1/right is
the microphone, per ADR-0018's amendment.

**Concurrency (planner, 2026-08-13):** T062 runs alongside and holds `crates/app/**` and the
recording probe in `crates/capture/**`. Do not edit either. This task reads recordings; it does not
change how they are produced.

**Owns:** `crates/asr/**`, `crates/asr/examples/model_benchmark.rs`,
`docs/experiments/asr-model-benchmark.md` by sequential handoff from T063, and this task

## Why this exists

T063 executed its plan correctly and documented it honestly, including its own caveat that the
sample was "enough to compare this observed case, not enough to claim a general accuracy ranking".
The reviewer's objection is not with the execution but with what the result can support.

The measured input was whisper.cpp's `samples/jfk.wav`: **11 seconds, 22 words**, of clean,
close-miked, single-speaker studio English. All three models returned the identical hypothesis with
the identical single insertion, so WER was 4.55% for `base.en`, `small.en` and `medium.en` alike.

T063's own decision rule was "accuracy chooses among eligible models; load time and peak memory
break a tie." Accuracy did not choose anything — the sample had no power to separate the candidates,
so the decision fell through entirely to the tiebreak. **`base.en` was kept by default, not by
measurement**, and the experiment cannot distinguish "the larger models add nothing" from "this
sample cannot detect what the larger models add."

That distinction matters here specifically. The maintainer's real 2026-08-13 meeting produced errors
of exactly the kind model size addresses — "labeled" heard as "a play", "Which one?" as "world which
one" — on multi-speaker, multi-accent, compressed conference audio with crosstalk. None of those
conditions are present in the measured sample.

The throughput result, by contrast, is genuine and decisive, and it is what makes this worth
redoing: `medium.en` ran at **3.62x realtime** on this hardware. Under ADR-0018 the recording is
transcribed behind the capture, so a model at 3.6x is entirely affordable — the constraint that
originally selected `base.en` is gone. Peak RSS of 1.7 GB on a 16 GB machine is the real cost to
weigh, and it should be weighed against a measured accuracy gain rather than against no measurement.

## Plan

1. Assemble a meeting-representative input: at least several minutes of real captured meeting audio
   with multiple speakers, at least one non-native accent, conference-codec compression, and some
   crosstalk. The maintainer's captured sessions with platform transcripts are the natural source.
2. Establish the reference transcript honestly. A meeting platform's own transcript is a usable
   reference but is not ground truth — it has its own errors. Say how the reference was produced and
   corrected, or the WER numbers mean nothing.
3. Measure WER per model on that input, with the same controlled method T063 already built. Reuse
   `crates/asr/examples/model_benchmark.rs` rather than writing a second harness.
4. Report where the models actually differ: proper nouns, domain jargon, accented speech, and
   overlapping speech are the segments worth calling out individually, since a whole-file WER can
   hide a large gain on the words a reader most needs correct.
5. Decide the default on that evidence. Changing it is a real possibility now; so is keeping
   `base.en`. Either is acceptable if the accuracy numbers separate the models. What is not
   acceptable is a second tie reported as a decision.
6. If `medium.en` wins on accuracy, state the memory consequence plainly for a 16 GB machine and
   say whether the default should depend on installed memory.

## Acceptance

- WER per model on multi-speaker, real meeting audio, with the reference's provenance and its own
  error characteristics stated.
- Per-category observations for proper nouns, jargon, accent and crosstalk.
- A default chosen on measured accuracy, or an explicit statement that the models remain
  indistinguishable on representative audio too — which would then be a real result.
- `docs/experiments/asr-model-benchmark.md` extended, not replaced. T063's numbers stay; this is an
  additional experiment on a harder input, and the reasoning for superseding its conclusion must be
  visible.
- Per the verification rule, real captured meeting audio, not a public sample or synthesized speech.

## Result (2026-08-13)

Blocked on the two acceptance inputs this task explicitly requires. The two retained recordings in
`~/Library/Application Support/Sotto/recordings/` both remain `growing` in the durable receipt and
carry a finalization error stating that the MP4 is not playable. Sotto's AVFoundation reader also
fails to decode either file. No meeting-platform export or maintainer-corrected reference transcript
was found; the persisted Sotto hypotheses are model output and were not reused as ground truth.

The bounded harness work is complete: `model_benchmark` now reads an explicitly selected meeting or
microphone channel directly from a retained MP4 and optionally calculates substitutions, deletions,
insertions, and WER from a supplied UTF-8 reference. T063's WAV workflow remains supported. The
experiment document records the exact inventory, failures, and rerun commands. No model was ranked
and `base.en` remains the provisional default.

## Out of scope

Changing the reader or the lag, diarization, GPU or quantization work, per-session model selection
UI, and any non-Whisper backend.

## Blocked on T061's amplitude defect — 2026-08-13

Do not benchmark against a retained recording until T061's per-channel amplitude defect is fixed.
The meeting channel is currently written at near-zero amplitude, so every model would be measured
against a degraded signal. Any accuracy ranking derived from it would describe the defect rather
than the models, and a wrong default chosen that way would be worse than today's provisional one.

Resumes when a cleanly finalized recording is audible on both channels — which is also the point at
which the maintainer's paired platform transcript becomes a usable reference.

## Unblocked — 2026-08-13

T061's per-channel amplitude defect is repaired and the meeting channel is audible, so a retained
recording is now a valid benchmark input. Before measuring anything, confirm the input independently:
report peak and RMS for the meeting channel of the recording you benchmark. A near-silent channel
would repeat exactly the failure this task exists to avoid, and the number costs nothing to print.

Still required from the maintainer: one platform transcript paired with a named recording, with its
provenance and its own error characteristics described.

## Healthy-input preflight — 2026-08-13

Session `1786660431733986000` is now durably `available`: 40.169333 seconds and 12,182,734
bytes. Independent AVFoundation decoding of its last audio track's meeting channel (left/channel 0)
produced 642,704 samples at 16 kHz with peak amplitude **0.625547290** and RMS amplitude
**0.085972686**. This is not a near-silent repeat of T061's defect.

The benchmark harness now prints peak and RMS immediately after decoding, before provisioning or
running a model. Its finalized full-range MP4 reader still returns
`Inference("The operation could not be completed")` for this recording, so the exact independently
decoded meeting channel was exported to temporary mono 16-kHz PCM16 WAV for a same-sample preflight.
The WAV measured peak **0.625537872** and RMS **0.085962381** after PCM16 quantization.

All three models produced materially different hypotheses, so this input has more separating power
than JFK. That is not an accuracy result: the clip is only 40 seconds, and no platform or
human-corrected reference transcript was supplied or found. No WER, category accuracy comparison,
or revised default is claimed. The task remains blocked on its named reference-transcript input and
the required several-minute representative sample.

## Partial result: the models are separated for the first time — 2026-08-14

A maintainer-supplied reference finally exists: a 111.5-second retained recording paired with the
source platform's own caption transcript. Reference provenance is stated in
`docs/experiments/asr-model-benchmark.md`, including that it is machine-generated and carries its
own errors, so only differences between runs are claimed.

Measured on that input with the trough-aligned windowing now shipping: `base.en` 6.98% WER,
`small.en` **4.75%**, `medium.en` 9.22%. The ranking is not monotonic in size, and `medium.en` is
both slower and worse — its errors are a few catastrophic hallucinations, including `Thanks for
watching.` replacing the opening sentence, rather than diffuse degradation. Throughput is not the
constraint for any of them: `small.en` runs at 12.3x realtime behind a lagged transcript.

`base.en` renders `Rust` as `us`/`Rusk` and `GitLab` as `Gillab` on clean single-speaker narration.
`small.en` gets all of them right.

**This does not close the task.** The acceptance criteria require multi-speaker audio with accent
variety, conference-codec compression and crosstalk, and this is one native-accented speaker reading
to camera. What has changed is that the harness now produces a real ranking rather than a tie, and
the evidence points at `small.en`. A representative recording is the only remaining input.
