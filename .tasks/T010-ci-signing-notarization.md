# T010 — CI: build, lint, test, and macOS code signing + notarization

**Status:** done (approved at review round 1)

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

## Notes

- Added macOS ARM workspace CI with pinned Xcode 26.6 and Rust 1.97.1, Cargo build
  caching, strict all-feature lint/build/test coverage, and a separate explicit
  headless-crate dependency/build/test guard covering `core`, `vad`, `asr`, `prosody`,
  `screen`, `providers`, `advisor`, `rag`, and `cli` without selecting `app`.
- Added the tag release workflow. It assembles an arm64 `Sotto.app`, signs it with a
  temporary keychain, notarizes with an App Store Connect API key, staples and validates
  the ticket, runs `codesign` and `spctl`, and publishes the ZIP plus SHA-256. A clearly
  marked hook reserves the updater manifest/signature step for Phase 4.
- `scripts/sign.sh` accepts one executable or app path. T002's approved local contract
  is `MACOS_SIGNING_IDENTITY=- scripts/sign.sh target/release/examples/soak`; CI mode
  imports the documented Developer ID `.p12`. All exits restore the original keychain
  search list and remove temporary certificate/keychain material.
- `scripts/notarize.sh` accepts an archive and optional separate staple target. A
  rejection prints the submission result and fetches Apple's full log before failing.
- Added capture entitlements and honest microphone/screen capture consent copy. Apple
  exposes ScreenCaptureKit through TCC rather than a Developer ID entitlement; that
  boundary and runtime-permission responsibility are documented.
- Static validation passed: `bash -n`, plist linting, YAML parsing, script negative-path
  exit checks, `cargo fmt --all -- --check`, and `git diff --check`. Real Developer ID
  signing, notarization, stapling, `spctl` on a clean Mac, and tag publication remain
  manual credentialed gates; no external release or signing was attempted here.
- Workspace all-feature Clippy was attempted against the concurrent wave state. It is
  presently blocked outside T010 ownership by an unused import in `core`, denied
  `panic!` calls in `capture/build.rs`, and the local machine having Command Line Tools
  but no `xcrun metal`; the pinned full-Xcode CI runner supplies the missing Metal tool.
- Approved follow-ups landed: the headless dependency/build/test guard includes
  `capture`, and full-workspace/release jobs explicitly download the separately
  versioned Metal Toolchain after selecting Xcode 26.6.

## Review round 1 — approved

CI, signing, notarization, release workflow and docs all land as specified. Reviewed
closely because this task handles secrets:

- `set -euo pipefail` in both scripts, `trap cleanup EXIT` plus INT/TERM handlers, so the
  temporary keychain and the decoded `.p12` are removed on the failure paths too — which
  is the part that usually gets skipped.
- Temporary keychain with restored `list-keychains` state rather than mutating the
  user/runner default keychain permanently.
- Notarization uses an App Store Connect API key, not an app-specific password, as the
  task required.
- The **headless-core job is the good one**: it rejects GPUI from the dependency graph
  mechanically rather than by convention, which is exactly what makes the `AGENTS.md`
  headless-core rule enforceable rather than aspirational.

The non-blocking follow-ups from review are now landed: `capture` participates in the
headless dependency/build/test checks, CI selects Xcode 26.6 explicitly, and jobs that
compile GPUI download the separate Metal Toolchain component before building.

## Environment update — Xcode installed (2026-07-29)

Xcode 26.6 (17F113) is now on the dev host, so `xcrun notarytool` and `xcrun stapler`
resolve locally. Two follow-ups, neither blocking the approval above:

- **Pin the Xcode version in CI now.** The task already called for this; it matters more
  now that a specific version is in use locally. A runner-image bump that silently changes
  the Xcode or SDK version is a nasty class of failure, and the dev host and CI disagreeing
  about it is worse.
- **The Metal Toolchain is a separate component in Xcode 26.** The full-workspace job
  builds `app` → GPUI → Metal shaders, so CI needs
  `xcodebuild -downloadComponent MetalToolchain` (and ideally a cache for it) or that job
  fails on a fresh runner exactly the way the dev host did. Add it as an explicit step
  rather than relying on the runner image happening to include it.

All CI follow-ups from this review are resolved.


## Follow-up — enforce the tier boundary in CI (2026-07-29)

`AGENTS.md` now states that `core` must never depend on `providers`: the map tier works with
no API key, and the crate graph is what makes that real rather than aspirational.

The headless job already proves the same kind of thing for GPUI, mechanically, with
`cargo tree`. Add the equivalent check for `providers` in the `core` dependency graph, and
fail the build if it appears. A convention nobody can violate accidentally is worth more
than a paragraph in a design document — this is exactly the pattern that made the
headless-core guard valuable.

Also still open from the earlier review: add `capture` to the headless job's crate list.
