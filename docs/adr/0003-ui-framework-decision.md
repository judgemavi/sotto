# ADR-0003: UI framework decision

- Status: accepted — retain GPUI
- Date: 2026-07-29
- Owners: T003, T020

## Context

Sotto needs a stable, continuously appending whiteboard plus a compact overlay lens over
the exact same scene. The spike investigated whether GPUI exposes the necessary scene,
rendering, and native-panel primitives. Its measurements inform the framework decision;
checks that could not be run remain explicit residual risk rather than evidence of a
technical failure.

## Prototype

The spike pins the aligned crates.io releases `gpui = "=0.2.2"` and
`gpui-component = "=0.5.1"`. It was built with Xcode 26.6 (17F113) and the separately
installed Metal Toolchain 17F109 (`Apple metal version 32023.883`). T010 must reproduce
both toolchain components. The pinned dependency graph emits future-incompatibility
warnings from transitive `block 0.1.6` and `proc-macro-error2 0.2.1`.

`Scene` assigns world-space placement once at append time. Rendering uses
`partition_point` to seek to the first viewport candidate and stops at the viewport's
right edge. Board and overlay hold the same GPUI `Entity<Scene>`; the overlay derives its
viewport from the shared scene frontier. Each lens owns only its transform, so append,
pan, and zoom do not reflow previously placed objects. The automated stability test
preserved the first object's identifier, rectangle, and payload after 10,000 appends.

The Tokio/GPUI seam is one bounded `tokio::sync::mpsc` receiver. A Tokio runtime on the
producer thread writes events; one GPUI foreground task drains the receiver every 16 ms
through `AsyncApp::update`, updates the shared entity, and notifies both lenses.

## Native overlay implementation

GPUI 0.2.2's `WindowKind::PopUp` constructs an `NSPanel` with
`NSWindowStyleMaskNonactivatingPanel`, level 101, and collection behaviour
`CanJoinAllSpaces | FullScreenAuxiliary`. The spike also sets `focus: false`.

There is one `objc2` escape hatch. `set_click_through` obtains GPUI's AppKit `NSView`
through `raw-window-handle`, gets its owning `NSWindow`, and sends
`setIgnoresMouseEvents:`. The harness alternates passive and interactive modes every five
seconds. Compilation and source inspection are not substitutes for the required manual
full-screen, focus, keystroke, and pointer tests.

## Measured evidence

The earlier one-minute release smoke reported p50 8.332 ms, p95 13.517 ms over 7,179
samples, 8.7% total CPU, and 70.9 MiB RSS. Idle CPU, GPU use, and visible-object count
were not captured in that run.

T020 corrected the harness to report non-cumulative frame intervals at 1, 10, 20, and 30
minutes. This makes the outstanding long-run test capable of exposing interval drift
instead of hiding it in a cumulative average.

Culling was measured deterministically using the 1,200-by-700 board viewport after
112,500 utterances, approximately 30 minutes at the harness's 16 ms append cadence:

| Zoom | Accumulated objects | Visible objects |
|---:|---:|---:|
| 3.00x | 112,500 | 2 |
| 1.00x | 112,500 | 4 |
| 0.15x (minimum) | 112,500 | 26 |

This confirms that narrow and minimum-zoom rendering work is bounded by viewport density
rather than accumulated duration. The test does not claim GPU behaviour.

The two lenses are demonstrably wired to one `Entity<Scene>` in the prototype and the
tests confirm stable placement and suggestion anchors. This architectural check passes.

## Unrun checks and residual risk

The 30-minute interactive run was not performed, so no 10-, 20-, or 30-minute frame-time
figures are claimed. Long-run frame-time drift therefore remains unknown.

The following checks were not run against a real full-screen Zoom or Meet call:

- staying visible over the meeting's full-screen Space;
- typing continuously in meeting chat without focus theft or dropped keystrokes;
- toggling click-through between passive and interactive modes;
- idle CPU, append CPU, RSS, and GPU use while the meeting runs beside Sotto.

The T020 execution environment did not provide an interactive meeting call, meeting chat,
or a manual GPU profiling session. Consequently the source-level expectations around the
non-activating panel and resource use remain unverified. This is residual implementation
risk, especially for focus behaviour, but it is not a demonstrated GPUI failure.

## Decision

Retain **GPUI 0.2.2** as Sotto's UI framework. The prototype establishes the required
append-only scene model, viewport-bounded culling, shared board/overlay state, Tokio-to-UI
seam, and access to the necessary AppKit panel and click-through primitives. The measured
results do not establish a reason to replace GPUI.

T012 and T016 may proceed using the shared `Entity<Scene>`, monotonic placement,
viewport-first rendering, bounded channel seam, `WindowKind::PopUp`, and the single
`objc2` click-through escape hatch described above. The overlay must remain a viewport
onto the shared scene rather than acquiring independent state.

Before calling the overlay production-ready, run and append the outstanding 30-minute
interval measurements, real-call full-screen/focus/click-through checks, and colocated
CPU/RSS/GPU profile to this ADR. A measured failure in those checks should trigger a
targeted fix and re-test; changing UI frameworks would require a separate ADR grounded in
that evidence.

## Validation commands

```text
cargo test -p app culling_is_measured_at_supported_zoom_extremes -- --nocapture
cargo test -p app
cargo clippy -p app --all-targets -- -D warnings
```

The culling measurement and all four app tests passed. Clippy initially identified a
test-only float-to-integer cast; T020 removed the cast and the final command passed.
