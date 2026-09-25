# Sotto

*From “sotto voce” — under the voice. The quiet prompt beneath the conversation.*

Sotto is a local-first desktop meeting copilot. Start a recording deliberately, select
a capture target through the macOS system picker, and review the transcript alongside
your notes. You can also record an audio note or import an audio/video file.

The local record works without an API key: audio stays on your Mac and transcription
runs on-device with Whisper. Optional reasoning adds cited meeting notes and an Ask
dock for questions about your recordings. Reasoning can use your own OpenAI API key
or an explicitly enabled experimental Codex CLI connection. Authorized MCP resources
can supply additional context.

Sotto is a native Rust application using Longbridge `gpui-kit`, with a small statically
linked Swift capture bridge. There is no Electron shell, webview, or bundled sidecar.
The desktop app currently targets macOS; this is an in-development project, not a
finished release.

## Requirements

- macOS 14 or later. Capture behavior and available target metadata depend on the OS version.
- Xcode with a macOS SDK and Swift 6.2 or later, selected as the active developer directory.
- Rust through `rustup`. The repo pins Rust **1.97.1**, including `rustfmt` and `clippy`,
  in [rust-toolchain.toml](rust-toolchain.toml).
- CMake for the native Whisper build (`brew install cmake` if you use Homebrew).
- Internet access for the initial dependency build and model downloads. Whisper weights
  are not bundled with the application.

Check the native build tools before building:

```sh
xcode-select -p
xcrun swift --version
xcrun clang --version
cmake --version
```

If Xcode is installed at its usual location but is not selected:

```sh
sudo xcode-select --switch /Applications/Xcode.app/Contents/Developer
```

Open Xcode once to complete any requested first-launch setup.

## Setup and run

```sh
git clone git@github.com:judgemavi/sotto.git
cd sotto
rustup show
```

The SSH clone requires repository access. `rustup show` resolves the repository's
pinned toolchain; Cargo downloads Rust dependencies during the first build.

### Run with capture support

From the repository root:

```sh
WHISPER_DONT_GENERATE_BINDINGS=1 scripts/run.sh
```

This builds the **release** binary, creates and signs `target/Sotto.app`, then launches
it in the foreground. Every invocation runs the build step, so it picks up saved source
changes. Quit the running app and rerun the command after editing; there is no hot reload.
The first build can take a while because GPUI, Whisper, and the Swift bridge compile locally.

`WHISPER_DONT_GENERATE_BINDINGS=1` uses the Whisper crate's pregenerated bindings;
it does not disable Whisper or on-device transcription.

The bundle is ad-hoc signed by default. macOS may require you to re-grant capture
permissions after a rebuild. If you already have a local code-signing certificate,
use its name to give the development bundle a stable signing identity:

```sh
SOTTO_DEV_IDENTITY="Your Code Signing Certificate" WHISPER_DONT_GENERATE_BINDINGS=1 scripts/run.sh
```

Local signing is not notarization or a release distribution workflow.

### Faster debug iteration

For UI development without relying on the bundled capture permission flow:

```sh
WHISPER_DONT_GENERATE_BINDINGS=1 cargo run -p app --locked
```

The bare executable may fail to present the system picker or may have different macOS
permission behavior. To run a **debug build inside the signed app bundle**, use:

```sh
WHISPER_DONT_GENERATE_BINDINGS=1 cargo build -p app --locked && scripts/dev-bundle.sh target/debug/app && ./target/Sotto.app/Contents/MacOS/sotto
```

These examples use Cargo's default `target/` directory. If you override
`CARGO_TARGET_DIR`, pass the resulting binary path to `scripts/dev-bundle.sh` yourself.

## First use

1. Download a transcription model when prompted. You can change the model in Settings;
   transcription needs local weights, but no reasoning provider or API key.
2. Grant Microphone and Screen & System Audio Recording permissions as requested in
   **System Settings → Privacy & Security**. Follow any macOS relaunch instructions.
3. Choose **Capture** and select the target in the system picker. Check the capture
   indicator for the reported audio and screen scope; do not assume window selection
   alone proves isolation from every other application's audio.
4. Stop explicitly, then review the recording from the session rail. Capture never
   starts automatically just because a meeting is open.

Reasoning is optional. Configure a provider in Settings and select it for Notes or Ask.
The OpenAI API path uses your own key stored in the OS keychain. The experimental Codex
path requires an installed, authenticated CLI and explicit consent; its tool-isolation
limitations are disclosed in the app. Leaving reasoning off preserves local recording
and transcript review.

Audio is not sent to reasoning providers. Enabled reasoning may send transcript text
and authorized context; screen-image disclosure requires separate opt-in. Local-only
use still needs the initial model downloads before it can operate offline.

## Local data and configuration

The default database is:

```text
~/Library/Application Support/Sotto/sotto.sqlite3
```

Recordings are stored in the adjacent `recordings/` directory. Application settings
and managed transcription models also live under Sotto's Application Support directory.

Optional environment variables:

| Variable | Purpose |
| --- | --- |
| `SOTTO_DATABASE` | Override the application database path; recordings are placed beside that database. |
| `SOTTO_WHISPER_MODEL` | Use an existing local Whisper ggml model file instead of the managed model. |
| `SOTTO_DEV_IDENTITY` | Choose the signing certificate used by the development bundle script. |

## Development checks

Run from the repository root:

```sh
cargo fmt --all -- --check
WHISPER_DONT_GENERATE_BINDINGS=1 cargo test -p app --lib --locked
WHISPER_DONT_GENERATE_BINDINGS=1 cargo clippy -p app --all-targets --locked -- -D warnings
git diff --check
```

For the full Rust workspace:

```sh
WHISPER_DONT_GENERATE_BINDINGS=1 cargo test --workspace --locked
```

Some integration tests are intentionally ignored because they need real models,
capture permissions, or external fixture tools. Passing automated tests does not replace
a native smoke test of capture, notes editing, Ask, settings, and both appearances.
If changing the app bundle metadata, also run `plutil -lint scripts/Info.plist`.

The headless CLI exposes transcription, timeline replay, and other pipeline commands:

```sh
WHISPER_DONT_GENERATE_BINDINGS=1 cargo run -p cli --locked -- --help
```

## Repository layout

- `crates/app` — native workspace, settings, and application controllers.
- `crates/core` — timeline types and headless pipeline contracts.
- `crates/capture` — capture abstraction and macOS Swift bridge.
- `crates/asr`, `vad`, `prosody`, `screen` — local processing stages.
- `crates/rag` — local persistence and retrieval.
- `crates/providers`, `insight`, `mcp` — optional reasoning and authorized external context.
- `crates/cli` — headless development and verification harness.
- `scripts` — app bundling, signing, and notarization helpers.
- `docs/adr` — architecture decisions, including the [Longbridge gpui-kit migration](docs/adr/0025-gpui-kit-longbridge.md).

See [AGENTS.md](AGENTS.md) for product boundaries and engineering conventions, and
[fixtures/README.md](fixtures/README.md) for test fixture guidance.
