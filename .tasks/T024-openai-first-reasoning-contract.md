# T024 — OpenAI-first reasoning backend contract and ADR

**Status:** done

**Wave:** R0 — gates every new reasoning connector

**Depends on:** T007 (historical provider implementation and frozen
`core::CompletionProvider` contract)

**Owns:** `crates/providers/src/backend/**`, `crates/providers/src/lib.rs`,
`crates/providers/Cargo.toml`, `docs/adr/0008-openai-first-reasoning.md`, `AGENTS.md`

## Goal

Make Codex CLI and the OpenAI Responses API the only product-facing v1 reasoning
backends while keeping the boundary open to future implementations. Codex is the
starter cloud path and uses its own ChatGPT login; direct OpenAI API access is an
optional BYOK path. No reasoning remains a normal configuration.

This supersedes T007's five-provider, hand-written transport direction without
rewriting that completed task's history.

## Plan

1. Keep `core::CompletionProvider` as the provider-neutral streaming primitive.
   `core` must not depend on `providers`, SDK types, subprocess types, or auth state.
2. Add a provider-layer backend descriptor with an open-ended stable backend id,
   display name, model id, capability set, auth kind/status, and cache fingerprint.
   Do not use a closed vendor enum as the extension seam.
3. Preserve independent role selection for watcher, suggester, and summarizer. A call
   resolves and pins one backend at start; later settings changes affect only new calls.
4. Define the common behavior both connectors must normalize: streaming deltas,
   cancellation, structured output, usage when available, actionable errors, and
   backend-aware cache identity.
5. Record the architecture in ADR-0008, including:
   - Codex CLI is a user-installed optional runtime, not a bundled sidecar.
   - Sotto never reads or copies Codex credentials.
   - OpenAI API keys remain optional and stay in Keychain.
   - other providers are deferred, not promised by the v1 UI.
   - the direct API adapter uses a pinned maintained Rust SDK rather than our own SSE
     protocol implementation.
6. Update the living architecture in `AGENTS.md` so it no longer instructs future work
   to build five hand-written provider transports or present API keys as the primary
   reasoning path.
7. Evaluate candidate Rust SDKs for the Responses API against streaming, structured
   output, tool/image inputs, cancellation, maintenance, dependency weight, and type
   isolation. Record the selection in the ADR; official OpenAI SDKs do not currently
   include Rust, so the chosen community SDK stays behind Sotto's contract.

## Contract for downstream tasks

T025 and T026 implement the two initial backends behind this boundary. T027 reads only
backend descriptors and role-selection state. Insight and advisor consumers continue
to depend on `Arc<dyn CompletionProvider>`, not concrete connectors.

## Acceptance

- Codex CLI and OpenAI API can be selected by stable backend id without consumer-side
  branching.
- A fake third backend registers without edits to `core`, `insight`, or `app`.
- Backend fingerprint, not model name alone, separates derived-view cache entries.
- Role switching preserves cancellation, usage, and error semantics.
- No configured backend is represented as an ordinary, non-error state.
- `core` remains headless and has no dependency on `providers`.
- ADR-0008 and `AGENTS.md` explicitly supersede the affected T007 architecture decisions.

## Out of scope

Launching Codex, calling the Responses API, settings UI, prompt content, and screen
inspection.

## Notes

- Added the provider-layer backend contract under `crates/providers/src/backend/`:
  open-ended stable ids, capability sets, actionable auth kind/status, and an
  unambiguous cache fingerprint over backend id, model id, and connector revision.
- Replaced the old role-to-provider map with registration plus independent role
  selection. Resolution pins an immutable descriptor and
  `Arc<dyn CompletionProvider>`; later role or readiness changes affect only new
  calls. No selection remains `Ok(None)`, while a selected unready backend is an
  actionable error.
- Added an explicit auth-status refresh for `codex login status` and Keychain changes.
  A regression test proves refresh does not alter cache identity or already resolved
  calls.
- Tests register an invented third backend without a vendor enum change, distinguish
  Codex and Responses fingerprints even with the same model name, preserve
  cancellation/usage through a pinned provider, and keep role choices independent.
- ADR-0008 and the living `AGENTS.md` now supersede the five-provider hand-written
  product direction. Connector construction stays local to T025/T026 rather than
  introducing a closed configuration/factory enum.
- Selected `async-openai = "=0.41.3"` for T026 with only `responses` and `rustls`
  features. Its inspected Responses surface covers streaming, JSON Schema output,
  image input, tools, and usage; T026 still owns transport-cancellation and mock-stream
  proof before the SDK enters the product path.
- Verification: `cargo fmt -p providers -- --check` and strict provider clippy pass.
  `cargo test -p providers` passes 14 unit tests with 5 manual live checks ignored.
  The first sandboxed test run could not bind the existing localhost mock servers;
  the approved unsandboxed rerun passed all tests.

## Review — accepted (2026-08-11)

The provider-layer contract preserves the frozen `core::CompletionProvider` seam while
adding the identity, capability, readiness, fingerprint, and call-pinning behavior the two
connectors need. Auth refresh changes only future resolutions, and the same-model test proves
Codex and direct OpenAI cannot share a cache identity. The factory deferral is deliberate and
recorded rather than hidden behind a new closed connector enum.

Root re-ran the five focused backend contract tests with `--locked`; all passed. T025 may
implement Codex against this boundary. T026 still owns proving the selected SDK's cancellation
and mock Responses behavior before that dependency enters the product path.

## Post-handoff contract clarification

The common mandatory backend baseline is **streaming plus cancellation**, not one ambiguous
"structured output" capability. `JsonObjectOutput` and caller-constrained `JsonSchemaOutput`
are separate optional capabilities, as implemented during T026. Likewise, the acceptance item
about selecting Codex and OpenAI by stable id proves registry resolution, not that both are
currently safe and user-selectable: T030 must establish Codex's model-visible no-tools boundary,
and T027 owns the eventual product selection surface. This clarification preserves the accepted
T024 evidence while preventing downstream tasks from over-reading it.
