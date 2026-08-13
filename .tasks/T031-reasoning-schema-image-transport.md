# T031 — Provider-neutral reasoning schema and image transport

**Status:** done

**Wave:** R2 — sequential request-contract extension before integration evals

**Depends on:** T025/T030 provider handoff; T026; T028

**Owns:** `crates/core/src/types.rs`, `crates/core/src/traits.rs`, `crates/core/src/lib.rs`,
`crates/providers/src/backend/**`,
`crates/providers/src/openai/**`, `crates/providers/src/codex/**`,
`crates/insight/src/context/**`, `crates/core/tests/reasoning_request_*`,
`crates/insight/tests/reasoning_request_*`, `docs/adr/0010-reasoning-request-shape.md`.
Provider unit tests inside the owned connector/backend directories are included. No manifest or
`Cargo.lock` changes without a planner amendment.

**Security-review ownership amendment (2026-08-11):** also owns
`crates/screen/src/inspection/**`, the matching exports/tests in `crates/screen/src/lib.rs`,
`crates/providers/src/reasoning.rs`, `crates/providers/src/lib.rs`,
`crates/providers/Cargo.toml`, and the resulting `Cargo.lock` edge. This planner-approved
amendment replaces the forgeable core consent value with screen-owned opaque authorization.

The `traits.rs`/`lib.rs` ownership is limited to a source-compatible reasoning-request wrapper
and default provider method. It must not add a `core -> providers` or `core -> screen` edge,
or move summarization, inspection policy, or other reasoning orchestration into `core`.

## Goal

Extend Sotto's text-only `CompletionRequest` with the smallest Sotto-owned contracts needed for
caller-constrained JSON Schema and an explicitly consented retained image. Keep SDK, wire, CLI,
and local-path types inside their connectors. This task transports evidence; it does not make
screen capture eager, add general tool calling, or move reasoning into `core`.

## Plan

1. Add provider-neutral output constraints that distinguish ordinary text, valid JSON object, and
   caller-constrained JSON Schema. The schema representation is stable Sotto data, not an
   `async-openai` or Codex protocol type. Validate malformed/unsupported constraints before I/O.
2. Add a provider-neutral image input carrying bounded bytes, declared media type, and screen
   provenance/consent evidence. Never pass a filesystem path, frame-cache root, or provider SDK
   object across the boundary. Preserve a text-only construction path for existing consumers.
3. Bridge only a successful T028 `ScreenInspection::Available` result. Missing, pruned,
   out-of-range, OCR-only, or denied-consent results remain typed absence and create no attachment.
   The initial reasoning request remains transcript-only with no `ScreenSnapshot` lines.
4. Map JSON object, JSON Schema, and image input through the Responses SDK. Advertise
   `JsonSchemaOutput`/image capability only after end-to-end request-shape tests prove the mapping;
   unsupported combinations fail explicitly rather than silently degrading to prompt instructions.
5. Keep Codex capability metadata honest. Add a mapping only if its post-T030 protocol supports
   the same contract without exposing tools or leaking local paths; otherwise reject schema/image
   requests and advertise neither capability.
6. Prove opt-in is checked at the last common boundary before provider dispatch, so a future
   caller cannot bypass the T028 orchestration helper. Bound attachment size and zero/drop owned
   bytes promptly after the request completes where the chosen representation permits.
7. Record the cross-crate schema change and privacy boundary in ADR-0010. Confirm `core` remains
   headless and depends on no provider or screen crate.

## Contract for downstream tasks

T029 consumes these request types but does not edit `core`, `providers`, `insight`, or `screen`.
T013 may request a second-pass image only through this contract after T028 inspection. Backend
capability checks remain the switch point, so a future connector can implement the contract
without consumer branching.

## Acceptance

- Existing text-only requests remain source-compatible where practical and behavior-compatible.
- JSON object and JSON Schema are distinct; unsupported schema requests fail before transport.
- A mock Responses request proves the exact schema and image payload without hand-written SSE.
- No inspection request, missing/pruned evidence, denied consent, or unsupported backend produces
  image bytes on any transport.
- Image attachments carry provenance but no local path, and have a tested size bound.
- Codex advertises/maps only capabilities its isolated protocol actually proves.
- SDK/CLI types do not escape `providers`; `core` has no dependency on `providers` or `screen`.
- Focused core/provider/insight tests and strict Clippy pass without credentials.

## Out of scope

General tool calling, automatic image sending, continuous OCR, UI consent design, realtime advisor
policy, or adding another vendor.

## Implementation evidence

- `core::ReasoningRequest` preserves text, JSON-object, and strict JSON-Schema output, but contains
  no image, consent, local-path, screen, or provider type.
- `screen::AuthorizedReasoningImage` is opaque, path-free, non-cloneable, non-Serde, and has no public
  constructor. `RetainedScreenInspector` alone mints it after `ImageInspectionPolicy::Allow`,
  selector/event/frame/interval agreement, canonical cache-root containment, a bounded read, and
  PNG/JPEG byte sniffing. Denied, mismatched, substituted, invalid, or oversized files mint
  nothing.
- The providers-owned `ReasoningProvider` seam accepts the opaque authorization separately from
  the core request. Its default rejects image input before delegation; a marker executable proves
  Codex schema/image rejection occurs before spawn.
- Direct OpenAI maps strict schema and the authorized image through `async-openai` with
  `strict: true`, `store: false`, `tools: []`, and `tool_choice: none`. Its connector revision is
  r3 so caches cannot reuse r2 output across the tightened authorization contract.
- Independent review validation on 2026-08-11: core contract 3 passed; screen 9 passed; providers
  51 passed/3 ignored; insight 9 passed; strict all-target Clippy passed for
  core/screen/providers/insight; formatting and scoped diff checks passed. No credentialed OpenAI
  or live Codex canary was run.

## Independent security review (2026-08-11)

Accepted after changes. The original public `ReasoningImageConsent::ExplicitUserOptIn` and
`ReasoningImage::new` were forgeable by any safe caller and therefore did not satisfy the
last-boundary acceptance criterion. They were removed rather than documented as policy. Image
authority now originates only in the screen cache/policy crate and cannot be directly constructed
or deserialized. Core remains headless with no `core -> screen` or `core -> providers` dependency.
