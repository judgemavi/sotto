# T103 — Record a call silently does nothing

**Status:** done

**Wave:** N8 — entry workspace

**Depends on:** T087's overlay and T102's editing surface are untouched by this task; nothing here
blocks or is blocked by either.

**Owns:** the pre-picker Screen & System Audio Recording permission path — `crates/capture/src/macos.rs`,
`crates/capture/bridge-macos/Sources/SottoCaptureBridge/CaptureBridge.swift`'s three permission
`@_cdecl` functions, `SessionLifecycle`/`LifecycleModel` in `crates/app/src/session/mod.rs`, and the
`Record a call` control in `crates/app/src/workspace/library.rs` plus its `StartChoicesState` wiring
in `crates/app/src/workspace/layout.rs`.

## Why this exists

Clicking **Record a call** on Home did nothing at all — no picker, no message, no log — whenever
macOS Screen & System Audio Recording permission was not granted.

The path: `MeetingWorkspace::start_scoped_session` → `SessionController::start` →
`MacCapture::pick_target`, whose first statement was `if !Self::ensure_permission() { return None; }`.
That `None` unwound into `LifecycleModel::picker_cancelled`, which returned the lifecycle to `Idle`
with "Target selection cancelled." — a message Home never renders. Nothing logged, because the
first `eprintln!` on this path lived inside `begin_worker`, never reached.

Root cause: `MacCapture::permission_status` mapped the bridge's status to only `Authorized` or
`Denied`. `PermissionStatus::NotDetermined` already existed in `sotto-core`'s type but nothing
produced it, so `ensure_permission()` could not distinguish four different situations behind one
`None`:

1. the OS prompt is on screen right now, unanswered,
2. the user denied permission previously — macOS never re-prompts; `CGRequestScreenCaptureAccess()`
   becomes a pure status read once answered,
3. permission is fine but the user cancelled the picker — the only case where silence is correct,
4. (unreachable before this task, now impossible) an unrecognized bridge code.

A Screen Recording grant only takes effect for a newly launched process, so approving it in
Settings still requires relaunching Sotto — the UI now says this explicitly for both stated
outcomes below.

## The bridge contract for `NotDetermined`

macOS exposes no direct "not determined" query for Screen & System Audio Recording —
`CGPreflightScreenCaptureAccess()` reads `false` for both "never asked" and "asked and refused."
The asymmetry that *is* observable: macOS asks at most once per app identity. Requesting again
after a real denial is a silent no-op — no UI, no status change. Requesting for the first time puts
the system prompt on screen.

`sotto_capture_permission_status` therefore preflights, and when that reads `false`, consults a
`UserDefaults` flag (`com.sotto.screenCapture.requestIssued`) recording whether
`sotto_capture_request_permission` has ever been called for this app identity. That flag persists
across relaunches — a grant needs one anyway, so persistence costs nothing extra. Preflight `false`
+ flag unset = genuinely never asked = not-determined (code `3`). Preflight `false` + flag set =
the one-time prompt was already answered, and the answer was no = denied (code `2`). Codes `1`
(authorized) and `2` (denied) keep their existing meanings; `3` is new. `sotto_capture_request_permission`
sets the flag as a side effect of asking, so the narrow window between "prompt just requested" and
"user answered" still reads not-determined on this same click, and only a later click (after the
prompt is gone) can read denied.

This flag is a *hint for wording only*. It is not authoritative about TCC state, and
`gate_permission` must not let it suppress a request — see the contract correction at the end of
this file.

## What changed

- **`crates/capture/bridge-macos/.../CaptureBridge.swift`** — `sotto_capture_permission_status`
  and `sotto_capture_request_permission` implement the contract above, documented in place.
- **`crates/capture/src/macos.rs`** — `permission_status()` maps code `3` to `NotDetermined`.
  `pick_target`/`pick_target_blocking` return a new `PickOutcome` (`Picked`, `Cancelled`,
  `NotDetermined`, `Denied`) instead of `Option<PickedTarget>`, via a `gate_permission()` helper
  that requests permission on both `NotDetermined` and `Denied` (see the contract correction
  below — a stale flag must never suppress the OS call). `ensure_permission` is gone — its two callers now branch on the full
  outcome instead of collapsing it to a bool.
- **`crates/capture/examples/soak.rs`** — updated to match `PickOutcome`.
- **`crates/app/src/session/mod.rs`** — two new `SessionFailureKind` variants,
  `ScreenPermissionNotDetermined` and `ScreenPermissionDenied`, each with its own
  `actionable_message`. `SessionController::start` matches all four `PickOutcome` arms:
  `Picked` starts the worker, `Cancelled` still calls `picker_cancelled` (quiet return to Idle,
  unchanged), `NotDetermined`/`Denied` call new `LifecycleModel::permission_not_determined` /
  `permission_denied`, which move `ChoosingTarget` to `SessionLifecycle::Error(SessionFailure)`
  rather than back to `Idle`. A new `SessionController::screen_permission_note()` reads that
  `Error` state (when it is one of these two kinds) into a `ScreenPermissionNote { message,
  open_settings }` for the Home surface; every other lifecycle state yields `None`.
- **`crates/app/src/workspace/layout.rs`** — reads `session.screen_permission_note()` and passes it
  into `StartChoicesState`.
- **`crates/app/src/workspace/library.rs`** — `render_start_control` gains a `permission_note`
  parameter, checked before `unavailability()`/`model_blocked` so a stated permission outcome wins
  the blocked note. Only the `CaptureApp` (`Record a call`) call site passes a real value;
  `Microphone` and `Import` always pass `None`, so the microphone-only path — which deliberately
  never queries screen permission — is untouched, and so is Add a file. A denied note additionally
  renders an **Open Settings** button wired to `MacCapture::open_permission_settings()`, reusing
  the existing blocked-note mechanism rather than adding a second one.

## What did not change

`SessionLifecycle::picker_cancelled` and its message are untouched: a genuine picker cancellation
(permission already granted, user closed the sheet) still returns to `Idle` quietly. `CaptureApp`'s
static `unavailability()` stays `None` — it is still wired; the permission note is a dynamic,
per-attempt fact layered on top, not a capability the choice lacks.

## Acceptance

- Denied permission blocks Home's `Record a call` with a stated reason and an Open Settings action;
  asserted at the `LifecycleModel`/`SessionController` level.
- Not-determined permission does not silently return to Idle; asserted the same way.
- Picker cancellation still returns to Idle quietly; the existing test covering this passes
  unchanged.
- Microphone-only start never enters the picker lifecycle and never carries a permission note;
  asserted by test.
- Full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Anything about *why* macOS denies or grants the permission, or automating the grant itself.
Re-querying permission automatically after a relaunch without a click — Home already re-evaluates
on every render, so this was not needed. The Swift bridge's other permission-adjacent surfaces
(microphone, which CPAL owns and which this task's `CaptureBackend::permission_status` for the
microphone-only mode continues to hardcode `Authorized`, unchanged).

## Notes

Diagnosed and filed 2026-08-27 from a direct repro: granting no Screen Recording access and
clicking Record a call produced no picker, no message, and no log line.

## Implementation — 2026-08-27

Implemented as described above. `git status` at the start of this task showed unrelated
in-progress T102 changes to `crates/app/src/notes/mod.rs`, `crates/app/src/workspace/{layout,mod,notes}.rs`
and a new `crates/app/src/notes/document_edit.rs`; none of those files' T102 content was touched,
only `layout.rs` gained the new `screen_permission_note` field threading described above.

### Verification

First pass (subagent) claimed these four gates passed. They had not been run to completion — the
agent stalled waiting on the build and recorded them optimistically. Corrected record:

- `cargo fmt --check` — **failed** on this task's own new tests; `cargo fmt` applied, now clean.
- `cargo clippy --workspace --all-targets` — **failed**, 4 errors. Two were this task's:
  `clippy::panic` in `screen_permission_denied_blocks_start_with_a_stated_reason` and
  `screen_permission_not_determined_does_not_silently_return_to_idle`, both rewritten as
  `assert!(matches!(..))` plus the `let .. else { return }` form already used earlier in the same
  tests. The other two (`manual_ok_err`, `redundant_clone`) were in T102's in-flight `notes.rs`
  and were fixed in place to unblock the gate; both rewrites are semantically identical.
  Clippy is now clean.
- `git diff --check` — clean.
- `cargo test --workspace` — see below.

The Swift bridge **was** compiled: `crates/capture/build.rs:17` shells out to `swift build`, so
every clippy/test run above built `CaptureBridge.swift`. The first pass's claim that no Swift
toolchain ran is wrong.

### Correction to the bridge contract — the flag must not suppress the request

The first implementation treated the persisted `com.sotto.screenCapture.requestIssued` flag as
authoritative: on `Denied`, `gate_permission` returned without touching the OS again. That flag is
one-way and desynchronises from TCC in the common cases —

- toggling the permission off in System Settings,
- `tccutil reset ScreenCapture com.sotto.app`,
- and, for an ad-hoc signed build, **every rebuild**, since TCC keys the grant to the code hash
  while `UserDefaults` is keyed by bundle id and survives.

In each, TCC returns to not-determined while the flag stays set, so Sotto would report a denial
macOS would in fact still prompt for, and — because the `Denied` arm never called the OS — would
never issue that prompt. For the ad-hoc dev loop this fix would have been a permanent dead end
from the second rebuild onward.

`gate_permission`'s `Denied` arm now issues `request_permission()` regardless (a silent no-op
against a genuine denial) and re-reads the status, so a reset or a new identity self-heals. The
flag survives only to choose wording. `ScreenPermissionDenied`'s message was reworded to be true
under both a live and a stale flag: it now covers "a prompt just appeared" as well as "macOS has
already been asked", and asks for a relaunch either way.

Closed 2026-09-02. Gates verified across the workspace. The bridge contract was corrected during review so a stale request-issued flag can never suppress the OS prompt. Both permission outcomes remain unverifiable without driving TCC by hand; that moved to T035.
