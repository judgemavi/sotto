# T067 — Shape reasoning requests to what the chosen backend can honour

**Status:** done

**Wave:** R1 — first no-API-key reasoning path

**Depends on:** nothing. T024's backend contract and T034's runtime registry are accepted.

**Owns:** the request construction in `crates/insight/src/ask.rs` and
`crates/insight/src/context/mod.rs`, the normalization seam in
`crates/providers/src/backend/mod.rs`, and this task

**Acceptance propagation (coordinator-authorized, 2026-08-13):** the narrow result-path changes in
`crates/providers/src/reasoning.rs`, `crates/insight/src/lib.rs`,
`crates/insight/src/notes/mod.rs`, `crates/insight/tests/meeting_notes.rs`, and
`crates/app/src/workspace/ask.rs` are also owned by T067. They close the review finding that a
diagnostic ledger alone was not the result contract required below.

**Concurrency (planner, 2026-08-13):** T025 holds `crates/providers/src/codex/**` — do not edit it.
If the Codex connector's `validate_request` needs to change, say so and stop; that is T025's call.

## Why this exists

Codex cannot execute a single Sotto reasoning call today, and it is not a Codex defect.

`crates/insight/src/ask.rs:298-299` and `crates/insight/src/context/mod.rs:170-171` set `max_tokens`
and `temperature` on every request. The Codex CLI's stable `codex exec` invocation cannot honour
either, so `validate_request` (`crates/providers/src/codex/mod.rs:278-287`) rejects both. Every
insight call through Codex therefore fails before it starts.

This was found by T025 and re-confirmed twice. T025 recorded it as an external dependency rather
than working around it, which was right: the controls exist for a reason and silently dropping them
would change model behaviour invisibly.

## The rule this task establishes

A request carries what the caller *needs*. The backend declares what it *can honour*. Where they
disagree, the disagreement is resolved explicitly and visibly — never by silently discarding a
control the caller set deliberately.

Three outcomes are legitimate, and the caller must be able to tell which occurred:

1. The backend honours the control. Nothing to do.
2. The backend cannot honour it, and the call proceeds without it because the difference is
   tolerable for that call. This must be **recorded on the result**, not swallowed.
3. The backend cannot honour it and the difference is not tolerable, so the call fails with a
   reason naming the control and the backend.

What is not legitimate is the current implicit fourth option: the request is constructed for one
backend's capabilities and simply breaks on another.

## Plan

1. Extend the backend capability declaration so a backend states which sampling controls it can
   honour. `BackendCapability` already exists; use it rather than inventing a parallel mechanism.
2. Normalize the request against the selected backend's declaration before dispatch, at one place
   both the ask and notes paths pass through. Two independent normalizations would drift.
3. Decide, per control, which of the three outcomes above applies — and justify each in the task
   file. `max_tokens` protects against runaway cost and latency; `temperature` affects determinism
   of structured output. They may not deserve the same treatment.
4. Surface the outcome. If a run proceeded without a control the caller set, the user or the caller
   can see that it did. A quiet downgrade is the failure mode this task exists to prevent.
5. Do not weaken the Codex connector's rejection. It is correct to refuse a control it cannot
   honour; the fix belongs on the request side.

## Acceptance

- A reasoning request built by the ask path and by the notes path both execute against a backend
  that cannot honour `max_tokens` or `temperature`, without either control being silently dropped.
- The outcome for each unhonoured control is observable on the result.
- A control whose loss is judged intolerable fails the call with a reason naming the control and the
  backend, asserted by test.
- The OpenAI Responses path is unaffected: controls it honours are still sent and still applied.
- Normalization happens in exactly one place, asserted by a test that would fail if a second path
  bypassed it.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

The Codex connector itself (T025), backend selection UI, adding new sampling controls, and any
change to the reasoning request schema in ADR-0010 beyond what normalization requires.

## Resolution (2026-08-13)

`BackendCapability` now declares `MaxTokens` and `Temperature`. `Registry::resolve` pins those
capabilities into one backend-bound provider wrapper; all three dispatch shapes (`stream`,
`stream_reasoning`, and `stream_advanced_reasoning`) call the same `prepare_reasoning_request`
function exactly once before the connector sees the request. Ask and notes keep stating their needs
as `max_tokens` plus `temperature`; they do not construct a Codex-shaped request themselves.

The insight policy permits both controls to downgrade, but does not pretend the loss is neutral:

- Losing `max_tokens` removes the requested output-latency ceiling. It is tolerated only for the
  current explicit, cancellable, subscription-backed Ask and post-call notes paths. Codex's JSONL
  event-size bound is not a total-output bound and is not treated as a substitute.
- Losing `temperature = 0` removes a determinism preference. It is tolerated because structured
  output, schema parsing, and citation validation remain fail-closed; the downgrade is still
  reported.

Every tolerated loss produces a `RequestNormalization` naming the backend and control. Resolved
dispatches retain `ObservedRequestNormalization` entries with a per-dispatch id, so observations
from two calls cannot be confused. The public preparation result also returns its exact downgrade
list. `RequestNormalizationPolicy::STRICT` proves that a caller which cannot tolerate a loss fails
before connector I/O with an error naming both control and backend. The Codex connector's own
unsupported-control rejection remains unchanged.

The observable result contract is now literal rather than indirect. `AskResult`,
`MeetingNotesReport`, and `GroundedMeetingNotesReport` each carry `normalizations`. Every
`ResolvedBackend::provider()` handle owns a separate result ledger, so concurrent consumers cannot
drain or misattribute each other's observations; the resolution also retains an independent shared
diagnostic ledger. The reasoning consumer drains its provider handle into the fresh result, so
dropping `ResolvedBackend` cannot lose the evidence. Supported backends and cache hits return an
explicit empty list. `AskReply` remains the model-wire reply used in conversation history; the app
receives `AskResult`, stores only its reply in history, and visibly names downgraded controls.

OpenAI advertises both sampling capabilities. Its preparation test proves the original
`max_tokens` and `temperature` values survive unchanged and no downgrade is reported.

## Acceptance evidence (2026-08-13)

- `cargo test -p providers backend::tests --no-fail-fast` — PASS, 14 passed.
- `cargo test -p providers --no-fail-fast` — PASS outside the restricted sandbox because its HTTP
  fixtures bind loopback, 70 passed, 10 ignored live/manual tests across unit and live targets.
- `cargo test -p insight --no-fail-fast` — PASS, 37 passed across unit and integration targets.
  This includes the fake-Codex cited-notes integration through the resolved normalization seam.
- `cargo test -p app --lib --no-fail-fast` — PASS, 121 passed.
- Ask seam proof — PASS:
  `ask::tests::ask_dispatch_uses_backend_normalization_and_exposes_downgrades`.
- Notes seam proof — PASS:
  `context::tests::notes_context_dispatch_uses_the_resolved_backend_normalization_seam`.
- OpenAI preservation proof — PASS:
  `openai::tests::backend_normalization_preserves_openai_sampling_controls`.
- Intolerable-control proof — PASS:
  `backend::tests::required_unsupported_control_names_control_and_backend`.
- Fresh unsupported-backend Ask and notes tests assert `result.normalizations`; the fake-Codex notes
  integration asserts `MeetingNotesReport.normalizations`, and supported/cache-hit tests assert an
  explicit empty list. Each consumer proves its result owns the evidence while the independent
  resolution diagnostic retains the same attributed observations.
- `cargo clippy -p providers -p insight -p app --all-targets -- -D warnings` — PASS.
- `cargo fmt --all -- --check` — PASS.
- `cargo clippy --workspace --all-targets -- -D warnings` — PASS after concurrent T050 completed.
- `cargo test --workspace --no-fail-fast` — PASS after concurrent T050 completed; all automated
  workspace tests and doc tests passed, with only the repository's declared ignored
  download/live/manual/performance gates remaining ignored.
