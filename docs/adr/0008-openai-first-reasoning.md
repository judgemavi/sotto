# ADR-0008: OpenAI-first reasoning backends

- Status: Accepted
- Date: 2026-08-11
- Decision owners: Sotto maintainers

## Context

The original provider plan and T007 implemented five hand-written HTTP adapters. That
made API keys the main cloud onboarding path and made Sotto responsible for several
streaming protocols. The evaluated product focus was narrower: determine whether a
user-installed Codex CLI could use its existing ChatGPT login safely, while retaining direct and
predictable OpenAI API access for a user who provides a key. T030 subsequently failed the required
Codex no-tools isolation gate. ADR-0014 later added a separately acknowledged experimental Codex
subscription path without reclassifying that failed result; direct OpenAI remains the supported
API path.
No reasoning remains a complete, ordinary configuration.

These two connectors have different transports and authentication owners but must look
the same to summaries, clustering, and the future advisor. They also must not share a
derived-view artifact merely because they report the same model name.

## Decision

Sotto exposes no reasoning, an explicit experimental Codex subscription mode, and direct OpenAI API
as selectable product states. Codex is off by default and selectable only after the user accepts the
T030 residual-risk disclosure and the supported login probe reports ChatGPT authentication.
Anthropic, Google, OpenRouter, Ollama, and other providers are deferred.
Their historical code does not constitute a v1 product promise.

`core::CompletionProvider` remains the headless streaming primitive. `core` does not
gain backend ids, SDK types, subprocess types, credentials, or readiness state. The
`providers::backend` module adds the product-layer contract:

- an open-ended persisted `BackendId`, with `openai.codex-cli` and
  `openai.responses` as the first two ids;
- a descriptor containing display/model identity, capabilities, authentication kind
  and current status;
- a connector-and-model-aware cache fingerprint with an explicit connector revision;
- independent watcher, suggester, and summarizer selection;
- resolution that pins an immutable descriptor and `Arc<dyn CompletionProvider>` for
  the lifetime of a call.

The common capability baseline is streaming plus cancellation. Output capability is
explicit rather than collapsed into one "structured" flag: `JsonObjectOutput` means the
connector can request syntactically valid JSON, while `JsonSchemaOutput` means a
caller-supplied schema is actually enforced. The direct OpenAI connector advertises JSON-object,
caller-constrained JSON-Schema, and image input only because T031 added Sotto-owned
request/authorization seams and request-shape tests, and T034 binds those claims to the registered
runtime implementation. Codex's default, fail-closed descriptor advertises none of those advanced
capabilities. The separately acknowledged ADR-0014 experimental descriptor advertises only
`JsonObjectOutput`, backed by the CLI's output-schema flag and captured invocation tests; it still
does not advertise caller-constrained JSON Schema or image input. SDK or CLI support by itself is
not a product capability: Sotto-owned request seams and normalization tests are required before
metadata may be advertised.

Changing a role affects only later resolutions. Refreshing Codex login or API-key
readiness changes later resolutions without replacing the provider, changing the cache
fingerprint, or retargeting an in-flight call. A role with no selection resolves to
`None`; a selected but unavailable backend is an actionable error.

Connector construction deliberately remains inside the connector modules in T025 and
T026. Each module constructs its provider and descriptor, then registers them through
the open registry. We do not add a common `BackendFactory` yet: CLI executable/login
probing and Keychain/API client construction have materially different asynchronous
inputs, and a shared configuration enum would recreate the closed vendor switch this
ADR removes. If repeated construction behavior emerges, a later factory may return the
same descriptor plus `Arc<dyn CompletionProvider>` without changing consumers.

### Codex CLI

Codex is a user-installed optional runtime, not a bundled or required sidecar. The map
tier and the OpenAI API path do not depend on it. Sotto invokes supported machine-readable
CLI behavior, lets Codex own login and token refresh, and never reads, copies, or watches
Codex credential files. T025 begins with stable `codex exec --json`; depending on the
experimental App Server protocol requires a measured result and an ADR amendment.

The CLI must run in an isolated empty working directory with inherited tools,
instructions, hooks, MCP servers, persistence, and filesystem/shell authority disabled
where the supported CLI contract allows it. Transcript and screen text are untrusted
input. If isolation cannot be demonstrated, the connector is blocked rather than made
safe by prompt wording.

T025 is currently blocked: the installed CLI does not yet provide proof that the model
sees no execution tools. The connector's fail-closed parsing, stdin cancellation,
terminal-event validation, probe cleanup, stderr draining, request-control handling,
and capability reporting have been hardened and independently re-reviewed, but those
deterministic safeguards do not establish the real model-visible tool inventory. A
successful ChatGPT login alone must not make the connector selectable.

T030 has since returned **FAIL** against Codex CLI 0.147.0. Official CLI/configuration
surfaces and the installed App Server schema provide sandbox/approval controls,
per-feature disables, and additive dynamic tools, but no supported field that guarantees
an empty built-in model-visible tool inventory. The default Codex descriptor therefore remains
unavailable. ADR-0014 permits an explicit experimental descriptor to propagate authenticated
readiness while preserving the warning and all connector hardening; direct OpenAI Responses
remains the supported BYOK reasoning path.
The detailed evidence and reconsideration condition are recorded in
`docs/experiments/codex-isolation-protocol.md`.

### Direct OpenAI API SDK

T026 pins this dependency exactly:

```toml
async-openai = { version = "=0.41.3", default-features = false, features = ["responses", "rustls"] }
```

`async-openai` was selected because the inspected 0.41.3 release has a typed Responses
API, parsed streaming response events, JSON Schema output types, image inputs,
function tools, and response usage. Its feature flags let Sotto compile the Responses
surface without the full API set, and it shares the workspace's Tokio, reqwest 0.13,
Serde, secrecy, and rustls direction. The SDK is moderately sized rather than minimal:
the Responses feature also brings its HTTP/SSE, builder, Tower, and support dependencies.
That cost replaces Sotto's hand-written OpenAI wire protocol and is acceptable for the
single direct provider in v1.

The official OpenAI SDK list does not currently include Rust, so this community SDK is
not allowed across the Sotto boundary. All its request, event, error, image, tool, and
client types stay in `crates/providers/src/openai/`; consumers receive only Sotto types.
T026 proves with loopback tests that dropping or cancelling a stream closes the
underlying request, and normalizes refusals, terminal failures, incomplete responses,
usage, and malformed SDK events. It preserves OpenAI's default stream-obfuscation
behavior. The current connector uses JSON-object mode; the stronger JSON-Schema SDK
surface remains isolated until Sotto owns a provider-neutral schema request contract.

`genai` was not selected for the direct adapter because multi-provider uniformity is no
longer the v1 goal and its extra abstraction would sit above the Responses features we
need to normalize explicitly. Rig and broader agent/RAG frameworks own substantially
more orchestration than this boundary needs. Raw reqwest was rejected because it would
continue the hand-written SSE/protocol burden this decision is intended to remove.

## Consequences

- The hardened Codex connector remains unavailable by default after T030 FAIL. ADR-0014 permits an
  off-by-default experimental selection with explicit user acknowledgement; only a future official
  empty-built-in-tools contract may remove that warning.
- Direct OpenAI remains optional BYOK, with the key stored in the OS Keychain.
- The selectable OpenAI path sends explicitly disclosed, redacted text to OpenAI and requires a
  network connection; it is not the offline map tier.
- Cache users must persist `BackendFingerprint`, not `model_id` alone. Existing insight
  code is not switched to the new connectors until its integration path consumes that
  fingerprint; T029 proves the separation end to end.
- Capability metadata records only behavior exposed through Sotto's current connector contract.
  OpenAI's T031-proven JSON-Schema and authorized-image shapes are advertised; tool calling is not.
  Transcript-only context remains the default and screen inspection remains explicit.
- T026 completes the adapter and its mock transport contract. T027 owns settings/app
  selection and T029 owns consumer integration/evaluation; adapter completion does not
  by itself make this the active product path.
- New connectors can register a new stable string id and implement
  `CompletionProvider` without edits to `core`, insight algorithms, or a vendor enum.
- The shipped application remains one Rust binary. A user's optional Codex executable
  is an external integration, not a bundled helper or a hidden requirement.

## Supersedes

This ADR supersedes the AGENTS.md and T007 direction that made five hand-written
Anthropic/OpenAI/Google/OpenRouter/Ollama adapters the product-facing provider layer.
T007 remains an accurate historical record of the implementation and review completed
under the old decision.

## References

- [Codex authentication](https://developers.openai.com/codex/auth)
- [Codex CLI reference](https://developers.openai.com/codex/cli/reference)
- [Codex App Server](https://developers.openai.com/codex/app-server)
- [async-openai 0.41.3](https://crates.io/crates/async-openai/0.41.3)
