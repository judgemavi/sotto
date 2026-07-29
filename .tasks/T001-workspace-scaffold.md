# T001 — Cargo workspace scaffold, frozen domain contracts, ADR skeleton

**Status:** done (approved at review round 2)

**Wave:** 0 (blocks all other tasks — nothing else starts until this is merged)

**Depends on:** nothing

**Owns:** `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`, `clippy.toml`,
`crates/**/Cargo.toml` (stubs only), `crates/core/**`, `docs/adr/**`

## Why this is one task and not nine

Every other task in wave 1 writes into exactly one crate directory. That only works if
the workspace membership list, the shared dependency floor, and the cross-crate types
already exist and never move again. This task produces all of that in one commit so the
nine parallel tasks never contend on a shared file.

## Plan

1. **Workspace root.** `Cargo.toml` with `[workspace] resolver = "3"` and `members`
   listing *all* crates from the `AGENTS.md` repo structure, even the ones that will be
   empty stubs for weeks:
   `core`, `capture`, `asr`, `vad`, `prosody`, `providers`, `rag`, `mcp`, `app`, `cli`.
   Pin `rust-toolchain.toml` to the stable channel currently installed (1.97.1) with
   `components = ["clippy", "rustfmt"]`.

2. **Stub every member.** For each crate: `crates/<name>/Cargo.toml` +
   `crates/<name>/src/lib.rs` containing only `#![deny(warnings)]` and a doc comment
   naming the task that will fill it in. `cli` and `app` get `src/main.rs` instead.
   `cargo build --workspace` must pass on an empty workspace before you go further.

3. **Dependency policy.** Put *only* genuinely universal deps in
   `[workspace.dependencies]`: `tokio`, `thiserror`, `anyhow`, `tracing`,
   `tracing-subscriber`, `serde`, `serde_json`. Everything crate-specific
   (`whisper-rs`, `ort`, `rusqlite`, `reqwest`, `gpui`, `rmcp`, `keyring`,
   `fastembed`, `cpal`) is declared by the owning crate in wave 1. Write this policy as
   a comment at the top of the root `Cargo.toml` so implementers see it.

4. **Freeze the domain types** in `crates/core/src/types.rs`. These are the wire format
   between every pipeline stage and are the single highest-leverage artifact of this
   task — get them right, because changing them later serialises nine agents.

   - `enum Source { Mic, System }` — mic is the rep, system audio is the customer.
   - `struct AudioFrame { source: Source, samples: Arc<[f32]>, sample_rate: u32, seq: u64, capture_ts: Instant, stream_offset: Duration }`.
     `stream_offset` is monotonic from stream start and is what downstream stages align
     on; `capture_ts` is wall-clock for latency measurement only.
   - `struct VadSegment { source: Source, start: Duration, end: Option<Duration>, kind: SpeechState }`,
     `enum SpeechState { SpeechStart, SpeechEnd }`.
   - `struct Utterance { source, start, end, text, is_final: bool, avg_logprob: f32, annotations: Vec<Annotation> }` —
     the sliding-window ASR emits many non-final `Utterance`s per final one; downstream
     stages must key on `(source, start)` to supersede a previous partial.
   - `enum Annotation { Pause(Duration), Interruption { by: Source }, SpeechRate(f32), TalkTimeRatio(f32), Hesitant, Emphatic }`
     plus `fn render_inline(&self) -> String` producing the
     `[customer, hesitant, 2.5s pause]` form from `AGENTS.md`.
   - `enum TriggerKind { CompetitorMention, PricingQuestion, Objection, DiscoveryGap }`,
     `struct Trigger { kind, utterance_span, confidence: f32 }`.
   - `struct Suggestion { trigger: Trigger, text: String, citations: Vec<Citation>, is_final: bool }`.
   - `enum PipelineEvent { Audio(AudioFrame), Vad(VadSegment), Transcript(Utterance), Trigger(Trigger), Suggestion(Suggestion), Error(PipelineError) }`.

   All types `Clone + Send + Sync + 'static` (broadcast channels clone per subscriber —
   keep payloads cheap, hence `Arc<[f32]>` for samples). `serde` derives behind a
   `serde` feature so the CLI can dump events as JSONL without forcing serde on the
   hot path.

5. **Freeze the stage traits** in `crates/core/src/traits.rs`. Each wave-1 crate
   implements exactly one of these, so their shapes are a hard contract:

   - `trait CaptureBackend: Send { fn start(&mut self, sink: broadcast::Sender<AudioFrame>) -> Result<(), CaptureError>; fn stop(&mut self); fn permission_status(&self) -> PermissionStatus; }`
   - `trait VoiceActivityDetector: Send { fn push(&mut self, frame: &AudioFrame) -> Option<VadSegment>; fn reset(&mut self); }`
   - `trait Transcriber: Send { fn push(&mut self, frame: &AudioFrame); fn poll(&mut self) -> Vec<Utterance>; }`
   - `trait Retriever: Send + Sync { async fn search(&self, query: &str, k: usize) -> Result<Vec<Chunk>, RagError>; }`
   - `trait CompletionProvider: Send + Sync { async fn stream(&self, req: CompletionRequest) -> Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>; fn model_id(&self) -> &str; }`

   No `async-trait` crate. **Every trait must be dyn-compatible** — see R1 in the review
   section: native `async fn` in a trait is not, and downstream tasks hold these behind
   `Arc<dyn _>`. Async methods return an explicit `BoxFuture`.

6. **Event bus** in `crates/core/src/bus.rs`: thin wrapper over
   `tokio::sync::broadcast` with a per-stage `subscribe()` and lag handling that logs
   and counts drops rather than panicking. A slow UI subscriber must never stall the
   audio path — document that invariant in the module doc.

7. **Error types** in `crates/core/src/error.rs` — one `thiserror` enum per stage
   (`CaptureError`, `VadError`, `AsrError`, `ProviderError`, `RagError`) plus a
   `PipelineError` that wraps them. No `unwrap()`/`expect()` outside tests per
   `AGENTS.md`.

8. **Lint gate.** `#![deny(warnings)]` in every crate root, `clippy.toml`, and a
   workspace `[lints]` table. Confirm `cargo clippy --workspace --all-targets -- -D warnings`
   is clean on the stubs.

9. **ADR skeleton.** `docs/adr/0000-template.md` and
   `docs/adr/0001-pure-rust-gpui-stack.md` recording the already-made decision from
   `AGENTS.md` (context / decision / consequences / revisit-if). Later ADRs are written
   by the tasks that deviate.

## Contract for downstream tasks

After this lands, the following are **frozen**: root `Cargo.toml`, `crates/core/src/types.rs`,
`crates/core/src/traits.rs`. A wave-1 agent that believes it needs a change here must
stop and report the specific type and reason instead of editing.

## Acceptance

- `cargo build --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo test -p core` passes, including a round-trip test for `Annotation::render_inline`.
- Every crate in the `AGENTS.md` structure exists as a member and compiles.
- No crate other than `core` contains logic.

## Out of scope

Any real pipeline logic, audio, or UI. This task ships an empty but complete skeleton.

## Notes

- Created all ten workspace members and verified both the default workspace build and
  the all-feature lint/test path on Rust 1.97.1.
- Used the latest crates.io releases available on 2026-07-29. `futures-core` is a
  core-only dependency needed to express the frozen streaming provider contract; it
  is intentionally not a workspace dependency.
- Added the supporting shared values referenced by the frozen traits (`PermissionStatus`,
  `Chunk`, `CompletionRequest`, `Delta`) plus explicit citation, message, and utterance
  span types so downstream crates do not invent competing wire shapes.
- The package remains named `core` for `cargo test -p core`, while its library target
  is `sotto_core`. Naming the library itself `core` shadows Rust's standard `core`
  crate in rustdoc/proc-macro expansion and breaks doctests.
- `AudioFrame.capture_ts` is deliberately skipped by the optional serde wire format
  because `Instant` is process-local. Deserialization replaces it with `Instant::now`;
  cross-stage alignment remains based only on `stream_offset`.
- No architecture decision deviated from `AGENTS.md`.
- Review round 1: made `Retriever` and `CompletionProvider` dyn-compatible with an
  explicit `BoxFuture`, and added the requested compile-time guard.
- Review round 1: expanded completion requests/deltas for prompt caching, usage, and
  stop reasons; replaced generic stage messages with the reviewed typed error variants.
  `ProviderError::Cancelled` is documented as non-retryable/non-user-facing and has a
  serde round-trip test through `PipelineError`.
- Review round 1: documented the capture-to-pipeline bridge and canonical inline
  rendering ownership boundaries.

## Review round 1 — changes requested

Scaffold, workspace layout, lint table and ADRs are accepted. The lint table is
intentionally strict and stays exactly as written — its cost to implementers is now
documented as the house test idiom in `.tasks/README.md`, not softened here.

Two changes are required before wave 1 unblocks. Both are in the contracts this task
**freezes**, which is why they are worth another round rather than a follow-up: the
whole premise of blocking nine agents on T001 is that these shapes do not move again.

### R1. Make `CompletionProvider` and `Retriever` dyn-compatible

`&dyn CompletionProvider` currently fails with `E0038`. The notes resolve this by
declaring that "provider selection uses a concrete enum" — but that closes the provider
set at compile time and contradicts three downstream contracts already written:
T007 exposes `Registry::get(Role) -> Arc<dyn CompletionProvider>`, T012 makes
provider and model selection a runtime user setting, and T013 holds watcher and
suggester providers that may be different vendors entirely.

The performance argument does not hold at this boundary: `stream()` already returns a
`BoxStream`, so native `async fn` saves exactly one `Box::pin` per remote HTTP call to
an LLM. Retrieval is the same story against a SQLite query.

- Add `pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;`
  alongside the existing `BoxStream`.
- `fn search<'a>(&'a self, query: &'a str, k: usize) -> BoxFuture<'a, Result<Vec<Chunk>, RagError>>;`
- `fn stream(&self, req: CompletionRequest) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>;`
- Delete both `#[expect(async_fn_in_trait, ...)]` blocks and the doc comments claiming
  non-object-safety is intentional.
- Add a compile-time guard so this cannot silently regress:
  `fn _assert_dyn_compatible(_: &dyn CompletionProvider, _: &dyn Retriever) {}`

### R2. Give the frozen types enough shape to carry wave 1

Three acceptance criteria in downstream tasks are unreachable against the current types.
Because `types.rs` and `error.rs` are frozen, they must be fixed here.

**`CompletionRequest` cannot express a cached prompt** (T013 acceptance: "prompt caching
verified working — cached-token counts reported"). Add `model: String`,
`system: Option<String>`, `stop: Vec<String>`, and a cache breakpoint. Represent the
breakpoint as `cache_boundary: bool` on `CompletionMessage`, meaning *everything up to
and including this message is static and may be cached*; at most one per request, and
providers without caching ignore it. This keeps the static/dynamic split that T013's
prompt assembly must align with visible in the type itself.

**`Delta` carries no usage** (T013 acceptance: "expose the wasted-token rate" — the user
is paying for speculative calls we abort). Add `usage: Option<Usage>` and
`stop_reason: Option<StopReason>`, both populated on the final delta:
- `struct Usage { input_tokens: u32, output_tokens: u32, cache_read_tokens: u32, cache_write_tokens: u32 }`
- `enum StopReason { EndTurn, MaxTokens, StopSequence, Aborted }` — `Aborted` is what
  speculation cancellation reports.

**Every stage error is `Message(String)`** via the `stage_error!` macro, which defeats
the module's own doc comment ("typed errors crossing stage boundaries"). T012 must tell
the user *your key is bad* apart from *the network is down*; T011 must react to revoked
capture permission specifically; T007 must know what is worth retrying. Replace the
macro with real variants, keeping `Clone + Eq` so they still ride the broadcast bus:

- `CaptureError`: `PermissionDenied { status: PermissionStatus }`, `PermissionRevoked`,
  `DeviceUnavailable { device: String }`, `StreamFailed(String)`, `Unsupported(String)`
- `ProviderError`: `Auth`, `RateLimit { retry_after: Option<Duration> }`,
  `ContextLengthExceeded { limit: u32, requested: u32 }`, `Network(String)`,
  `Upstream { status: u16, message: String }`, `Cancelled`, `Decode(String)`
- `AsrError`: `ModelNotFound { path: String }`, `ModelLoad(String)`, `Inference(String)`
- `VadError`: `ModelLoad(String)`, `Inference(String)`
- `RagError`: `Storage(String)`, `Embedding(String)`, `Migration { from: u32, to: u32 }`,
  `NotFound { id: String }`

`ProviderError::Cancelled` is load-bearing for T013 — an aborted speculative call must
be distinguishable from a real failure so it is never retried and never surfaced.

### R3. Document two boundaries that are already correct

No code change, just module docs, so wave 1 does not have to infer them:

- `CaptureBackend` takes a `broadcast::Sender<AudioFrame>` rather than the
  `PipelineEvent` bus. This is the right call — audio frames at ~100/s across two
  streams should not share a channel with UI-facing events — but it means T011 must run
  a bridge task lifting `AudioFrame` into `PipelineEvent::Audio`. Say so in `bus.rs`.
- `Utterance::render_inline` is the canonical `[customer, hesitant, 2.5s pause] "…"`
  renderer and lives in core. T006 has been rescoped to own annotation *selection* and
  the token budget, not a competing renderer. Note that ownership split in `types.rs`.

### Re-review

Same acceptance criteria as above, plus: `cargo clippy --workspace --all-targets
--all-features -- -D warnings` clean, the dyn-compatibility guard present, and a test
asserting `ProviderError::Cancelled` survives a `PipelineError` round trip.
