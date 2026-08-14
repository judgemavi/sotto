# T063 — Re-benchmark the ASR default now that latency is not the constraint

**Status:** done

**Wave:** M4 — recording

**Depends on:** T058 (`done`). ADR-0018 is accepted.

**Owns:** `crates/asr/**`, `crates/cli/src/pipeline.rs`, `docs/experiments/asr-model-benchmark.md`,
and this task

**Concurrency (planner, 2026-08-13):** T062 runs alongside and holds `crates/app/src/session/**`,
`crates/app/src/workspace/**` and the recording persistence path in `crates/rag/**`. Do not edit
them. If changing the default model requires an app-side change, state exactly what it is and stop —
T062 will make it.

## Why this exists

`base.en` was chosen under a realtime streaming constraint that ADR-0018 removed. Transcription now
runs behind the capture from a committed recording, so a slower, more accurate model costs latency
rather than correctness. Nobody has measured whether that trade is worth taking, and the ADR
explicitly says the default "should be re-benchmarked once lag is in place" rather than assumed.

T058 carried this item and closed without it; it lands here with the reader already built.

## Plan

1. Provision `base.en`, `small.en` and `medium.en` through the existing
   `crates/asr/src/model/` provisioner. It already pins a whisper.cpp revision, verifies checksums
   and resumes partials — use it rather than fetching weights by hand. Roughly 2 GB total.
2. Transcribe real recorded speech to known text with each model. Report word error rate, or a
   stated and defensible substitute, against the same input for all three.
3. Measure the throughput that matters here: **seconds of audio transcribed per second of wall
   clock**, on this hardware, for each model. A model below 1.0 accumulates unbounded backlog and
   cannot be the default at any accuracy, because a one-hour meeting would never settle. Say so
   explicitly for each model rather than reporting only accuracy.
4. Report peak memory and model load time. Load time is paid at every session start today.
5. Recommend a default from the measurements. If the recommendation changes the default, change it
   and say what a user with an existing session library should expect. If the numbers do not
   justify a change, keep `base.en` and record why — a measured non-change is a real result.
6. Record everything in `docs/experiments/asr-model-benchmark.md`: hardware, input, model revisions,
   raw numbers, and the reasoning. A future reader must be able to disagree with the conclusion
   using the same data.

## Acceptance

- All three models are measured on the same real recorded speech, with accuracy, realtime factor,
  memory and load time reported per model.
- The realtime factor for the recommended default is stated, and is above 1.0 with stated headroom.
- The default is either changed with reasoning or deliberately kept with reasoning.
- Per the verification rule, the numbers come from actual transcription runs, not from published
  model claims or estimates.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Result (2026-08-13)

Completed. `docs/experiments/asr-model-benchmark.md` records the input digest/reference, hardware,
pinned artifact revision, cold observation, controlled post-provision raw numbers, WER calculation,
throughput eligibility, load time, peak RSS, reproduction commands, and decision.

All three models had the same 4.55% WER (one terminal insertion) on the same 11-second real JFK
recording and all cleared the `> 1.0` throughput floor. The controlled throughputs were 33.307x
(`base.en`), 12.403x (`small.en`), and 3.619x (`medium.en`). Peak RSS was 242.6 MiB, 619.1 MiB, and
1,703.7 MiB respectively. `base.en` remains the default because the larger models produced no
accuracy gain on the measured case while loading more slowly and retaining substantially more
memory. No app-side change or existing-library migration is required.

Verification:

- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test -p asr --all-targets --locked`: PASS (19 passed,
  1 explicitly ignored network/inference test).
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test --workspace --all-targets --locked`: PASS.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo clippy -p asr --all-targets --locked -- -D warnings`:
  PASS.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo clippy --workspace --all-targets --locked -- -D warnings`:
  PASS; Cargo emitted existing future-incompatibility advisories for `block` and
  `proc-macro-error2`.
- `cargo fmt --all -- --check`: PASS.
- Scoped `git diff --check` over T063-owned paths: PASS. The repository-wide command reports the
  pre-existing `.tasks/T048-top-down-meeting-workspace.md:53: new blank line at EOF`, outside this
  task's ownership; it was not modified.

## Out of scope

Changing the reader or the lag (T058, closed), the app-side model settings surface (T062), GPU or
quantization work, and any non-Whisper backend.

## Planner review — accepted as executed, conclusion not supported — 2026-08-13

The work is accepted: the harness, the controlled method, the provisioning discipline, the raw
numbers, the cold/warm separation, and the reproduction commands are all sound, and the write-up
states its own limits rather than overselling.

The **conclusion** does not follow from the evidence. The input was 11 seconds and 22 words of
clean single-speaker studio English, and all three models returned an identical hypothesis with an
identical single insertion. This task's own decision rule was "accuracy chooses among eligible
models; load time and peak memory break a tie" — accuracy chose nothing, so `base.en` was kept by
tiebreak. The experiment cannot separate "larger models add nothing" from "this sample cannot
detect what larger models add."

The throughput finding is the durable result and it changes the picture: `medium.en` at **3.62x
realtime** is entirely affordable under ADR-0018's lagged design, so the constraint that originally
selected `base.en` no longer applies. That makes an accuracy measurement worth doing properly rather
than settling by default.

Re-run on meeting-representative audio is filed as **T065**, holding `crates/asr/**` and this
document by sequential handoff. `base.en` remains the default in the meantime — that is the correct
provisional state, not a settled one.
