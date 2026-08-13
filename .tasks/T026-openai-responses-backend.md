# T026 — OpenAI Responses API backend through a Rust SDK

**Status:** done

**Wave:** R1 — optional direct API path

**Depends on:** T024; schedule after T025 unless ownership of the providers manifest and
module registry is explicitly transferred

**Owns:** `crates/providers/src/openai/**`, OpenAI-specific provider tests and fixtures;
`crates/providers/src/backend/**`, `docs/adr/0008-openai-first-reasoning.md`,
`crates/providers/src/lib.rs`, `crates/providers/Cargo.toml`, and `Cargo.lock` during its
sequential handoff

## Goal

Provide optional BYOK access to the OpenAI Responses API behind the same contract as
Codex CLI, using the SDK selected by T024 rather than Sotto's hand-written Chat
Completions/SSE implementation.

## Plan

1. Implement the direct OpenAI Responses API adapter through the pinned SDK selected in
   ADR-0008. T027 owns the app-settings cutover and T029 owns the CLI consumer cutover;
   the legacy constructor is not removed from product use until both land.
2. Keep SDK request, event, error, and tool types inside this module. Normalize them to
   Sotto's request/delta/usage/error types at the boundary.
3. Preserve cancellation by dropping/aborting the SDK stream and prove the underlying
   transport closes.
4. Support JSON-object output required by the current recap, clustering, and watcher
   prompts without claiming caller-constrained JSON Schema. Keep image/tool capability
   metadata honest even before T028 consumes it.
5. Load the optional OpenAI key through the existing Keychain path. CI and the Codex
   path must remain fully usable without one.
6. Keep legacy adapters parked without implying v1 support. T027 and T029 own removing
   them from app and CLI product paths.

## Contract for downstream tasks

T027 can switch between this backend and T025 using only T024 descriptors and role ids.
Insight/advisor code imports no SDK types.

## Acceptance

- Mock Responses streams cover text deltas, JSON-object output, usage, refusal, rate
  limit, malformed data, and cancellation.
- Aborting a call closes the in-flight transport and preserves normalized cancellation
  semantics.
- An ignored live test uses a Keychain-backed key and produces a cited fixture recap.
- Focused provider CI passes with no OpenAI API key. The workspace-wide Whisper bindings
  failure is a separate build-infrastructure residual and must not be reported as green here.
- Backend fingerprint prevents cache reuse with a Codex run that reports the same model.
- There is no hand-written SSE parser inside the new Responses adapter. T027/T029 must remove
  the legacy adapter from app and CLI product paths before the overall cutover closes.
- JSON Schema and image attachment are not implied by JSON-object support; T031 owns the
  provider-neutral request seam and transport mappings before T029 may exercise either.

## Out of scope

Other model providers, settings UI, prompt design, and automatic screen-image sending.

## Notes

- Replaced the product-facing `openai::configured` path with `OpenAiProvider`, backed by
  the exactly pinned `async-openai = 0.41.3` Responses feature and rustls. SDK client,
  request, stream-event, usage, and error types remain private to `providers::openai`;
  consumers still receive only `CompletionProvider`, `Delta`, `Usage`, and
  `ProviderError` values.
- Requests use `POST /responses` through the SDK's typed client and parsed event stream.
  They are stateless (`store: false`), default to transcript-only context, and expose no
  tools. Sotto does not contain an OpenAI SSE parser on this path. The connector rejects
  unsupported stop sequences instead of silently discarding them. It omits
  `stream_options`, preserving OpenAI's default stream-obfuscation mitigation. ADR-0010
  subsequently added strict JSON-Schema and explicitly consented bounded-image mappings
  through the same contained SDK boundary.
- The initial reasoning roles default to Responses **JSON object mode**. This guarantees
  valid JSON but is not schema-constrained Structured Outputs. T031 retained the legacy
  text-only request and added a provider-neutral `ReasoningRequest` for strict schema and
  image dispatch, without leaking SDK types. The OpenAI connector now advertises and maps
  those proven shapes; ordinary recap and clustering calls remain JSON-object and
  transcript-first.
- API keys remain optional. `from_keychain` loads through the existing OS credential
  store, a missing key yields `NeedsApiKey`, and stream attempts without a key return the
  normalized auth error. Tests use only a mock secret and require no environment key.
- The common backend baseline is now only streaming plus cancellation. Capability
  metadata distinguishes `JsonObjectOutput` from `JsonSchemaOutput`. Following T031,
  OpenAI truthfully advertises both output modes, usage reporting, and image input because
  each has a captured SDK request-shape regression. It still omits tool calling. Codex
  advertises neither schema nor image and remains unavailable after T030's isolation
  failure.
- Mock Responses tests cover incremental text/JSON, request shape, usage including
  cached input tokens, refusal redaction, stream and HTTP rate-limit normalization,
  `response.failed` and incomplete terminal classification, malformed events, and
  cancellation both before response headers and after text. Both
  cancellation cases prove that dropping the SDK future/stream closes the loopback
  transport; pre-text cancellation returns `Cancelled`, while post-text cancellation
  emits a terminal `Aborted` delta without fabricated usage.
- Per-call model ids must exactly match the configured descriptor model, preventing a
  request from invalidating the backend fingerprint/cache identity.
- The OpenAI connector revision is `2` after the T031 schema/image transport expansion,
  so artifacts created under the earlier connector contract cannot reuse the same
  backend fingerprint. The regression also retains connector-id separation from a Codex
  run that reports the same model.
- A manual ignored canary loads only the Keychain-backed key and requires a JSON recap
  whose citations point to timestamped fixture event ids. It requires the operator to
  set `SOTTO_OPENAI_MODEL` to a currently enabled Responses model. It was not run and
  consumes user tokens when explicitly invoked.
- SDK limitations retained at the boundary: this is a community Rust SDK rather than an
  official OpenAI SDK; its `ApiError` does not expose response headers, so normalized
  HTTP 429 errors cannot preserve `Retry-After`; its feature still brings its own parsed
  SSE/Tower/support dependency set. Refusal prose is intentionally discarded rather
  than echoed through cross-crate errors.
- Historical Anthropic, Google, OpenRouter, Ollama, and hand-written OpenAI transport
  code remains parked for compatibility during T027; it is not the product-facing
  direct OpenAI constructor and is not a v1 support promise.
- Final verification: `cargo fmt --all -- --check`,
  `cargo clippy -p providers --all-targets --locked -- -D warnings`, and
  `cargo test -p providers --locked` pass without an OpenAI environment key. The provider
  unit suite passes 51 tests with 3 manual checks ignored; the 5 historical provider live
  tests are also ignored. Within the OpenAI module, 17 tests pass and the Keychain live
  canary is ignored. The refusal regressions prove both normal `refusal.done` handling and
  fail-closed handling when refusal content is followed directly by `response.completed`.
- `cargo test --workspace --locked` was attempted without any OpenAI environment key.
  It remains blocked outside this task in `whisper-rs-sys` generated bindings with an
  existing `whisper_full_params` size assertion overflow, including when rerun with
  `WHISPER_DONT_GENERATE_BINDINGS=1`. Provider and T026 acceptance remain green; this is
  not reported as workspace-test success.

## Review — changes addressed

- Preserve the API's default stream-obfuscation behavior; do not explicitly disable its
  side-channel mitigation for sensitive transcript traffic.
- Normalize `response.failed` and incomplete terminal codes into the same actionable auth,
  rate-limit, context, and request errors as other SDK error paths, with focused tests.
- Reject per-call model ids that differ from the descriptor's configured model so cache identity
  cannot describe model A while the transport sends model B.
- Distinguish JSON-object output from strict JSON-Schema Structured Outputs in backend capability
  metadata. OpenAI currently supports the former; Codex supports neither until `--output-schema`
  is wired. Do not use one ambiguous `StructuredOutput` claim for both.
- The live canary must take an explicitly configured current model rather than hardcode a
  deprecated model id.
- Advance the connector revision when T031 adds strict schema and image semantics, and
  assert the revision in the same-model cross-backend fingerprint regression.
- Never normalize an accumulated refusal as successful merely because a malformed stream
  reaches `response.completed` without a `response.refusal.done` event.

All seven review items are covered by focused regressions and ADR-0008 as amended by
ADR-0010. The SDK adapter is complete, but app and CLI consumer cutover remains explicitly
assigned to T027 and T029 rather than being reported as complete in this task.

The original workspace-CI wording is superseded by the focused no-key acceptance above: the
exact workspace command was attempted and failed before provider tests in generated
`whisper-rs-sys` bindings. T026 stays `in-review` for adapter acceptance, not for an unrelated
ASR toolchain repair. T031, rather than T029, owns extending the text-only completion contract.
