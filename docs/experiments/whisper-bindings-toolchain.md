# Whisper bindings and macOS toolchain

Date: 2026-08-11

## Locked environment

- Host: macOS 26.5.2, arm64
- Rust: 1.97.1 (`8bab26f4f`, LLVM 22.1.6), pinned by `rust-toolchain.toml`
- Local Xcode: 26.4.1 (`17E202`)
- Local macOS SDK: 26.4
- CI Xcode: 26.6, selected in `.github/workflows/ci.yml`
- Deployment target: macOS 14.0, declared in `.cargo/config.toml`
- ASR dependency: `whisper-rs` 0.15.1 / `whisper-rs-sys` 0.14.1 with the
  `metal` feature

## Fresh-target reproduction

Live generation was reproduced without using the repository `target/` cache:

```sh
CARGO_TARGET_DIR=/private/tmp/sotto-t033-generated-20260811-a \
  cargo test -p asr --locked
```

`whisper-rs-sys` generated this type from its vendored headers:

```rust
pub struct whisper_full_params {
    pub _address: u8,
}
```

The same generated file retained bindgen's expected 296-byte layout check. Rust
therefore rejected `size_of::<whisper_full_params>() - 296usize` as `1 - 296`
at compile time. This demonstrates drift in the host bindgen/libclang pipeline,
not stale generated output. The forward declaration is being treated as the
type definition in the combined live binding output after `whisper-rs-sys` adds
`ggml-metal.h`. The generated Rust alone does not distinguish a libclang AST
change from bindgen's traversal of that AST, so the diagnosis does not claim one.

The crate's bundled `src/bindings.rs`, released alongside the same vendored
whisper.cpp source, contains the complete 296-byte structure and all field
offset assertions. The crate build script exposes
`WHISPER_DONT_GENERATE_BINDINGS` specifically to copy that file into `OUT_DIR`.

## Decision

`.cargo/config.toml` declares and forces the supported bundled-binding mode:

```toml
WHISPER_DONT_GENERATE_BINDINGS = { value = "1", force = true }
```

This is preferable to patching generated target files, changing the pinned Rust
or Xcode toolchain, or taking an unrelated whisper.cpp upgrade. `whisper-rs`
0.16.0 / `whisper-rs-sys` 0.15.0 is available, but includes whisper.cpp and API
changes and retains both the live-bindgen path with the added Metal header and
the supported bundled-binding mode. It is not a smaller build-only fix.

The setting changes only the source of Rust FFI declarations. Metal remains
enabled in `crates/asr/Cargo.toml`, and the native whisper.cpp CMake build still
sets `GGML_METAL=ON` and `GGML_METAL_EMBED_LIBRARY=ON`.

## Locked commands

No shell environment workaround is required. From the workspace root, downstream
tasks use ordinary locked commands such as:

```sh
CARGO_TARGET_DIR=/private/tmp/sotto-asr-clean cargo test -p asr --locked
cargo test --locked \
  -p core -p capture -p vad -p asr -p prosody -p screen \
  -p providers -p advisor -p rag -p cli
cargo test --workspace --all-features --locked
```

The task-local target override is only for proving cache independence; normal
local and CI builds may use their usual Cargo target directory. Configuration
comes from the repository in both cases.

An existing target directory that already contains the broken live-generated
`whisper-rs-sys` output may require a one-time `cargo clean -p whisper-rs-sys`
or a fresh `CARGO_TARGET_DIR`. The upstream build script does not declare the
binding-mode environment variable as a rerun trigger, so changing repository
configuration cannot invalidate that old `OUT_DIR` automatically. New and CI
targets select the bundled bindings from their first build.

## Separate Metal-toolchain diagnostic

The binding failure occurs while compiling generated Rust declarations. A
missing Apple Metal Toolchain occurs later, while GPUI or native Metal shaders
are compiled, and must be reported separately. CI installs the pinned Metal
component before the workspace build. T033 does not install Xcode components on
developer machines.
