# T027 — Reasoning settings and live backend switching

**Status:** done

**Wave:** R2

**Depends on:** T032's explicit `settings/**` handoff; T025; T026; T030's recorded
Codex isolation verdict (PASS, FAIL, or INCONCLUSIVE); T034 reasoning runtime/credential seam

**Owns:** `crates/app/src/settings/**` after T032 hands the directory off;
`crates/app/src/reasoning/**`; `crates/app/src/main.rs` for construction/injection of the one shared
reasoning controller after T032 hands it off; `crates/app/src/lib.rs` only for minimal module
registration after T016/T032 hand it off; `crates/app/Cargo.toml` for app-local identifier-only
settings serialization dependencies (with planner-owned sequential `Cargo.lock` reconciliation)

## Goal

Make the three honest product states easy to understand and switch between:
`No reasoning`, `Codex subscription`, and `OpenAI API`.

## Plan

1. Lead cloud onboarding with Codex only after a T030 PASS. Show install, sign-in, ready,
   and unavailable states; never show an API-key field for the Codex path. After FAIL or
   INCONCLUSIVE, keep Codex visibly unavailable and lead with direct OpenAI instead.
2. Offer OpenAI API as an optional write-only Keychain credential path, with bad key,
   credential-store, network, and rate-limit failures kept distinct.
3. Keep no reasoning first-class: capture, transcription, board, and review do not show
   an error or dead panel when no backend is selected.
4. Provide a simple apply-to-all-roles choice with optional watcher/suggester/summarizer
   overrides using the existing registry roles.
5. Persist only backend ids, model ids, and role mapping. Never persist API keys, Codex
   tokens, prompts, or CLI credential paths.
6. Resolve and pin a backend when a call starts. Switching affects new work without
   restarting Sotto; existing work remains attached to its original cancellable backend.
7. State privacy and connectivity honestly: Codex subscription reasoning and OpenAI API
   reasoning both send redacted text to OpenAI; neither is local/offline.
8. Remove the legacy five-provider settings/default path. The v1 UI exposes only no
   reasoning, Codex subscription readiness, and OpenAI Responses API; historical adapters
   are not selectable product capabilities.
9. Keep map-tier lifecycle state owned by T032 intact. Reasoning readiness may decorate
   settings, but it must not gate Start, transcription, the board, or post-call review.

## Contract for downstream tasks

T029 drives both reasoning modes through these persisted selections. T013 receives a
resolved provider per call and does not inspect UI settings.

## Acceptance

- Switching between no reasoning, Codex, and OpenAI requires no application restart.
- New calls use the new backend while an in-flight call remains pinned and cancellable.
- Codex never requests or stores an API key.
- Only backend/model/role identifiers persist.
- Missing/expired Codex login, bad API key, keychain failure, network loss, and rate
  limit render different actionable states.
- No-reasoning mode remains complete and calm.
- No app settings or default runtime path constructs the legacy hand-written OpenAI Chat
  Completions adapter or advertises Anthropic, Google, OpenRouter, or Ollama.

## Out of scope

Provider implementation, adding other vendors, prompt content, and model benchmarking.

## Implementation evidence (2026-08-11)

- The application constructs one shared `ReasoningController` beside the existing
  `SessionController`. It owns the open provider registry and returns call-pinned
  `ResolvedBackend` values; capture start/stop, provisioning, timeline ingress, and the board do
  not inspect reasoning readiness and retain T032's lifecycle unchanged.
- Cold settings use no reasoning for watcher, suggester, and summarizer. Apply-to-all and
  per-role overrides persist open backend ids, one OpenAI model id, and role mappings only in
  `~/Library/Application Support/Sotto/reasoning-settings.json`. Writes sync a same-directory
  temporary file and atomically rename it. Unsupported or Codex selections fail closed to no
  reasoning.
- Codex is registered without executing or probing the CLI and is visibly disabled with T030's
  FAIL reason. It has no key input and cannot be selected. The product registry contains only the
  Codex descriptor and the real advanced OpenAI Responses provider; app settings contain no
  `ProviderKind`, legacy `Provider`, or Anthropic/Google/OpenRouter/Ollama construction path.
- OpenAI API keys cross an injectable app credential-store trait whose production implementation
  delegates to T034's OpenAI-specific Keychain functions. The UI immediately clears its masked,
  write-only input. Bad key, missing key, Keychain failure, network loss, rate limit, and generic
  validation failure remain distinct. Credential-flow tests use only an in-memory fake and make no
  real Keychain writes.
- Focused regressions prove cold no-reasoning, identifier-only atomic persistence and reload with
  no secret/prompt/path fields, Codex rejection, fake OpenAI store/delete/failure behavior,
  model/selection switching with old resolutions pinned, and absence of legacy product choices.
  The complete app suite passed with `gpui/runtime_shaders`: 45 passed, 0 failed. App check and
  strict all-target Clippy passed.
- The credential-free provider regression suite passed after disposable loopback permission was
  granted: 60 automated tests passed, 3 manual/live unit tests ignored, and 5 historical live
  adapter tests ignored. Provider strict all-target Clippy, workspace formatting, and scoped diff
  checks passed. The initial sandboxed provider run failed only because loopback listener binding
  was denied; the approved rerun passed.
- No OpenAI request, Codex process, real Keychain write, or model quota was used. Clicking the
  explicit OpenAI validation action is the only new settings path that sends a test request.
  Manual settings readability and real Keychain/API behavior were not exercised by the automated
  review and remain product dogfood rather than evidence claimed here.

## Independent review evidence (2026-08-11)

- Generation/cancellation review passed. Every selection, model, key-store, and key-delete mutation
  invalidates the active validation attempt; invalidation increments the generation, cancels its
  token, restores prior readiness, and retains the worker handle until it can be joined. A stale or
  disconnected result can affect state only when its generation still owns the active attempt.
- Ownership review passed. `ValidationTicket` and the settings `ValidationLease` cancel on drop;
  replacing a view lease cancels the previous attempt. The controller owns active and retired
  `JoinHandle`s, reaps completed workers, and cancels then joins all remaining workers on drop.
  Validation awaits both provider startup and first stream output with the cancellation token and
  does not rely on a fixed timeout.
- Notification review passed. Public mutations snapshot the observable settings/readiness/status,
  active-attempt state, and runtime revision, notifying only when that tuple changes. Accepted
  validation completion notifies because it terminates the attempt; stale completion returns false
  and does not notify. The GPUI observer regression verifies one real mutation notification and no
  notification for the stale result.
- Fresh focused verification used no real Keychain or network access:
  `CARGO_TARGET_DIR=/tmp/sotto-t027-review WHISPER_DONT_GENERATE_BINDINGS=1 cargo test -p app
  --features gpui/runtime_shaders reasoning --locked` passed 13/13, and strict
  `cargo clippy -p app --all-targets --features gpui/runtime_shaders --locked -- -D warnings`
  passed. Only dependency future-incompatibility notices for `block` and `proc-macro-error2` were
  emitted; they are not T027 warnings.
