# T009 — CLI harness: WAV + frames in → timeline events out, fixtures, latency bench

**Status:** todo (unblocked — T014 frozen)

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
