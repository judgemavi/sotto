# T009 — CLI harness: WAV in → events out, fixtures, latency bench

**Status:** todo (unblocked — T001 approved)

**Wave:** 1 — fully parallel; builds against traits, stubs where crates are unfinished

**Depends on:** T001 (types, traits, bus)

**Owns:** `crates/cli/**`, `fixtures/**`

## Goal

`AGENTS.md` makes the headless core a hard architectural constraint, and this crate is
its proof: it must stay green in CI forever. It is also how every other stage gets
tested and measured without a display server, a microphone, or a Zoom call — which is
what lets the wave-1 tasks proceed in parallel.

## Plan

1. `crates/cli/src/main.rs` with `clap`. Subcommands:
   - `sotto-cli run <wav> [--system <wav>]` — feed one or two WAVs through the pipeline,
     emit `PipelineEvent`s as JSONL on stdout. Two WAVs means mic + system; one means
     mic only.
   - `sotto-cli transcribe <wav>` — ASR only.
   - `sotto-cli vad <wav>` — VAD segments only.
   - `sotto-cli bench <wav>` — stage-by-stage latency report.
   - `sotto-cli ingest <path>` / `sotto-cli search <query>` — exercise the RAG store.

2. **A WAV-file `CaptureBackend`** (`src/file_capture.rs`) — the single most valuable
   thing in this task. It implements the same trait as T002's real capture and feeds
   frames at either real-time pace (`--realtime`, for honest latency numbers) or as
   fast as possible (for CI). Everything downstream becomes testable with no hardware,
   and T002 stops being a blocker for anyone.

3. **Stub-tolerant wiring.** Wave-1 crates land at different times. Build against the
   traits with `NoOp` implementations behind feature flags so the CLI compiles and CI
   stays green before every stage exists, then swap the real impls in as they land.
   State clearly in `--help` output which stages are live versus stubbed.

4. **JSONL event output** using the `serde` feature from T001. Stable field names — this
   is what tests, benches, and future debugging all parse. Include `stream_offset` and
   wall-clock on every event so latency is derivable post hoc.

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
   - silence-only and music-only negative cases.
   Keep them short (30–90 s) and commit them — CI needs them and they must be
   deterministic. If any real audio has licensing or consent issues, synthesise it with
   TTS and say so in `fixtures/README.md`. Include hand-labelled ground truth
   (transcript + segment boundaries) alongside each file.

7. **CI integration test** that runs the full harness over a fixture and asserts on the
   event stream. This is the test that keeps the headless-core invariant honest.

## Contract for downstream tasks

`fixtures/` is the canonical test corpus — other crates reference it by relative path
rather than committing their own copies of long audio. `FileCapture` is the standard way
to drive the pipeline in tests.

## Acceptance

- `cargo run -p cli -- run fixtures/call-01-mic.wav --system fixtures/call-01-sys.wav`
  emits a well-formed JSONL event stream.
- Builds and runs with **no display server and no audio hardware**.
- Latency table produced with real numbers for every implemented stage.
- Fixture corpus committed with ground-truth labels and provenance documented.

## Out of scope

Any UI, live capture (T002), suggestion quality evaluation (later task once T013 lands).
