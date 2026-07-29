# T007 — Providers crate: BYOK LLM abstraction with streaming, cancellation, caching

**Status:** done (approved at review round 2)

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
   suggester (larger, called on trigger) may be different providers entirely, as may the
   post-call summarizer (T017). The
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

`providers::Registry::get(Role) -> Arc<dyn CompletionProvider>` with roles Watcher,
Suggester and Summarizer — T017 needs a third role and it is cheaper to have it now.
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
tool/function calling (revisit when MCP lands in Phase 5).

## Notes

- Built provider-neutral streaming adapters for Anthropic, OpenAI, Gemini, OpenRouter,
  and keyless local Ollama. The request model is authoritative; the configured model
  remains the display/registry default. Anthropic alone maps the portable cache
  boundary, and multiple boundaries fail before network I/O.
- Added explicit cancellable calls plus drop-safe HTTP streams. Cancellation before
  output yields `ProviderError::Cancelled`; cancellation after output yields a final
  `StopReason::Aborted`. A loopback HTTP test proves abort closes the connection.
- Added a three-role registry, macOS Keychain-backed secret operations, redacted
  provider `Debug`, status/error normalization, and provider-specific usage/stop
  normalization. Streaming completions are not automatically retried because they
  are not provably idempotent once accepted upstream; notably, cancellation is never
  retried.
- All five adapters pass transport-level loopback mock streaming. Malformed payloads,
  cache validation, secret redaction, independent role resolution, and cancellation
  semantics are covered without API keys. Five ignored live checks accept model IDs
  through `SOTTO_<PROVIDER>_MODEL` and load keys from the OS keychain.
- The implementation is about 500 production lines excluding protocol fixtures,
  cancellation/mock tests, and the five tiny provider constructor modules. The
  overage in total source is test coverage for five incompatible wire formats rather
  than framework/production abstraction weight.
- `cargo test -p providers` and
  `cargo clippy -p providers --all-targets -- -D warnings` pass. Live Anthropic and
  Ollama verification was not run: it requires user credentials in Keychain and a
  running local Ollama model respectively. Run the relevant ignored test manually
  after setting its model environment variable.

## Review round 1 — changes requested

Five adapters, mock coverage, redacted `Debug`, role registry, and the two ambiguities
from the T001 review all resolved and documented in the crate docs. Verified: 9 passed,
5 live tests correctly ignored, strict clippy clean. Three things to fix.

### R1. Cancellation is unreachable through the trait — the interface T013 uses

`CompletionCall` / `CancellationHandle` work, and the abort test proves it. But the
`CompletionProvider::stream()` impl is:

```rust
Box::pin(async move { self.start(req)?.future.await })
```

`self.start(req)?.future` moves `future` out of the temporary `CompletionCall`; the
`handle` (`watch::Sender<bool>`) is dropped when that statement ends — before the caller
ever polls the returned stream. `wait_cancelled` then hits
`cancelled.changed().await.is_err()` and parks on `pending()` forever, so the cancellation
arm of the `select!` can never fire. The trait also exposes no handle to abort with.

T013 holds providers as `Arc<dyn CompletionProvider>` — that was the point of the R1
dyn-compatibility round. So through the only interface it has, speculative execution
cannot abort, and the `StopReason::Aborted` + `Usage` path that the earlier review
specifically preserved for the wasted-token rate is unreachable. Dropping the stream
kills the request but yields no final delta and no usage.

Fix: put cancellation in the trait contract. Either have `stream()` accept a cancellation
token, or return a type that carries its own handle. Whichever you choose, add a test
that cancels **through `Arc<dyn CompletionProvider>`** and asserts the `Aborted` delta —
the current test exercises the concrete path only, which is why this passed.

### R2. The Google API key travels in the URL query string

`Transport::url` builds `…:streamGenerateContent?alt=sse&key={key}`. Query strings land
in proxy logs, server access logs, `tracing` spans if anyone ever logs a request URL, and
crash reports — and `tracing` is already a workspace dependency. This defeats the "keys
never logged" requirement no matter how careful the rest of the crate is.

Use the `x-goog-api-key` header instead; Gemini supports it. While you are there:
`Provider::start` copies the key out of `SecretString` into a plain `String`
(`expose_secret().to_owned()`) that lives for the whole call and is not zeroized, and
`authenticate` re-wraps that plain `&str` in a fresh `SecretString`, which protects
nothing. Carry `SecretString` end to end.

### R3. Keychain failures are reported as network errors

`entry()`, `store_key`, `load_key` and `delete_key` all map `keyring::Error` to
`ProviderError::Network(..)`. A locked keychain or a denied access prompt is not a network
failure, and T012's requirement is precisely to tell the user *your key is bad* apart from
*the network is down*. This silently defeats the taxonomy the T001 review round existed to
create. `ProviderError::Auth` is the closer fit; if keychain access deserves its own
variant, come back and say so rather than widening `Network`.

Also minor, fix while you are in there:
- `ContextLengthExceeded { limit: 0, requested: 0 }` fabricates zeros the UI would
  display. Parse the real numbers or make the fields `Option<u32>` and report back.
- `validate()` returns `ProviderError::Decode` for an invalid *request*; `Decode` is for
  malformed provider *responses*.

## Re-review

R1–R3 fixed, cancellation tested through `Arc<dyn CompletionProvider>`, no secret in any
URL, and the live-test instructions unchanged.

## Review round 1 implementation notes

- Cancellation now enters through the dyn-compatible core trait using a caller-retained
  `CancellationToken`. The mid-stream test invokes `stream` through
  `Arc<dyn CompletionProvider>`, observes the final `Aborted` delta, and verifies that
  the server sees the HTTP connection close. Any latest upstream usage is attached to
  that final delta.
- Provider keys remain `SecretString` from configuration through request
  authentication. Gemini sends its key only in `x-goog-api-key`; a transport test
  asserts that the request target contains no secret.
- Keychain failures map to `CredentialStore`, invalid cache-boundary requests map to
  `InvalidRequest`, and unknown context limits are represented as `None` rather than
  fabricated zeros.
- `cargo test -p providers` passes with 10 mock/unit tests and the same 5 ignored live
  checks. `cargo clippy -p providers --all-targets -- -D warnings` is clean.
