# T009 — CLI harness: WAV + frames in → timeline events out, fixtures, latency bench

**Status:** done (approved at review round 2)

**Wave:** 1 — fully parallel; builds against traits, stubs where crates are unfinished

**Depends on:** T001 (types, traits, bus) · T014 (timeline events are the CLI output format)

**Owns:** `crates/cli/**`, `fixtures/**`

## Goal

`AGENTS.md` makes the headless core a hard architectural constraint, and this crate is
its proof: it must stay green in CI forever. It is also how every other stage gets
tested and measured without a display server, a microphone, or a Zoom call — which is
what lets the wave-1 tasks proceed in parallel.

## Plan

1. `crates/cli/src/main.rs` with `clap`. Subcommands:
   - `sotto-cli run <wav> [--system <wav>] [--frames <dir>]` — feed one or two WAVs plus
     an optional timestamped frame directory through the pipeline, emit `TimelineEvent`s
     as JSONL on stdout. Two WAVs means mic + system; one means mic only.
   - `sotto-cli transcribe <wav>` — ASR only.
   - `sotto-cli vad <wav>` — VAD segments only.
   - `sotto-cli ocr <frames-dir>` — screen snapshots only.
   - `sotto-cli bench <wav>` — stage-by-stage latency report.
   - `sotto-cli replay <jsonl>` — feed a recorded timeline to downstream consumers.
   - `sotto-cli ingest <path>` / `sotto-cli search <query>` — exercise the RAG store.

2. **File-backed capture** (`src/file_capture.rs`) — the single most valuable thing in
   this task. A WAV-file `CaptureBackend` implementing the same trait as T002's real
   capture, feeding frames at either real-time pace (`--realtime`, for honest latency
   numbers) or as fast as possible (for CI). Pair it with a directory-of-PNGs frame
   source on the same clock, so the screen stage is equally testable without hardware.
   Everything downstream becomes runnable with no microphone and no display, and T002
   stops being a blocker for anyone.

3. **Stub-tolerant wiring.** Wave-1 crates land at different times. Build against the
   traits with `NoOp` implementations behind feature flags so the CLI compiles and CI
   stays green before every stage exists, then swap the real impls in as they land.
   State clearly in `--help` output which stages are live versus stubbed.

4. **JSONL timeline output** — one `TimelineEvent` per line, in timeline order, using the
   `serde` feature. This is the canonical debugging and testing format for the whole
   project: stable field names, and `id`/`ts`/`supersedes` on every line so supersede
   chains are inspectable by eye. Add `--kinds utterance.final,trigger` filtering; a
   two-hour session is a lot of JSONL and consumers usually want one kind.

   A `sotto-cli replay <jsonl>` subcommand that feeds a recorded timeline back through
   downstream consumers is worth the hour it costs — T016, T017 and T013 all need to
   iterate against real sessions without re-recording calls.

5. **Latency instrumentation.** Timestamp each event's entry and exit per stage and
   report p50/p95/p99 for: frame→VAD decision, speech-end→first partial,
   speech-end→final, and (once T013 lands) speech-end→first suggestion token. That last
   one is the ~1s budget from `AGENTS.md`. Print a table plus machine-readable JSON.

6. **Fixture corpus** under `fixtures/` — the shared asset the whole project tests
   against, which is why this task owns it:
   - a short two-speaker sales-call excerpt (mic + system as separate WAVs),
   - one with a clear objection and a competitor mention,
   - one with heavy crosstalk/interruption,
   - one with long silences,
   - silence-only and music-only negative cases,
   - **at least one fixture with accompanying timestamped screen frames** — a pricing
     slide visible while pricing is discussed. That pairing is the entire premise of the
     fused timeline, and T015, T016 and T017 all need something to test against.
   Keep them short (30–90 s) and commit them — CI needs them and they must be
   deterministic. If any real audio has licensing or consent issues, synthesise it with
   TTS and say so in `fixtures/README.md`. Include hand-labelled ground truth
   (transcript + segment boundaries + expected OCR text) alongside each file.

6b. **A recorded reference timeline.** Commit the JSONL output of a full fixture run as
   `fixtures/timelines/`. T016 replays it to build a board and T017 summarises it, neither
   of which should need to run ASR in CI. Regenerate deliberately, and treat a diff in it
   as a signal worth reading rather than noise to be blessed.

7. **CI integration test** that runs the full harness over a fixture and asserts on the
   event stream. This is the test that keeps the headless-core invariant honest.

## Contract for downstream tasks

`fixtures/` is the canonical test corpus — other crates reference it by relative path
rather than committing their own copies of long audio. `FileCapture` is the standard way
to drive the pipeline in tests.

## Acceptance

- `cargo run -p cli -- run fixtures/call-01-mic.wav --system fixtures/call-01-sys.wav
  --frames fixtures/call-01-frames/` emits a well-formed JSONL timeline.
- Builds and runs with **no display server and no audio hardware**.
- Latency table produced with real numbers for every implemented stage.
- Fixture corpus committed with ground-truth labels and provenance documented, including
  one audio+frames pairing.
- A reference timeline committed and replayable via `sotto-cli replay`.

## Out of scope

Any UI, live capture (T002), suggestion quality evaluation (later task once T013 lands).

## Review round 1 — changes requested

The harness itself is good: file-backed `CaptureBackend` with realtime and fast modes, the
subcommand set is complete, `--kinds` filtering works, JSONL shape is right, and it runs
with no display and no audio hardware. `ocr`, `replay` and `search` all behave. Verified
fmt clean, strict clippy clean, 56 passed / 6 ignored.

**But the fixture corpus cannot exercise the pipeline it exists to test.**

`fixtures/README.md` states it plainly: *"All audio here is programmatically synthesised…
Tone bursts… these tones intentionally contain no human speech."* The consequence was not
followed through. Silero correctly finds no speech in a tone burst, so nothing flows
downstream:

```
$ cargo run -p cli -- run fixtures/call-01-mic.wav --system fixtures/call-01-sys.wav
$ echo $?
0
```

Zero events, exit 0. `sotto-cli vad fixtures/call-01-mic.wav` likewise produces nothing.
The committed `fixtures/timelines/call-01.jsonl` holds 6 `vad` events that **cannot be
reproduced from the corpus** — it was generated some other way, so replay is validating a
fiction.

This is the same shape as the 2×2 frames: the artifact exists, the suite is green, and
nothing is being tested. `fixtures/` is meant to be *"the shared asset the whole project
tests against"* — T004, T005 and T011 all depend on it, and against tone bursts none of them
can be validated at all.

### R1. Regenerate the audio corpus with real synthetic speech

The brief asked for TTS specifically. macOS ships it, so there is no licensing or consent
issue and it stays fully deterministic. Verified working:

```bash
say -v Samantha -o out.aiff "We are already using Salesforce and the pricing feels high"
afconvert -f WAVE -d LEI16@16000 -c 1 out.aiff out.wav
```

Zero-crossing rate 2119/s versus 185/s for the current tones — genuinely speech-shaped.
Use **two different voices** for mic and system so speaker separation is testable, and keep
the ground-truth JSON in step with what is actually spoken. Negative cases (silence, music)
stay as they are; they are correct already.

Then confirm the corpus does its job end to end: `run` must emit VAD, utterance and prosody
events, and the committed reference timeline must be **regenerated from an actual run**, not
authored alongside it.

### R2. Frames need real glyphs, and the fixture must be verified against Vision

Same root cause on the visual side: the generated bands contain no rendered text, so T015's
OCR has never extracted a character.

Note what I found while checking this — a PDF-rendered text PNG returned **zero
observations from a plain Swift `VNRecognizeTextRequest`**, not just from our binding. So
generating something that looks like text to us is not sufficient. **Whatever you generate,
verify Vision actually reads it before committing it**, or the fixture will silently repeat
the current problem. Render glyphs properly (CoreGraphics into a bitmap is deterministic and
Swift is already a build dependency), then assert expected strings in a test.

### R3. Silence should not look like success

A run that produces no events exits 0 and prints nothing, which is indistinguishable from
working. Emit a warning to stderr when a stage produces no output — no speech detected, no
model configured, no frames found. This matters more now that `AGENTS.md` makes the no-key
map tier a shipping product: users will legitimately run without a model, and "nothing
happened" has to be distinguishable from "nothing was supposed to happen".

### Re-review

Corpus regenerated with real speech, reference timeline produced by an actual run, a frame
fixture whose text Vision demonstrably reads, and empty stages reporting themselves.

## Review round 2 — approved

All three items fixed, and verified independently rather than from the report.

**R1 — the corpus is real speech now.** `say` with Samantha and Daniel gives two distinct
voices, and the decisive check is that the pipeline actually flows: a fixture run emits
**10 VAD events where it previously emitted zero**. The reference timeline is genuine
output — 10 vad, 3 utterance_final, 3 prosody, 2 screen_snapshot — with real Whisper
transcription in it ("The migration and support are included."). Imperfect transcription of
TTS audio is expected and fine; what matters is that it is transcription rather than
fiction.

**R2 — Vision demonstrably reads the frames:**

```
"ocr_text":"Sotto Product Overview\nLocal-first sales call copilot"
"ocr_text":"Enterprise Pricing\nMigration and support included"
```

So T015's binding was never broken — it was unverified, and is now verified working.

**R3 — silence reports itself,** and better than asked. The warnings explain causation
rather than just absence:

```
warning: ASR stage disabled (no model configured)
warning: prosody stage emitted no events (ASR produced no utterances)
warning: screen stage disabled (no frames provided)
```

Telling the user prosody was empty *because* ASR produced nothing is the difference between
a warning and a useful one.

Verified: fmt clean, strict workspace clippy clean, 59 passed / 0 failed / 6 ignored.

### The Silero fix is the most valuable thing in this round

> *"Fixed Silero's missing `sr` model input, which was silently preventing real VAD inference."*

`crates/vad` never passed the sample-rate tensor the model requires. VAD was not running
real inference — and **I approved T004 with three green tests and a benchmark over a crate
that did not work.** The tone-burst fixtures hid it perfectly: no speech in, no speech out,
tests pass, everyone satisfied.

This is the fourth instance of one pattern: 2×2 frames, tone-burst audio, glyphless frames,
and now a VAD model missing a required input. Every time, an artifact existed, the suite was
green, and nothing was being exercised. Every time it surfaced only by running the thing
against real data. The verification rule in `.tasks/README.md` now says so explicitly.

Crossing into `crates/vad` to fix it was the right call rather than filing it and leaving a
broken crate approved. T004's notes record the defect.
