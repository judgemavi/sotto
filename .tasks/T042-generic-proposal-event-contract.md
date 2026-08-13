# T042 — Generic proposal timeline contract

**Status:** done

**Wave:** P0 — proposal schema gate

**Depends on:** T036; T038 completion/app ownership handoff; T029's accepted automated-slice CLI
handoff; T016's accepted automated board-code handoff; T001's frozen-core amendment below. T029,
T016, and T035 manual/live gates remain required for later product acceptance, not this schema cutover.

**Owns:** `crates/core/src/types.rs`, `crates/core/src/timeline/**`, `crates/core/src/lib.rs`,
core tests/fixtures, `crates/cli/src/main.rs`, `crates/app/src/lib.rs`,
`crates/cli/src/pipeline.rs`, focused CLI proposal-latency tests,
`crates/app/src/board/projection.rs`,
`docs/adr/0012-proposal-events.md`, `.tasks/T042-generic-proposal-event-contract.md`

## Goal

Replace the sales-only trigger/suggestion timeline vocabulary with meeting-general proposal
events while preserving append-only anchors and supersession.

## Plan

1. Define proposal kinds: clarifying question, decision check, next step, follow-up, and relevant
   context.
2. Define partial/final proposal events with same-session anchors and typed meeting/external
   evidence references.
3. Distinguish proposed, dismissed, copied, and explicitly accepted UI state from meeting fact.
4. Perform a one-way schema/fixture/call-site migration; do not retain dual event names.
5. Record compatibility and migration consequences in ADR-0012.

## Contract for downstream tasks

Proposal display is append-only, anchored, and explicitly typed as system/model output rather
than captured meeting fact. A proposal without a known same-session meeting anchor is invalid.
External evidence ids are references to a durable T041 context bundle, not core MCP types.
Constructors and replay validation enforce that anchors are prior same-session captured meeting
facts; proposal/system-output events cannot anchor another proposal as if it were meeting evidence.

## Acceptance

- Serialization round trips the new schema and rejects invalid anchors/evidence shape.
- Old sales trigger/suggestion names are absent from production call sites and current fixtures.
- Partial-to-final supersession never mutates or relocates the anchor.
- `core` still has no dependency on providers, insight, advisor, MCP, or app.

## Out of scope

Watcher/proposer logic, MCP retrieval, rendering, and acceptance evaluation.

## Notes — 2026-08-12 implementation handoff

- Replaced the sales-only types and wire names with private, fallibly constructed generic proposal
  values and `proposal.trigger|partial|final|disposition|run_audit` timeline kinds. Meeting anchors
  and meeting evidence are typed `EventId`s; external evidence is a bounded core-owned opaque
  reference with no MCP dependency.
- Added checked builder methods plus strict/lenient replay validation. Anchors must name earlier
  same-session captured facts. Partial streaming and finalization preserve kind, anchors, and both
  evidence vectors; proposal output cannot supersede captured facts and ordinary payloads cannot
  supersede proposal output. Checkpoint state retains compact proposal provenance, not generated
  text.
- Classified captured facts, system output, user interactions, and diagnostics explicitly.
  Dismissed/copied/accepted remain append-only UI interactions, never meeting facts. Provider
  fingerprint, optional usage, and completed/cancelled/failed outcome live in a separate
  provider-neutral run-audit event with no SDK, credential, endpoint, prompt, or error text.
- Migrated the spike scene, board projection exhaustiveness, CLI event filters, and the optional
  latency field to proposal vocabulary. ADR-0012 records the pre-release one-way reset policy; no
  legacy reader or dual schema remains.
- PASS — `cargo test -p core --all-features --locked`: 19 unit, 8 pipeline, and 3 request-contract
  tests plus doc tests.
- PASS — `cargo clippy -p core --all-targets --all-features --locked -- -D warnings`.
- PASS — app library tests with a fresh target, `WHISPER_DONT_GENERATE_BINDINGS=1`,
  `SOTTO_SKIP_SWIFT_BUILD=1`, and `gpui/runtime_shaders`: 73 passed.
- PASS — CLI library tests: 5 passed. The combined app/CLI all-target run additionally passed app
  main, CLI main, and 3 headless harness cases; one real-ASR canary was ignored.
- PASS — app/CLI all-target Clippy with the same environment substitutions and warnings denied.
- PASS — `cargo fmt --all -- --check`, `git diff --check`, and the owned production/test vocabulary
  sweep (no old suggestion/sales taxonomy or bare `trigger` wire name).
- NOT A T042 FAILURE — the pre-existing Apple Vision fixture test
  `pricing_frame_contains_text_readable_by_vision` failed with
  `Foundation._GenericObjCError.nilError`; this environment-specific residual is already recorded
  by T029 and is unrelated to proposal schema behavior.

## Run-audit review fix — 2026-08-12

- Enforced the terminal phase matrix in both checked append and persisted replay: completed targets
  an active final, while cancelled/failed target an active trigger or partial.
- Enforced one terminal run audit per referenced event. An audited partial cannot be streamed or
  finalized later; a later proposal with the same anchors remains an independent run.
- Added builder and serialized-replay adversarial regressions for completed-to-partial,
  cancelled/failed-to-final, contradictory duplicate audits, and audited-partial supersession.
- PASS — `cargo test -p core --all-features --locked`: 21 unit, 8 pipeline, and 3 request-contract
  tests plus doc tests.
- PASS — strict core all-target/all-feature Clippy, Rustfmt check, and diff check.

## Independent acceptance — 2026-08-12

- Accepted the one-way generic proposal schema, contextual same-session captured-fact anchor
  validation in both checked construction and persisted replay, stable partial-to-partial-to-final
  provenance, cross-kind rejection, active-final dispositions, and compact checkpoint state.
- Accepted the provider-neutral run-audit phase matrix, duplicate-terminal and audited-partial
  fencing, and its adversarial builder/replay regressions. Core remains independent of reasoning,
  MCP, and UI crates; local retrieval citations remain explicitly deferred to a distinct future
  core evidence type rather than being encoded as external evidence.
- PASS — `cargo test -p core --all-features --locked`: 21 unit, 8 pipeline, and 3
  request-contract tests plus doc tests.
- PASS — `cargo clippy -p core --all-targets --all-features --locked -- -D warnings` and the
  owned proposal-vocabulary/diff checks. The workspace-wide Rustfmt check was blocked only by an
  independently active T045 formatting delta in `crates/rag/src/store.rs`; no T042-owned file was
  reported.
