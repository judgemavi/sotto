# T033 — Restore locked workspace builds across whisper.cpp bindings

**Status:** done

**Wave:** M0a — build gate before map-tier integration can be accepted

**Depends on:** T005; done T023 provisioner library

**Owns:** `crates/asr/Cargo.toml`, `rust-toolchain.toml`, `.cargo/config.toml`,
`.github/workflows/ci.yml`, `.github/workflows/release.yml`, `Cargo.lock`, and
`docs/experiments/whisper-bindings-toolchain.md`. Any dependency or toolchain change is
sequential and requires a recorded lockfile handoff.

## Goal

Make a clean, locked build of `whisper-rs-sys` and the complete headless/workspace test
graphs reproducible on the pinned macOS arm64 toolchain. Remove the current generated
`whisper_full_params` layout assertion failure without relying on a developer's stale
`target/` contents or an undocumented ambient environment variable.

## Plan

1. Reproduce from a new task-local `CARGO_TARGET_DIR` with the repository's pinned Rust,
   Xcode/SDK, deployment target, and both binding-generation modes recorded.
2. Determine whether the mismatch is a `whisper-rs`/`whisper.cpp` release incompatibility,
   bindgen/Clang input drift, feature selection, or stale generated output. Do not patch
   generated files in `target/`.
3. Prefer the smallest maintained fix: a compatible pinned crate revision/release or a
   documented supported bundled-binding mode. A Rust/Xcode pin change needs measured
   GPUI/capture fallout and an ADR-quality note.
4. Make CI and local commands use the same declared configuration. Keep Metal enabled and
   preserve the headless no-GPUI dependency guard.
5. Re-run ASR, headless, and workspace gates from clean task-local build directories.

## Contract for downstream tasks

T032 and T016 receive one documented locked build command. They must not encode their own
Whisper binding workaround or claim a product failure for a toolchain-only error.

## Acceptance

- A new empty `CARGO_TARGET_DIR` builds and tests `asr` with `--locked` on macOS arm64.
- The CI headless build/test commands pass without an undeclared
  `WHISPER_DONT_GENERATE_BINDINGS` requirement.
- The workspace reaches application compilation when the pinned Metal toolchain is
  installed; missing local Metal is reported separately and actionably.
- The real T023 base.en known-answer canary still loads with Metal and recognises the
  fixture terms after any dependency change.
- Formatting, strict all-target Clippy, dependency/lockfile diff review, and
  `git diff --check` pass.

## Out of scope

Installing Xcode components on a user's machine, changing ASR model policy or provisioning
UX, removing Metal acceleration, GPUI renderer work, and app session wiring.

## Notes

On 2026-08-11, `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test --workspace --locked` still
failed in generated `whisper-rs-sys` bindings because
`size_of::<whisper_full_params>() - 296` underflowed. The same run also reached GPUI's
separate missing-local-Metal-toolchain error. Focused ASR tests pass with the existing
local cache, so this task must begin from a clean target directory and must not treat a
warm-cache pass as acceptance.

On 2026-08-11, a fresh target without the bundled mode reproduced the failure exactly:
host bindgen emitted an opaque one-byte `whisper_full_params` while retaining its 296-byte
layout assertion. A second fresh target with the crate-supported bundled mode compiled ASR
and reached its tests; sandboxed loopback-server tests then failed with `Operation not
permitted`, which is unrelated to bindings. `.cargo/config.toml` now declares and forces
that bundled mode for local and CI commands while leaving the Cargo `metal` feature enabled.
See `docs/experiments/whisper-bindings-toolchain.md` for the toolchain record and diagnosis.

## Independent review — accepted (2026-08-11)

- A clean target with the ordinary locked command compiled the crate-packaged bindings; the
  prior generated one-byte struct failure did not recur.
- The full ASR suite passed 18 automated tests with the explicit real-model test ignored. The
  separately run base.en canary passed on Metal and recognised the known fixture terms.
- A separate fresh app target subsequently compiled through `whisper-rs-sys` without any shell
  binding override and reached Sotto's app tests, confirming the fix applies to a downstream
  feature graph rather than only the ASR crate.
- A warm pre-fix app target still reused its old broken `OUT_DIR` because the upstream build
  script does not emit a rerun trigger for the binding-mode variable. The runbook records the
  one-time package clean/fresh-target action; clean developer and CI builds are unaffected.
- Strict ASR Clippy, the headless GPUI dependency guard, and scoped diff checks passed. The task
  changed no dependency, toolchain pin, or lockfile entry.

T033 is accepted. The missing local Metal compiler remains a separate environment condition;
CI installs that component, and focused local app verification used GPUI runtime shaders.

Verification on the same date, without an explicit binding environment variable:

- `CARGO_TARGET_DIR=/private/tmp/sotto-t033-fixed-20260811-a cargo test -p asr
  --locked` from an empty target: 18 passed, 0 failed, 1 ignored canary. The produced
  `OUT_DIR/bindings.rs` is byte-identical to the crate's bundled binding.
- The ignored real base.en known-answer canary was then run explicitly: 1 passed; the
  runtime logged `using Metal backend` on Apple M1 Pro and recognised `pricing` and
  `enterprise`.
- Strict all-target ASR Clippy passed, the headless dependency guard found no GPUI, and
  `git diff --check` passed.
- No dependency was changed and T033 made no `Cargo.lock` edit. The lockfile remains a
  shared handoff containing the already-active T023/T026 work.
- Full workspace formatting was not claimed: `cargo fmt --all -- --check` reported only
  concurrent T016 work in `crates/app/src/board/thumbnails.rs`, outside T033 ownership.
  The full app/workspace run remains the separate Metal-toolchain consumer check in T022;
  T033 did not install Xcode components or wait on that gate.
