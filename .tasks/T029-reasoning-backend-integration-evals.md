# T029 — Codex/OpenAI integration, switching, privacy, and latency evals

> **T047 amendment (2026-08-12):** T030 remains a valid FAIL for provable zero-tool isolation, and
> this task still makes no Codex realtime or latency claim. ADR-0014/T047 separately authorize an
> explicit, off-by-default Codex subscription experiment for post-call meeting notes only. That
> Summarizer exception does not authorize Codex Watcher/Suggester use or satisfy this task's live
> realtime acceptance.

**Status:** in-review

> Planner handoff (2026-08-12): the automated CLI cutover is independently accepted and releases
> `crates/cli/src/main.rs` plus the proposal-latency naming seam in `crates/cli/src/pipeline.rs` to
> T042. T029 remains in review only for T035-backed live OpenAI quality/latency evidence; T042 must
> not alter or claim that live gate.

**Wave:** R3 — gates the realtime advisor

**Depends on:** a T035-accepted real T032 session; T025 plus T030's recorded isolation verdict; T026; T027;
T028; T031

**Owns:** `crates/cli/src/reasoning/**`, `crates/cli/src/main.rs`,
`crates/cli/src/lib.rs` for reasoning module registration, `crates/cli/Cargo.toml` and the
resulting sequential `Cargo.lock` reconciliation for CLI-local test/stream utilities,
`crates/cli/tests/reasoning_*`,
`fixtures/reasoning/**`, `docs/experiments/openai-first-*`

## Goal

Prove the OpenAI Responses path and, only after a T030 PASS, the Codex CLI path against
the same persisted timeline without weakening privacy, provenance, cancellation, or the
no-key map tier. A FAIL/INCONCLUSIVE verdict is a supported outcome: Codex remains
unavailable and the extension seam is still evaluated with the fake third backend.

## Plan

1. Run identical summary and topical-clustering inputs through direct OpenAI and every
   T030-approved backend using the same role-selection path. After FAIL/INCONCLUSIVE,
   assert Codex is unavailable rather than attempting a live call.
2. Cut CLI commands over from the legacy five-provider `Provider` constructor to T024's
   resolved registry and T025/T026 connectors; the hand-written OpenAI Chat Completions
   path must no longer be reachable from the CLI.
3. Keep CI credential-free with a fake Codex executable and mock Responses server. Real
   Codex-login and Keychain-backed OpenAI runs are explicit ignored canaries.
4. Assert cited event ids exist, the timeline is byte-identical, backend fingerprints
   separate caches, and normalized cancellation/errors behave the same.
5. Assert the first request is transcript-only and contains no `ScreenSnapshot` lines.
   Exercise T028 inspection separately; use T031's provider-neutral attachment only after
   explicit image consent, and prove denied/unsupported images never reach a transport.
6. Exercise JSON-object output on both backends. Exercise caller-constrained JSON Schema only
   through T031 and only where the resolved backend advertises it; an honest unsupported
   result is preferable to silently weakening the constraint.
7. Register a fake third backend and run the same consumer flow without changes to
   `core`, `insight`, or app selection logic.
8. Measure time-to-first-delta and total latency p50/p95 for direct OpenAI and, after a
   T030 PASS, Codex process startup plus representative recap and watcher-shaped prompts.
9. Record an explicit verdict: Codex may enter T013's realtime loop, remains an offline
   summary/clustering backend, or is unavailable because T030 did not pass.

## Contract for downstream tasks

T013 remains blocked until this task records the Codex realtime verdict and the existing
T035 map-tier gate passes. T019's participant-recognition gate requires the same accepted real
session.

## Acceptance

- Credential-free CI covers both normalized backend paths.
- After a T030 PASS, a live Codex run succeeds with no Sotto API key configured; after
  FAIL/INCONCLUSIVE, tests prove it cannot be selected.
- A live OpenAI run succeeds using a Keychain-backed key.
- Both produce valid event citations without mutating the timeline.
- Backend switching cannot reuse a derived artifact from the other backend.
- CLI commands no longer instantiate the legacy five-provider or hand-written OpenAI Chat
  Completions path.
- Transcript-only default, no eager screen lines, and opt-in screen inspection are test-asserted.
- Schema and image behavior follows T031 capability/consent rules without CLI-owned changes to
  `core`, `providers`, `insight`, or `screen`.
- Cancellation and actionable errors have parity.
- Startup and stream p50/p95 plus the Codex offline/realtime verdict are recorded.

## Out of scope

Implementing the realtime advisor, adding vendors, and treating synthetic output as the
real-call product gate.

## Notes

- 2026-08-11 automated slice: CLI `Summarize` and `Cluster` now resolve stable `none`/OpenAI/Codex
  states through `Registry`; `none` is a normal no-credential outcome and T030 FAIL keeps Codex
  unresolvable. Legacy `ProviderKind`, Ollama default, hand-written adapter construction, and eager
  `--screen-context ocr` are absent from the CLI source.
- Credential-free mock Responses coverage passes for cited summary/clustering, timeline
  immutability, same-fingerprint cache reuse, fake-third-backend extensibility, transcript-only
  first request, denied/authorized one-shot screen inspection, strict schema shape, rate-limit and
  cancellation normalization, and deterministic p50/p95 aggregation. The focused result is 4
  passed, 0 failed, 1 ignored; exact commands are recorded in
  `docs/experiments/openai-first-t029-automated.md`.
- The ignored Keychain/live canary was added but not run. T035 real-session acceptance, live
  OpenAI result/citation review, and real startup/TTFT/total p50/p95 remain mandatory. T030 FAIL
  means no live Codex or Codex latency claim. The task is therefore `in-review`, not done.
- T027's persisted settings semantics are confined to the app and do not couple the CLI eval path.
  Existing Apple Vision fixture failure (`Foundation._GenericObjCError.nilError`) is an unrelated
  local gate and is not counted as T029 Responses evidence.
- 2026-08-11 review hardening: Codex now returns the authoritative T030 isolation-unavailable
  result before model validation or connector construction. Summary/clustering require advertised
  `JsonObjectOutput` before provider I/O; the fake third backend advertises it explicitly. A
  resolved text backend that asks for image evidence is proven to stop after its first text call
  with an unsupported-image result, never transporting the image. TTFT and total latency report
  independent sample counts so missing text cannot be hidden by a shared minimum count.
- The ignored live gate now requires `SOTTO_T035_ACCEPTANCE_ACK=owner-accepted`, an explicit new
  `SOTTO_T035_EVIDENCE_PATH`, and `SOTTO_T035_LATENCY_SAMPLES` of at least five. If deliberately
  run after T035 acceptance, it executes summary and clustering, validates all cited/linked event
  ids, compares timeline bytes, gathers repeated Keychain/startup/TTFT/total observations, and
  writes p50/p95 evidence. It remains unrun and cannot itself mark T029 or T035 accepted.
- Final live-gate hardening rejects an empty/vacuous recap, rejects clustering without at least one
  cited region/link/open thread, and requires `cached == false`; the operator must use a fresh T035
  evidence database rather than relabel a prior derived artifact as a live result.
- 2026-08-11 independent final review accepts the automated/live-gate slice. The ignored canary now
  rejects blank semantic content in every evidenced recap item and every cluster region, link, or
  open thread, with focused blank-summary and blank-cluster regressions. It still requires the
  explicit T035 owner acknowledgement, a create-new evidence path, at least five samples, uncached
  clusters, known event ids, byte-identical timeline data, and recorded startup/TTFT/total p50/p95;
  Codex remains skipped under T030 FAIL. Fresh-target `--locked` no-run compilation, both focused
  regressions, strict CLI test clippy, scoped rustfmt, and scoped diff-check pass. The ignored live
  run was not executed, so status remains `in-review` pending T035 acceptance and real evidence.

## Independent automated-slice review — accepted (2026-08-11)

- Confirmed the CLI has no reachable legacy `ProviderKind`, hand-written Chat Completions, or
  Ollama-default reasoning path. `none` returns before database, credential, provider, or network
  work; Codex returns T030's authoritative isolation failure before model validation or probing.
- Confirmed summary and clustering require advertised `JsonObjectOutput` before provider I/O, the
  fake third backend declares that capability explicitly, and schema dispatch remains gated by the
  resolved OpenAI capability. Missing capability is covered by a zero-call regression.
- Confirmed transcript-only first requests exclude OCR, frame paths, and image input. Denied image
  policy transports no bytes; an authorized OpenAI second pass carries path-free provenance; a
  resolved text backend stops after the first inspection request and rejects the image before
  transport.
- Confirmed cited summary/clustering output, byte-identical timeline state, cache fingerprint
  separation, usage, normalized rate-limit/cancellation, and independent TTFT/total percentile
  populations are covered by credential-free tests.
- Confirmed the ignored live gate cannot pass on a vacuous or cached artifact: it requires explicit
  T035 owner acknowledgement, a create-new evidence path, at least five matched latency samples,
  nonempty known citations from both summary and clustering, `cached == false`, timeline byte
  identity, and written startup/TTFT/total p50/p95 evidence.

The independently reviewed automated slice is accepted. T029 remains `in-review` because T035 is
still `todo` and the Keychain-backed live OpenAI quality/citation/latency gate has not run. No live
Codex run or Codex latency claim is permitted after T030 FAIL.
