# T010 — CI: build, lint, test, and macOS code signing + notarization

**Status:** todo (unblocked — T001 approved)

**Wave:** 1 — fully parallel; touches only `.github/` and `scripts/`

**Depends on:** T001 (a buildable workspace)

**Owns:** `.github/**`, `scripts/**`, `docs/signing.md`

## Goal

`AGENTS.md` says to set up signing and notarization **now**, before capture work
matures, because capture bugs on unsigned builds waste days — TCC permissions are keyed
to code signature, so an unsigned or ad-hoc-signed binary gets re-prompted or silently
denied on every rebuild, and the resulting "capture is broken" symptoms are
indistinguishable from real bugs. T002 is blocked on this in practice even though it
compiles without it.

## Plan

1. **CI workflow** (`.github/workflows/ci.yml`) on macOS ARM runners:
   `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo build --workspace`, `cargo test --workspace`. Cache the cargo registry and
   target dir — whisper.cpp and ONNX Runtime builds are slow enough that an uncached CI
   will get ignored by the team.

2. **Headless-core guard.** A separate job building and testing `core`, `vad`, `asr`,
   `prosody`, `providers`, `rag`, `cli` **without** the `app` crate, proving the core
   never depends on GPUI. `AGENTS.md` states the core must always build headless; make
   that mechanically enforced rather than a convention.

3. **Swift bridge in CI.** Ensure the runner builds `crates/capture/bridge-macos` via
   T002's `build.rs`. Pin the Xcode version explicitly — a runner image bump silently
   changing the Swift or SDK version is a nasty class of failure.

4. **Signing pipeline** (`scripts/sign.sh`): import the Developer ID cert from a
   base64 secret into a temporary keychain, `codesign --deep --options runtime` with
   hardened runtime, and the entitlements the capture work needs
   (`com.apple.security.device.audio-input`, plus whatever ScreenCaptureKit requires).
   Delete the temporary keychain in a step that runs on failure too.

5. **Notarization** (`scripts/notarize.sh`): `xcrun notarytool submit --wait` with an
   App Store Connect API key (not an app-specific password), then `xcrun stapler
   staple`. Fail loudly with the full notarization log on rejection — the default
   output is uninformative.

6. **Entitlements + Info.plist** (`scripts/entitlements.plist`): the usage-description
   strings for microphone and screen recording. Write them as honest,
   user-facing consent copy — this is the first thing a user reads about what Sotto
   records, and per `AGENTS.md` consent handling is a first-class feature, not
   boilerplate.

7. **Release workflow** (`.github/workflows/release.yml`) on tag: build universal or
   arm64 binary, sign, notarize, staple, produce a `.dmg` or `.zip`, attach to a GitHub
   release. Leave a documented hook for the signed update manifest the Phase 4 updater
   will consume — do not build the updater here.

8. **Secrets documentation** (`docs/signing.md`): every required secret, how to
   generate it, and how to rotate it. Include a "signing works locally but not in CI"
   troubleshooting section — it will be needed.

## Contract for downstream tasks

T002 uses `scripts/sign.sh` for its soak binary. The Phase 4 updater task consumes the
release artifact format defined here.

## Acceptance

- CI green on the T001 skeleton, and stays green as wave-1 crates land.
- Headless-core job proves no GPUI dependency in the core.
- A tagged build produces a notarized, stapled artifact that passes
  `spctl -a -vvv` on a clean machine.
- Signing secrets documented well enough for a second person to set them up.

## Out of scope

The auto-updater itself (Phase 4), Windows CI (Phase 5), release notes automation.
