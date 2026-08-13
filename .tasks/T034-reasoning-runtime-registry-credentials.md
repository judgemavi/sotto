# T034 — Reasoning runtime registry and OpenAI credential seam

**Status:** done

**Wave:** R1.5 — provider prerequisite for product settings

**Depends on:** T024; T026; T031; T030's recorded FAIL verdict

**Owns:** `crates/providers/src/backend/**`, `crates/providers/src/reasoning.rs`,
`crates/providers/src/openai/credentials.rs`, the narrow OpenAI credential exports/call sites in
`crates/providers/src/openai/mod.rs`, `crates/providers/src/lib.rs`, and matching provider tests.
`crates/providers/Cargo.toml` and `Cargo.lock` only if implementation requires a dependency change.

## Goal

Preserve T031's screen-authorized reasoning transport when a backend is registered, selected, and
resolved, and give T027 an OpenAI-specific write-only Keychain seam that does not reintroduce the
legacy five-provider enum into product settings.

## Plan

1. Store `Arc<dyn ReasoningProvider>` in the product registry and return the same trait from a
   pinned `ResolvedBackend`.
2. Retain the existing text-only registration signature as an adapter only when the descriptor
   does not advertise image input. Require explicit advanced registration for image-capable
   descriptors so capability metadata cannot outlive the actual dispatch trait.
3. Keep backend ids open-ended and retain fake-third-backend registration, independent role
   selection, cache fingerprinting, readiness refresh, and pinned-call cancellation semantics.
4. Add OpenAI-specific store/load/delete/has credential functions backed by the existing
   `dev.sotto.llm` / `openai` OS credential entry. Secrets remain `SecretString`, never appear in
   debug output, and are not written to application settings or files.
5. Make `OpenAiProvider::from_keychain` use the OpenAI-specific seam. Keep historical generic
   credential functions parked for compatibility; T027 does not import them or `ProviderKind`.

## Contract for downstream tasks

T027 registers OpenAI with the advanced registry method, persists only backend/model/role ids, and
uses `providers::openai` credential functions. T029 receives a pinned
`Arc<dyn ReasoningProvider>` and can exercise T031 without bypassing the registry.

## Acceptance

- Resolving an advanced OpenAI-shaped backend preserves the `ReasoningProvider` dispatch seam.
- The source-compatible registration method rejects an image-capable descriptor before registry
  mutation, while ordinary text-only and fake-third backends continue to register unchanged.
- Selection changes and readiness refreshes do not retarget an already resolved backend.
- OpenAI credential helpers preserve the existing Keychain identity, distinguish missing entries
  from credential-store failures, redact secret debug output, and perform no file persistence.
- Provider tests, formatting, strict all-target Clippy, and `git diff --check` pass without API
  credentials.

## Out of scope

App settings, settings persistence, model choice UI, map/session lifecycle, live API validation,
Codex enablement, and adding another provider.

## Implementation evidence

- `Registry` and `ResolvedBackend` now retain `Arc<dyn ReasoningProvider>`. The existing
  `register` method remains source-compatible for text-only descriptors by wrapping the supplied
  `CompletionProvider`; it rejects `ImageInput` before mutation. `register_reasoning` preserves
  the concrete advanced trait for OpenAI and future image-capable connectors.
- A fake image-capable third backend proves advanced dispatch survives registration, role
  selection, and resolution. The existing fake-third-backend, independent role, fingerprint,
  readiness-refresh, cancellation, and pinned-call regressions remain green.
- The OpenAI schema/image request-shape test now dispatches through a resolved registry backend,
  proving T031's opaque screen authorization is not erased by product selection.
- `providers::openai::{store_api_key, load_api_key, delete_api_key, has_api_key}` use the existing
  `dev.sotto.llm` / `openai` Keychain identity. `OpenAiProvider::from_keychain` uses this seam;
  product settings no longer need `ProviderKind`. Missing/deleted entries remain ordinary while
  other keyring failures normalize to `CredentialStore`; `SecretString` debug output is redacted.
- No provider manifest or lockfile change was required. No credentialed request or Keychain write
  was run, and no API/model quota was consumed.
- Verification: `env -u OPENAI_API_KEY cargo test -p providers --locked` passed with disposable
  loopback permission (56 passed, 3 ignored; 5 historical live tests ignored). The first sandboxed
  attempt failed only because local listener binding returned `Operation not permitted`.
  `cargo clippy -p providers --all-targets --locked -- -D warnings`,
  `cargo fmt --all -- --check`, and `git diff --check` pass.

## Independent review — accepted (2026-08-11)

- Confirmed `register_reasoning` checks both `JsonSchemaOutput` and `ImageInput` against capability
  support reported by the exact `ReasoningProvider` trait object before mutating the registry. The
  default text adapter reports neither; focused regressions cover image and schema overclaims.
- Confirmed `BackendDescriptor::new`, `with_auth_status`, and `Registry::refresh_auth_status`
  reject `AuthKind`/`AuthStatus` mismatches without mutation. OpenAI reports `ApiKey` with
  `NeedsApiKey` or `Ready`; Codex reports only login-compatible states.
- Confirmed resolved calls pin both the advanced provider trait object and descriptor/fingerprint,
  so later selection or readiness changes do not retarget an in-flight call. Cancellation and usage
  remain observable through the resolved provider.
- Confirmed the OpenAI-specific credential seam retains service `dev.sotto.llm` and account
  `openai`, treats missing credentials as an ordinary optional state, normalizes store failures,
  and keeps secret debug output redacted. No Keychain write, live request, or quota was used.
- Independent verification: the credential-free locked provider run passed with 60 automated tests,
  3 provider-unit manual/live tests ignored, and 5 historical live-adapter tests ignored. The first
  sandboxed run failed only because disposable loopback listeners were denied; the approved
  loopback rerun passed. Strict all-target Clippy, workspace formatting, and the scoped provider/task
  `git diff --check` passed.

T034 is `done`; its provider registry and credential handoff is released to T027.
