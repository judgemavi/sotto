# ADR-0001: Use a pure-Rust core and GPUI application shell

- Status: Accepted
- Date: 2026-07-29
- Decision owners: Sotto maintainers

## Context

Sotto is a local-first, real-time call copilot that must share a machine with a video
meeting application. Audio capture, VAD, transcription, prosody, retrieval, and
provider streaming all sit on a latency-sensitive path. The product identity also
depends on a small, inspectable footprint and on keeping audio entirely on-device.

A webview or Electron application would introduce another runtime and an IPC boundary
between the interface and the pipeline. A native application keeps the deployment
surface smaller, but GPUI is pre-1.0 and may not expose every macOS `NSPanel` behavior
needed by the overlay.

## Decision

Sotto will be a Cargo workspace implemented in Rust and distributed as one native
binary. The asynchronous pipeline uses Tokio. The desktop interface uses GPUI with
`gpui-component`, with every GPUI version pinned exactly and upgrades recorded in a
new ADR.

The pipeline and intelligence layers remain headless and never depend on GPUI. The
application consumes core events through one explicit Tokio-to-GPUI boundary. This
keeps the CLI and CI usable without a display server and prevents a future interface
change from rewriting the pipeline.

Phase 0 includes a five-day GPUI overlay spike. GPUI is the v1 interface only if that
spike proves the required non-activating, always-on-top, full-screen, and click-through
panel behavior.

## Consequences

- The product has one primary implementation language and no web runtime or hot-path
  IPC boundary.
- Core crates remain independently buildable and testable in headless CI.
- UI work must account for GPUI API churn and may require narrow AppKit escape hatches.
- Contributors must consult GPUI and Zed source when published documentation is
  incomplete.
- Crate-specific dependencies stay with their owning crates so platform and UI weight
  does not leak into the headless core.

## Revisit if

- The Phase 0 overlay spike cannot satisfy any required panel behavior within five
  days.
- GPUI cannot provide stable macOS support at an acceptable maintenance cost.
- Profiling shows the selected UI stack violates Sotto's CPU, memory, or latency
  budgets.
- A supported alternative can preserve the headless-core boundary while materially
  reducing product risk.

