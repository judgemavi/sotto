# ADR-0003: GPUI whiteboard and overlay decision

- Status: pending 30-minute and real-call validation
- Date: 2026-07-29
- Owners: T003

## Context

Sotto needs a stable, continuously appending whiteboard plus a compact overlay lens over
the exact same scene. The decision depends on measured 30-minute canvas behaviour and
native macOS panel behaviour during a real full-screen call, not compilation alone.

## Prototype

The spike pins the latest aligned crates.io releases, `gpui = 0.2.2` and
`gpui-component = 0.5.1`, exactly. `gpui-component 0.5.1` itself depends on GPUI 0.2.2.
It was built with Xcode 26.6 (17F113) and the separately installed Metal Toolchain
17F109 (`Apple metal version 32023.883`). T010 must reproduce both toolchain components.
The pinned GPUI dependency graph currently emits future-incompatibility warnings from
transitive `block 0.1.6` and `proc-macro-error2 0.2.1`.

`Scene` assigns world-space placement once at append time. The renderer asks it only for
objects intersecting a lens's world-space viewport. Board and overlay windows hold the
same GPUI `Entity<Scene>`; the overlay derives its viewport from the scene frontier.
The monotonic append layout permits `partition_point` to seek to the first candidate;
rendering no longer scans all prior session objects before applying viewport culling.
Each lens owns only its viewport transform: scrolling pans in world space and
Control-scroll zooms from 0.15x to 3x. Object coordinates and the shared scene never
change during either interaction.

The fake stream periodically anchors a suggestion to its newest utterance, then emits
five progressively longer text payloads at 100 ms intervals. Both windows observe the
same card and its stable anchor through the shared scene entity.

The Tokio/GPUI seam is one bounded `tokio::sync::mpsc` receiver. A Tokio runtime on the
fake producer thread writes events; one GPUI foreground task drains the channel every
16 ms through `AsyncApp::update`, updates the shared entity, and notifies both lenses.
T012 should retain this single ingress seam.

## Native overlay implementation

Source inspection corrected the initial finding: GPUI 0.2.2's `WindowKind::PopUp` path
already constructs an `NSPanel` with `NSWindowStyleMaskNonactivatingPanel`, level 101,
and collection behavior `CanJoinAllSpaces | FullScreenAuxiliary`. The spike selects that
kind and also sets `focus: false`; no escape hatch is needed for those properties.

There is exactly one `objc2` escape hatch. `set_click_through` obtains GPUI's AppKit
`NSView` via `raw-window-handle`, sends `window` to obtain its owning `NSWindow`, and
sends `setIgnoresMouseEvents:`. The harness alternates passive and interactive modes
every five seconds. This compiles under strict Clippy, but the interaction still needs
manual demonstration against a real call before it counts as acceptance evidence.

## Validation still required

The release harness requests animation frames continuously and records board render
interval percentiles. One short smoke measurement was completed; it is not a substitute
for the required 30-minute run:

| Elapsed | Frame time p50/p95 | Idle CPU | Append CPU | RSS | GPU | Visible objects |
|---|---:|---:|---:|---:|---:|---:|
| 1 min | 8.332 / 13.517 ms (7,179 samples) | not isolated | 8.7% total | 70.9 MiB | not measured | not recorded |
| 10 min | pending | pending | pending | pending | pending | pending |
| 30 min | pending | pending | pending | pending | pending | pending |

Also verify that the overlay stays above the full-screen call, never drops focus or a
keystroke from meeting chat, and toggles between passive click-through and interaction.

## Decision

No verdict yet. Automated checks and the one-minute release smoke run are encouraging,
but frame-time drift is undefined without the 10- and 30-minute samples. No real Zoom or
Meet call, full-screen Space, keystroke-focus test, interactive click-through test, or GPU
measurement was performed. Those claims remain deliberately open. Mark GPUI as pass only
if frame-time drift is negligible and all overlay behaviors pass; if they cannot be
demonstrated inside the seven-day timebox, choose the Electron shell.
