# ASR model default benchmark

**Date:** 2026-08-13  
**Decision:** keep `base.en` as the provisional default pending the representative-audio rerun.

## Question and decision rule

ADR-0018 removed the realtime-output constraint but did not remove the need to settle a recording
faster than it grows. The hard eligibility floor is therefore more than `1.0` second of audio per
second of inference wall clock. Accuracy chooses among eligible models; load time and peak memory
break a tie.

All three models produced the same normalized transcript error on this sample. `base.en` was both
the fastest and the lightest in the controlled repetition, so there is no measured benefit that
justifies increasing the default. Existing and new libraries therefore continue to use `base.en`;
there is no migration or automatic re-transcription.

## Hardware and software

- MacBook Pro `MacBookPro18,1`, Apple M1 Pro, 10 CPU cores (8 performance, 2 efficiency), 16 GPU
  cores, 16 GB unified memory.
- macOS Darwin 25.5.0, arm64.
- Repository commit at measurement start: `d9f87aa7214d55fbac8e638ef570af26d23046c1` plus the dirty
  T058/T063 working tree described by `git diff`; `whisper-rs` 0.15.1 with its Metal feature.
- Managed model artifacts all came through `ModelProvisioner` and were verified before use. The
  pinned whisper.cpp artifact revision was `c521a4b02f422512d734391fdf08bb08c0862f68`.
- Inference used Sotto's production settings: 10-second windows, greedy decoding (`best_of = 1`),
  four threads, English, Metal enabled. The 11-second file consequently used a 10-second window
  followed by a 1-second tail, matching `FinalWhisperTranscriber`.

## Input and accuracy method

The common input was whisper.cpp's public-domain real-speech `samples/jfk.wav`: 11.000 seconds,
16-kHz mono PCM16, SHA-256
`59dfb9a4acb36fe2a2affc14bacbee2920ff435cb13cc314a08c13f66ba7860e`. It was downloaded from
`https://raw.githubusercontent.com/ggerganov/whisper.cpp/master/samples/jfk.wav`.

The normalized reference (lowercase, punctuation removed) was:

> and so my fellow americans ask not what your country can do for you ask what you can do for your
> country

It has 22 words. Each hypothesis repeated the terminal word `country`, so each result had one
insertion, no deletion and no substitution: WER = `(0 + 0 + 1) / 22 = 4.55%`. This short,
well-recorded English excerpt is enough to compare this observed case, not enough to claim a
general accuracy ranking. In particular, the larger models did not improve the observed error.

## Raw measurements

The primary table is a controlled post-provision repetition in the order base, small, medium after
the machine had already initialized Metal for all three. Every model ran in its own process.
`load` spans `WhisperContext::new_with_params`; `inference` spans the two `state.full` calls; peak
RSS is the process `maximum resident set size` from `/usr/bin/time -l`. Throughput is
`11.0 / inference seconds` and excludes model load, because load is paid once per session rather
than once per audio window.

| Model | Hypothesis after normalization | WER | Load (s) | Inference (s) | Audio s / wall s | Peak RSS (bytes / MiB) | Eligibility |
|---|---|---:|---:|---:|---:|---:|---|
| `base.en` | reference + terminal `country` | 4.55% | 0.097613 | 0.330260 | 33.307075 | 254,377,984 / 242.6 | eligible |
| `small.en` | reference + terminal `country` | 4.55% | 0.203799 | 0.886910 | 12.402606 | 649,199,616 / 619.1 | eligible |
| `medium.en` | reference + terminal `country` | 4.55% | 2.760644 | 3.039415 | 3.619118 | 1,786,413,056 / 1,703.7 | eligible |

The slowest eligible model, `medium.en`, still processed 3.619 seconds of audio per second: 2.619
audio-seconds/second (261.9%) above the input rate. `base.en`, the recommendation, processed 33.307
seconds per second: 32.307 audio-seconds/second above the input rate. All three pass the hard floor
on this hardware; none would accumulate unbounded backlog at the measured rate.

### Cold and earlier observations

The first-ever base run on this working tree included Metal kernel/cache initialization and was much
slower: load 0.141500 s, inference 4.887438 s, throughput 2.250668, and peak RSS 245,497,856 bytes.
It still passed the `> 1.0` floor. The first small run after that was load 0.205568 s, inference
0.678987 s, throughput 16.200597, RSS 651,132,928 bytes. The first verified medium run was load
0.815554 s, inference 1.914655 s, throughput 5.745160; it was not wrapped by `time`, so it has no
peak-RSS observation. These are retained as raw order effects, not mixed into the primary table.

The sizable variance—especially base's one-time Metal initialization and medium's later slower
repetition—means these figures should be read as this machine's observed bounds, not universal
claims. Importantly, every observed throughput still clears the eligibility floor.

## Reproduction

Build the ASR-owned harness, then run each verified model in an isolated process:

```sh
WHISPER_DONT_GENERATE_BINDINGS=1 cargo build -p asr --example model_benchmark --locked
/usr/bin/time -l target/debug/examples/model_benchmark base.en /path/to/jfk.wav
/usr/bin/time -l target/debug/examples/model_benchmark small.en /path/to/jfk.wav
/usr/bin/time -l target/debug/examples/model_benchmark medium.en /path/to/jfk.wav
```

The harness calls `ModelProvisioner::for_current_user().resolve_or_download`, prints the pinned
revision and exact input/model paths, and refuses non-16-kHz-mono-PCM16 WAV input. It installs
whisper-rs logging hooks only to prevent verbose backend logs from obscuring the measurements.

## Recommendation

Keep `base.en`. On the actual common recording, `small.en` and `medium.en` tied rather than improved
accuracy while using about 2.6x and 7.0x base's peak RSS respectively in the controlled run. Base
also had the largest throughput margin and shortest measured load. A larger default needs a broader
real-conversation corpus that demonstrates a repeatable accuracy gain large enough to pay those
costs; this measurement does not.

## T065 representative meeting-audio rerun — blocked evidence inventory

**Inventory date:** 2026-08-13  
**Outcome:** no accuracy measurements or model decision; `base.en` remains provisional.

The local Sotto recording directory contains two retained files from explicitly scoped Microsoft
Teams window captures:

| Session | File size | Session wall-clock span | Durable recording receipt |
|---|---:|---:|---|
| `1786652195282693000` | 68,391,174 bytes | 243.108 s | `growing`; finalization says the MP4 is not playable |
| `1786652633023855000` | 7,608,738 bytes | 26.383 s | `growing`; finalization says the MP4 is not playable |

The longer session's persisted Sotto hypotheses show conference speech, non-native accents, domain
jargon such as CMDB/mainframe/complexity analysis, and multiple remote speakers. That makes the
session a plausible benchmark candidate, but those hypotheses are model output and cannot serve as
the reference. No platform transcript, VTT/SRT export, or maintainer-corrected transcript was found
in the Sotto data or normal Desktop, Documents, Downloads, and Teams export locations.

Neither MP4 is currently a usable benchmark input. Direct decoding through Sotto's AVFoundation
recording reader failed for both with `Inference("The operation could not be completed")`. This
matches the SQLite recording receipts: neither row reached `available`, and each retains
`Recording finalization failed: capture stream failed: recording is not playable: <path>`.

Running three models despite either missing input would fabricate evidence: Sotto's base-model
timeline cannot be labeled as ground truth, and a failed recording cannot establish that every
model received the same decoded samples. Consequently there are no T065 WER values, category
observations, or revised default decision yet.

### Prepared rerun workflow

The existing benchmark now accepts retained MP4 directly and requires an explicit channel. This
keeps ADR-0018's channel mapping visible at the command line and prevents accidentally benchmarking
the microphone when the intended input is meeting audio. An optional UTF-8 reference file produces
normalized substitution, deletion, insertion, and WER counts in the same output as the timing and
hypothesis:

```sh
WHISPER_DONT_GENERATE_BINDINGS=1 cargo build -p asr --example model_benchmark --locked
/usr/bin/time -l target/debug/examples/model_benchmark base.en recording.mp4 \
  --channel meeting --reference corrected-transcript.txt
/usr/bin/time -l target/debug/examples/model_benchmark small.en recording.mp4 \
  --channel meeting --reference corrected-transcript.txt
/usr/bin/time -l target/debug/examples/model_benchmark medium.en recording.mp4 \
  --channel meeting --reference corrected-transcript.txt
```

For MP4 input, `meeting` means left/channel 0 and `microphone` means right/channel 1. Mono 16-kHz
PCM16 WAV remains supported for reproducing T063. The rerun can proceed once one playable retained
meeting recording and one provenance-described, human-reviewed reference transcript for it exist.
The reference should retain timestamps or segment boundaries for manual proper-noun, jargon,
accent, and crosstalk comparisons; whole-file WER alone does not satisfy T065.

## T065 healthy-input preflight after T061

**Run date:** 2026-08-13  
**Session:** `1786660431733986000`  
**Outcome:** the meeting channel is healthy and the models diverge, but there is still no accuracy
ranking because no independent reference transcript exists.

The durable receipt records an available 40.169333-second, 12,182,734-byte MP4. An independent
AVFoundation pass decoded the last audio track's left/meeting channel to 642,704 samples at 16 kHz:

| Signal check | Peak | RMS |
|---|---:|---:|
| Retained MP4, meeting channel | 0.625547290 | 0.085972686 |
| Extracted PCM16 benchmark WAV | 0.625537872 | 0.085962381 |

These levels independently reject the near-silent-channel defect that invalidated the earlier
candidate. `model_benchmark` now prints both values immediately after input decode and before model
provisioning or inference.

Sotto's finalized full-range MP4 reader still failed before returning samples with
`Inference("The operation could not be completed")`. For this bounded preflight, AVFoundation read
the same recording's last audio track without an explicit end-bounded time range; its left channel
was quantized to temporary 16-kHz mono PCM16 and supplied to the existing harness. The retained MP4
was not modified.

| Model | Load (s) | Inference (s) | Audio/wall | Max RSS |
|---|---:|---:|---:|---:|
| `base.en` | 0.114066 | 3.157178 | 12.723x | 262,619,136 bytes |
| `small.en` | 0.343876 | 2.493485 | 16.110x | 657,719,296 bytes |
| `medium.en` | 0.917607 | 7.089750 | 5.666x | 1,822,179,328 bytes |

The hypotheses differ materially on names, sentence boundaries, and several difficult phrases, so
the input can separate model behavior unlike the JFK tie. It cannot rank accuracy: the only
available Sotto transcript is model output, the clip is 40 seconds rather than the required several
minutes, and no platform export or human-corrected reference was supplied or found. Therefore no
WER or per-category accuracy number is reported and `base.en` remains provisional. T065 still
requires a provenance-described reference transcript paired with a representative retained
recording before its default decision can be completed.

## Windowing and decoding sweep (2026-08-14)

A separate question from model ranking, run on the same harness. T063 and T065 both asked which
*model* to use. This asks what the code around the model does to the text, and it turns out to
matter more than the remaining model gap.

### Input and reference

A 111.5-second retained recording captured by the maintainer, meeting channel (left/channel 0),
peak amplitude 0.961971164 and RMS 0.054471771 — audible, not a repeat of T061's near-silent
defect. Reference: the source platform's own caption transcript for the same material, supplied by
the maintainer and held outside the repository under `benchmarks/` (third-party text, gitignored).

**The reference is machine-generated and carries its own errors.** No absolute WER below is a claim
about Sotto's accuracy. Every variant is scored against the same reference, so the *differences*
between variants are valid while the absolute values are not. That is the only claim made here.

The audio is also single-speaker, clean, and native-accented — the easy case. A technique that fails
here fails everywhere; one that succeeds here has not been shown to survive a real meeting.

### Method

`model_benchmark` was changed to drive `FinalWhisperTranscriber` itself rather than reimplement its
decode loop, so the sweep measures shipping code. Each technique runs alone against the baseline and
then combined, because a combined-only run cannot attribute its own result.

### Result, base.en

| variant | segments | fragments | sub | del | ins | WER % |
| --- | --- | --- | --- | --- | --- | --- |
| baseline (5 s clock-aligned windows) | 35 | 6 | 13 | 14 | 7 | 9.50 |
| context-2s | 42 | 4 | 14 | 22 | 41 | 21.51 |
| context-10s | 46 | 9 | 14 | 8 | 37 | 16.48 |
| **trough-cut (500 ms search)** | **39** | **6** | **12** | **6** | **7** | **6.98** |
| prompt carry-over | 25 | 4 | 27 | 30 | 12 | 19.27 |
| safeguards | 35 | 6 | 13 | 14 | 7 | 9.50 |
| all | 21 | 1 | 32 | 76 | 93 | 56.15 |
| all, 10 s window | 14 | 0 | 9 | 45 | 21 | 20.95 |

### What this establishes

**Cutting windows at an energy trough instead of on the sample count is adopted.** Deletions fall
from 14 to 6, which is the predicted mechanism and is confirmed in the text: `hirable`, the `100` of
`100 to 200K`, and `back-end` were each destroyed outright by a clock-aligned cut — a word split
across two windows is decoded by neither — and all three survive a trough-aligned one. Nothing is
duplicated to recover them. This is now the default in `Config::new`.

**Acoustic context is rejected as implemented, not as an idea.** It does what it was meant to:
substitutions 13 to 9 and deletions 14 to 8 at a 10-second span. It is rejected because re-decoding
committed audio duplicates text faster than the overlap trimmer removes it — insertions rise to 37.
Two successive dedup designs were measured, exact-prefix and 70%-agreement fuzzy matching, and both
leak. The failure is instructive: a second decode of the same audio with different surroundings is
frequently *better* than the first, so the repeated passage is not textually identical and cannot be
removed by matching it. The published answer to this is LocalAgreement — commit only what two
successive decodes concur on — which is a larger change than a dedup filter and is not attempted
here.

**Prompt carry-over is rejected on measurement.** 9.50% to 19.27%, with segments dropping from 35 to
25 and deletions rising to 30 — Whisper is emitting *less*, not repeating. Whether a shorter prompt
than 200 characters behaves differently is untested.

**The "decoding safeguards" were a no-op, and the advice to enable them was wrong.**
`temperature_inc` 0.2, `entropy_thold` 2.4, `logprob_thold` -1.0, `no_speech_thold` 0.6 and
`suppress_blank` are already whisper.cpp's defaults (`whisper_full_default_params`, whisper.cpp
src/whisper.cpp). Only `suppress_nst` differs, and `split_on_word` has no effect without `max_len`.
The row is byte-identical to baseline because the variant changed nothing that applied. Sotto was
never missing this fallback.

### What remains unfixed

Trough-aligned cutting repairs the seams; it does not touch vocabulary. `Rust` still decodes as
`us` and `Rusk`, and `GitLab` as `Gillab`, throughout. Those are `base.en` errors on clean,
single-speaker audio — which is evidence for T065's question, and it points away from `base.en`
rather than toward it. Six fragment segments also remain, so the prosody guard against
words-per-minute readings computed over one-word segments is still needed.

### Rerun

```
cargo run --release -p asr --example model_benchmark -- \
  base.en <recording.mp4> --channel meeting \
  --reference <reference.txt> --sweep [--transcript]
```

### All three models on the same input (2026-08-14)

The sweep repeated per model. Same recording, same reference, same caveats — differences between
rows are valid, absolute values are not.

| model | baseline WER % | trough-cut WER % | trough-cut del | xRT | size |
| --- | --- | --- | --- | --- | --- |
| base.en | 9.50 | 6.98 | 6 | 30.0 | 148 MB |
| **small.en** | **6.42** | **4.75** | **6** | **12.3** | **488 MB** |
| medium.en | 13.97 | 9.22 | 18 | 5.1 | 1.5 GB |

**This is the separation T063 and T065 never got.** JFK gave an identical 4.55% for all three; a
40-second clip gave differing hypotheses but no reference. Here the models are ranked, and the
ranking is not monotonic in size.

**`small.en` is the best model on this input, and `medium.en` is both slower and worse.** The
technique ranking is identical across all three models — trough-cut wins, context and prompt
carry-over lose — which is the strongest evidence available here that the windowing result is a
property of the method rather than of one model.

`medium.en`'s deficit is concentrated, not diffuse. Its first committed segment is `Thanks for
watching.`, a hallucination that replaced the opening sentence outright, and it renders `Rust` as
`Russ` where `small.en` does not. It is *better* than `small.en` in places — it recovers `backend
Rust roles` where `small.en` produces `rust rolls`. A handful of severe inventions, not uniformly
worse transcription, is what moves its WER.

That failure mode is the one this product can least afford. A dropped word is visibly missing; a
fluent invented sentence carries a citation and reads exactly like a real one.

#### On `base.en`

`Rust` decodes as `us` and `Rusk`, and `GitLab` as `Gillab`, throughout — on clean, single-speaker,
native-accented narration. `small.en` gets every one of them right. Whatever the correct default is
for meeting audio, `base.en` is failing the easy case.

#### Correction to the safeguards finding above

`suppress_nst` is not quite a no-op. It is one on `base.en`, where the sweep row is byte-identical.
On `small.en` and `medium.en` it shifts one or two errors, and `[BLANK_AUDIO]` survives into
committed output without it. The substance of the correction stands — whisper.cpp's temperature
fallback and thresholds were already enabled and Sotto was never missing them.

#### What this cannot support

One 111-second recording of one native-accented speaker reading to camera, scored against a
machine-generated reference. T065 asks for multi-speaker audio with accent variety, conference-codec
compression and crosstalk before a default is changed on accuracy grounds. This result separates the
models for the first time and points clearly at `small.en`; it is not yet the representative sample
that task requires.

## Building a reference where none exists: corrected drafts (2026-08-14)

T065 has been blocked since it opened on one input: a reference transcript for real captured
conversation. Sotto's own captured sessions have no platform export, and there is no second
transcription system on the machine to borrow one from.

`--draft <path>` closes this. It transcribes **both** channels, merges them into one time-ordered
document with speaker labels and timecodes, and writes it in the format `--reference` reads back. A
listener corrects it against the audio; the corrected file is then a reference.

```
# produce
cargo run --release -p asr --example model_benchmark -- \
  small.en <recording.mp4> --variant trough-cut --draft <draft.txt>
# correct <draft.txt> by ear, then
cargo run --release -p asr --example model_benchmark -- \
  small.en <recording.mp4> --channel meeting --reference <draft.txt> --sweep
```

`#` lines, timecodes and speaker labels are stripped before scoring. `--channel` selects the
matching speaker's lines, because scoring one channel against a two-speaker reference counts the
other speaker's every word as a deletion — measured at 11.92% WER with zero substitutions and zero
insertions before the filter was added, a number describing the question rather than the
transcriber.

**The correction is not optional and the draft is not neutral.** Scored uncorrected against itself,
the generating configuration reads **0.00% WER**. Every line a reviewer leaves alone is a line that
silently ratifies the configuration that wrote it. A skimmed draft measures nothing, and the draft
header says so where whoever edits it will read it.

Prefer correcting a draft produced by a configuration the sweep is *not* trying to promote, or
accept that the promoted configuration enjoys a floor it did not earn.

### Available two-speaker material (2026-08-14)

`1786738113163383000.mp4`, 125.4 seconds, both channels carrying signal: meeting peak 1.006797075 /
RMS 0.101056072, microphone peak 0.272705853 / RMS 0.007548760. Spontaneous two-party conversation
with overlapping speech, laughter, disfluencies and proper nouns — materially harder than the
narration sample, and much closer to what T065 requires.

One finding independent of transcription: **the microphone channel's RMS is roughly thirteen times
lower than the meeting channel's.** Both transcribe, so this is not T061's near-silent defect, but a
local speaker being that much quieter than a remote one in the same recording is worth explaining
before it is normalized away.

## Non-speech tokens reaching committed output (2026-08-14)

The defect: Whisper's non-speech vocabulary — `[BLANK_AUDIO]`, `(laughs)`, `(keyboard clicking)` —
was reaching the committed timeline as if it were transcribed speech, eligible for the summarizer to
cite. Confirmed on `1786738113163383000.mp4` (the two-speaker recording above), `small.en`,
trough-cut windowing: `(laughs)` on the meeting channel, `[BLANK_AUDIO]` three times on the
microphone channel (the quieter channel, consistent with it having more near-silent gaps for Whisper
to mislabel).

### What `suppress_nst` actually does

`whisper.cpp/src/whisper.cpp` defines a fixed `non_speech_tokens` vocabulary of forty-odd punctuation
and symbol tokens — brackets, parentheses, music notes, quote marks. `suppress_nst` denies these
tokens' logits during decode. Whisper has no single token for `[BLANK_AUDIO]`; it assembles the tag
from ordinary punctuation tokens, so denying those tokens denies the tag. This means `suppress_nst`
is not a vague "changes decoding" knob — it is a small, enumerable deny-list, and real spoken meeting
transcripts essentially never require literal brackets or parentheses.

### Two mechanisms, measured separately

**Mechanism 1 — decode-time suppression (`suppress_nst`).** Isolated on the public-domain
`jfk.wav` (T063's sample, re-verified against the documented SHA-256) across all three models,
baseline vs. `suppress_nst` alone: **0.00% WER, identical, on every model.** Clean single-speaker
narration has no ambiguity for it to resolve badly.

The real captured audio tells a different story. Baseline vs. `suppress_nst` alone (clock-aligned
windows, `small.en`, no reference transcript exists for this recording — see below for why), same
recording, both channels:

- **Microphone channel:** baseline commits 10 segments with zero non-speech markers already (the
  filter below removes them regardless of this setting). `suppress_nst` commits 13: it drops "You"
  from "You know...", and adds "Thank you." twice and "Mm-hmm." once in windows that decode to
  nothing without it. "Thank you." is a well-documented Whisper hallucination on quiet/silent audio
  (compare the medium-model "Thanks for watching." hallucination in the entry above) and the mic
  channel is the quiet one. Whether "Mm-hmm." is recovered real speech or the same failure mode is
  not resolvable without a human-corrected reference for this recording.
- **Meeting channel:** 33 segments either way, but not the same 33: `suppress_nst` inserts "But",
  inserts "like", and inserts a "Thank you." segment where the unfiltered decode had produced
  nothing.

**Denying whisper.cpp a null decode does not make an ambiguous window silent — it makes the window
invent a plausible sentence instead.** That is a worse failure mode than the one being fixed: a
dropped or garbled non-speech tag is visibly not a sentence, but "Thank you." or an inserted "But"
reads exactly like a transcribed word and would carry a citation. **`suppress_nst` is not adopted as
Sotto's default** on this evidence; `Config::new` leaves `DecodingOptions::suppress_non_speech` off.
It remains a configurable, sweep-measurable field (`--variant suppress-nst`) rather than removed,
so this rejection can be re-run against better material rather than trusted from memory.

**Mechanism 2 — filtering committed text.** `asr::retain_speech` (called unconditionally from
`FinalWhisperTranscriber::infer`, the same pattern `bound_hypotheses_to_audio` already uses for its
own invariant) drops any hypothesis whose trimmed text is *nothing but* a single bracketed or
parenthesized span — `is_non_speech_marker` — before it can be committed. It is matched on the whole
segment, never a substring, so a sentence that merely contains a parenthetical aside is untouched.
This is not an accuracy technique with a tradeoff to sweep: a segment that is entirely
`[BLANK_AUDIO]` or `(laughs)` is by construction not a transcribed word, so the filter can only ever
remove Whisper's own non-speech annotations, never real speech, and there is nothing to measure a WER
delta against. Re-running both channels with baseline `DecodingOptions` (`suppress_nst` off)
confirms it: the transcript printed above with `[BLANK_AUDIO]`/`(laughs)` present is superseded by a
transcript with the same segments minus those three, unconditionally.

**Adopted: filtering, not `suppress_nst`.** The filter gives a complete, unconditional guarantee that
does not depend on decoder behaviour and costs nothing measurable; `suppress_nst` gives a
probabilistic reduction that measurably traded a safe, visible gap for a fluent, citable invention on
this input. Both remain available — `suppress_non_speech` for further sweeps, the filter always on —
but only the filter is load-bearing for the defect.

### The `safeguards` rename

`DecodingOptions::safeguards` bundled `suppress_nst` with seven other `FullParams` setters
(`temperature`, `temperature_inc`, `entropy_thold`, `logprob_thold`, `no_speech_thold`,
`suppress_blank`, `split_on_word`). The 2026-08-14 windowing sweep above already established that all
seven equal whisper.cpp's own defaults (`whisper_full_default_params`) or, for `split_on_word`, do
nothing without `max_len`, which Sotto never sets — the `safeguards` row was byte-identical to
baseline on `base.en` for exactly this reason. The field name implied Sotto was enabling seven
protections it was not; only the eighth, `suppress_nst`, ever did anything. The seven no-op setter
calls are removed rather than left dead behind the flag, and the field is renamed
`suppress_non_speech` to name what it actually controls. This is a code change with no behavioural
effect: not calling a setter leaves whisper.cpp's own default in force, which is what was being set
anyway.

### What this cannot support

No WER number exists for this recording because no corrected reference exists for it and one was not
fabricated: `benchmarks/teammate-call.draft.txt` is an uncorrected model draft, and scoring against it
would (per the entry above) trivially favor whichever configuration generated it. The `suppress_nst`
comparison above is a direct transcript diff on real audio, not a WER delta, and is reported as such.
Whether "Mm-hmm." is real and "Thank you." is invented, or the reverse, requires a listener.

### Reproducibility note

`DecodingOptions::default()` is unchanged (`suppress_non_speech: false`, same as the old
`safeguards: false`), so the sweep's `baseline` variant still reflects pre-technique decoding
settings. The unconditional filter in `finals.rs` is outside `DecodingOptions` and therefore applies
to every variant including `baseline` — a deliberate scope difference, because it fixes a correctness
defect rather than tuning accuracy. It does not change any WER number recorded elsewhere in this
document: the JFK and `rust-career-paths` inputs used for other sweeps contain no bracket-only
segments for it to remove.

### What remains unfixed

`>> Yeah.` — a speaker-change artifact, not a bracketed/parenthesized annotation — reaches the
microphone channel's committed output untouched by either mechanism. It is not in scope for this
entry (the defect was non-speech tokens specifically) but is the same shape of problem and is left
for a follow-up.
