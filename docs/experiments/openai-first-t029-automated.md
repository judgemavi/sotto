# T029 automated OpenAI-first reasoning evidence

Date: 2026-08-11

## Scope and verdict

This checkpoint covers only the credential-free, automatable T029 slice. The CLI now selects
`none`, direct OpenAI Responses, or the deliberately unavailable Codex connector through the
provider registry. It no longer constructs the legacy five-provider adapter, defaults to Ollama,
or accepts an eager OCR screen-context mode. Summary and topical clustering use the same pinned
`ResolvedBackend`; `none` exits normally without opening the database or credential store.

The synthetic evidence supports an **OpenAI-first connector seam**, not a production or latency
acceptance verdict. T030 recorded FAIL, so Codex remains unavailable and was not invoked.

## Credential-free automated evidence

The CLI-owned mock Responses integration verifies:

- valid summary citations and clustering event references;
- byte-identical timeline state before and after derived reasoning;
- cache reuse only for the same backend fingerprint, plus a fake third connector through the
  unchanged consumer path;
- transcript timestamps on the first request with OCR, frame paths, and image inputs absent;
- explicit image-consent denial and a single authorized, provenance-bearing image second pass;
- strict JSON Schema request shape through the capability-bearing OpenAI connector;
- pre-transport rejection when a structured consumer's backend omits `JsonObjectOutput`;
- honest rejection when a resolved text backend requests image inspection without `ImageInput`;
- normalized rate-limit and pre-text cancellation results;
- deterministic p50/p95 aggregation over fixed observations with distinct TTFT/total counts;
- an ignored live canary that is the only CLI test allowed to read the OpenAI Keychain identity.

Focused commands:

```text
CARGO_TARGET_DIR=/tmp/sotto-t029-target WHISPER_DONT_GENERATE_BINDINGS=1 cargo check -p cli --locked
PASS

CARGO_TARGET_DIR=/tmp/sotto-t029-target WHISPER_DONT_GENERATE_BINDINGS=1 cargo test -p cli --test reasoning_integration --locked -- --nocapture
PASS: 6 passed, 0 failed, 1 ignored (live T035/Keychain evidence canary)

CARGO_TARGET_DIR=/tmp/sotto-t029-target WHISPER_DONT_GENERATE_BINDINGS=1 cargo clippy -p cli --all-targets --locked -- -D warnings
PASS

cargo fmt --all --check
PASS
```

The loopback mock test needed execution outside the managed network sandbox because sandboxed
binding returned `Operation not permitted`; it remained credential-free and addressed only a
local ephemeral listener.

## Deferred gates

The ignored canary was not run. It fails closed unless the caller supplies the exact T035 owner
acknowledgement, a new evidence-file path, and at least five samples. It then runs both summary and
clustering, validates every referenced event, proves the timeline byte-identical, measures separate
startup/TTFT/total populations, and writes their p50/p95 plus outputs to that file. T035 must first
accept a real persisted T032 session, after which the owner must run this Keychain-backed gate and
review result quality and cited moments. Empty summary/cluster evidence is rejected, and clustering
must be freshly generated rather than served from the derived-view cache. The existing local Apple
Vision fixture test also fails independently with `Foundation._GenericObjCError.nilError`; that is
not evidence about the T029 Responses path. No Codex timing or live call is planned after T030
FAIL. T027's persistence and settings semantics are app-only and introduce no CLI coupling.
