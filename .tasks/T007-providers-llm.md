# T007 — Providers crate: BYOK LLM abstraction with streaming, cancellation, caching

**Status:** todo (unblocked — T001 approved)

**Wave:** 1 — fully parallel

**Depends on:** T001 (`CompletionProvider`, `ProviderError`)

**Owns:** `crates/providers/**`

## Goal

One hand-rolled abstraction over Anthropic / OpenAI / Google / OpenRouter / Ollama.
`AGENTS.md` sizes this at ~500 lines we control and explicitly rules out a framework —
hold that line. Speculative execution means calls are started and aborted constantly,
so **cancellation is a first-class requirement**, not an afterthought.

## Plan

1. Deps in `crates/providers/Cargo.toml`: `reqwest` (rustls, not native-tls, for a
   self-contained binary), `eventsource-stream` or a hand-rolled SSE parser,
   `futures`, `keyring`, `secrecy`. No LLM framework crates.

2. **Request/response types are already frozen in `core`** — `CompletionRequest`,
   `CompletionMessage`, `Delta`, `Usage`, `StopReason`. Do not define your own; normalise
   provider-specific quirks *inward* so callers never branch on provider. Two ambiguities
   T001 left for you to settle and document, because T013 will otherwise guess:

   - **Model identity has two homes.** `CompletionRequest.model` and
     `CompletionProvider::model_id()` can disagree. Decide which wins — recommended: the
     request field is authoritative and `model_id()` reports the provider's configured
     default, used for display and for the role registry. Say so in the crate docs.
   - **Cancellation has two representations.** `ProviderError::Cancelled` and
     `StopReason::Aborted` both exist. Define when each appears: an aborted speculative
     call that produced no output should end the stream with `Err(Cancelled)`, while one
     that already streamed text should emit a final `Delta` with `stop_reason: Aborted`
     and whatever `Usage` was incurred — T013 needs that usage to report the
     wasted-token rate, so it cannot simply vanish on abort.

   `cache_boundary` on `CompletionMessage` is the portable cache breakpoint: everything
   through the marked message is static. Map it to Anthropic `cache_control`; ignore it
   where the provider has no caching. At most one per request — validate and reject
   rather than silently using the last one.

3. **Per-provider adapters**, each its own module:
   - `anthropic` — Messages API, SSE. Support `cache_control` breakpoints; the static
     context prefix (battlecards, company docs, call metadata) is re-sent on every
     watcher call, so prompt caching is the difference between viable and expensive.
     See the `claude-api` skill for current model IDs, pricing, and cache semantics
     rather than hardcoding remembered values.
   - `openai` — Chat Completions (or Responses), SSE.
   - `google` — Gemini `streamGenerateContent`.
   - `openrouter` — OpenAI-compatible, different base URL and headers.
   - `ollama` — local, no key, NDJSON stream not SSE. This is the fully-local story;
     make sure it works without any key configured at all.

4. **Cancellation.** Every call returns a stream plus an abort handle; dropping the
   stream must actually terminate the in-flight HTTP request, not leak a task that
   keeps billing the user. Test this explicitly — spawn, abort mid-stream, assert the
   connection closed. Speculative execution fires and kills these constantly; a leak
   here is a real cost to the user's own API budget.

5. **Key storage** via `keyring` → macOS Keychain. Keys are `secrecy::SecretString`,
   never logged, never in `Debug` output, never written to disk by us. Provide
   `store_key`, `load_key`, `delete_key`, `has_key` per provider. Add a test asserting
   the `Debug` impl of a configured provider does not contain the key — this is the
   kind of leak that ships silently.

6. **Two-tier support.** The watcher (cheap/fast, called on every partial) and the
   suggester (larger, called on trigger) may be different providers entirely. The
   registry must hold multiple configured providers simultaneously and resolve by role,
   not be a single global "current provider".

7. **Errors and resilience.** Distinguish auth failure, rate limit (surface
   `retry-after`), context-length, network, and provider-side errors — the UI must tell
   the user *their key* is bad versus *the network* is down. Retry only idempotent
   failures, with backoff, and never retry a call the pipeline has already cancelled.

8. **Testing without keys.** Mock SSE server (`wiremock` or a local hyper server)
   covering: normal stream, mid-stream error, malformed SSE, slow stream + abort,
   rate-limit response. CI must pass with no API keys present. Add an ignored
   integration test per provider for manual live runs.

## Contract for downstream tasks

`providers::Registry::get(Role::Watcher | Role::Suggester) -> Arc<dyn CompletionProvider>`.
T012's settings UI and T013's watcher loop both build on this.

## Acceptance

- All five providers stream text end to end against mocks; at least Anthropic and
  Ollama verified live.
- Abort mid-stream terminates the request, verified by the mock server.
- Keys never appear in logs or `Debug` output (test-asserted).
- Full test suite passes with zero API keys configured.
- Crate stays close to the ~500-line budget; justify overruns in the notes.

## Out of scope

Prompt content and assembly (T013), RAG retrieval (T008), settings UI (T012),
tool/function calling (revisit when MCP lands in Phase 4).
