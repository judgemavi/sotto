# T022 — Live session: picker to transcript

**Status:** done

**Wave:** Phase 2 — the first end-to-end use of the product

**Depends on:** T021 (`PickedMacCapture` implements `CaptureBackend`) · T011 (the pipeline) ·
T012 (the timeline seam, `attach_ingress`)

**Owns:** `crates/app/src/session/**` (new), `crates/app/src/main.rs`, and the minimal
`crates/app/src/lib.rs` session-module registration needed to compile the controller once for
both the library tests and executable

Disjoint from T012 (`devwindow/**`, `settings/**`) and T016 (`board/**`), so all three can run
at once. Do not edit `crates/capture` — its remaining gaps are parked in T021.

## Why this exists

Every piece is built and tested, and none of them have met. `MacCapture` is referenced only by
the capture crate and its soak example. The CLI drives the pipeline with `FileCapture` over
fixture WAVs. The dev window is fed by `start_dev_timeline`, a fixture generator that invents
partials and finals on a timer.

So the chain that *is* the product — capture, VAD, ASR, prosody, timeline, UI — has never once
run on live audio. That is the single gap between having all the parts and having a tool. Nothing
downstream can be judged until it closes: not whether clustering finds real topics, not whether
the board reads well, not whether suggestions are worth anything.

## Plan

1. **Replace the fixture generator with a real session.** `start_dev_timeline` goes. In its
   place: pick a target, build the pipeline around `PickedMacCapture`, and feed the timeline
   ingress from the pipeline's event stream. The seam stays exactly as it is — one bounded mpsc
   drained on the GPUI foreground. T016 consumes the same `Entity<TimelineState>`, unchanged.

2. **The session's `CaptureTarget` comes from the picker.** `PickedTarget::description()`, not a
   synthesized value. `crates/cli/src/pipeline.rs` currently hardcodes `kind: TargetKind::Window`
   and derives `audio_scoped` from a CLI flag — that is fine for a fixture harness and wrong here.
   The timeline's record of what was captured has to be what the OS actually scoped, because the
   recording indicator reads from it and `AGENTS.md` makes that indicator a consent feature.

3. **Start is picker-first.** The settings button turns Sotto on, the system picker appears, and
   a session begins only if a target comes back. Cancelling leaves no session and no error state.
   Mirror the CLI's builder wiring — Silero per source, Whisper, the prosody annotator — rather
   than inventing a second configuration.

4. **Stop is always one action away, and the app never restarts a session itself.** Handle every
   terminal `CaptureStatus`: `TargetEnded` when the window closes, `UserStopped` when the user
   hits Stop Sharing in the system UI, `Failed` for real errors. Each ends the session and says
   which one happened. A session that outlives the thing it was recording is the failure mode to
   avoid — the indicator would keep claiming capture from a window that no longer exists.

5. **Whisper must be present before capture starts.** `AGENTS.md` puts `base.en` on first-run
   download. Starting a session that silently produces no transcript because the model is missing
   is worse than refusing to start with a clear message.

6. **Backpressure is already solved — do not re-solve it.** The pipeline's asymmetric policy
   (audio drops oldest, finals await capacity) and the bounded seam exist. Wire them together;
   resist adding a second queue anywhere.

## Acceptance

- Press start, pick a window, speak, and see finals appear in the dev window within a couple of
  seconds — the first time this product has transcribed live speech.
- Partials arrive and are visibly superseded by finals as new events referencing the old, not as
  in-place edits.
- The session's `CaptureTarget` matches what the picker returned, field for field.
- Closing the captured window ends the session and says so. Same for Stop Sharing.
- Cancelling the picker leaves no session.
- A ten-minute live session persists a timeline that reloads intact.

## Out of scope

The board (T016), settings screens (T012), clustering (T019), suggestions (T013). Also the
parked capture gaps in T021 — leave them parked.

## Why this unblocks everything else

The ten-minute session from the acceptance list is the real recorded conversation that T019's
validation, ADR-0006's OCR retest, and the Phase 2 gate have all been waiting on. One live
session pays off three tasks.

## Review round 1 — the wiring is right; three fixes before this can be run

The shape is correct and several judgement calls are good ones. `CaptureTarget` is cloned from
`PickedTarget::description()` rather than synthesized, so the timeline records what the OS
actually scoped. `pick_target().await` on GPUI's foreground executor is exactly the case the
async variant exists for — the run loop keeps turning, so the picker's main-queue task can run.
`SessionId::new(now.as_nanos())` is safe: `SessionId` is a `u128`. The current-thread runtime
matches `crates/cli/src/pipeline.rs`, so no new divergence. And disabling `vad_gating` with a
comment explaining that Silero is a separate producer is the right call, explained.

Clippy is clean workspace-wide with `--all-features`.

### 1. The tail of every conversation is lost from the UI

```rust
let terminal = loop { tokio::select! { … } };
pipeline.stop().await;
```

The loop breaks the moment a terminal status arrives, and nothing receives from `events` after
that. Whatever `stop()` flushes — the final utterances, which is to say the end of the
conversation — goes to a bus with no reader. Persistence is inside the pipeline and still records
them, so the UI and the persisted timeline diverge precisely at the end of the session, which is
the hardest kind of discrepancy to notice and the worst kind to have.

Drain `events` after `stop()` and forward what is left before returning.

### 2. The database path will not survive being a real app

```rust
.unwrap_or_else(|| PathBuf::from("sotto.sqlite3"))
```

Relative, so it resolves against the working directory. A bundled app's working directory is
`/`, so the session fails to open the store — and when it does work, it scatters databases
wherever the binary happened to be launched from. Use `~/Library/Application Support/Sotto/`,
creating the directory if absent. `SOTTO_DATABASE` can stay as an override.

### 3. `cargo fmt --all -- --check` fails

Four files. The checklist listed `git diff --check`, which tests trailing whitespace, not
formatting — they are not substitutes, and T010's CI runs the former. Verification was also
per-crate again (`-p app`) rather than `--workspace --all-features`; that is the fourth round.
The habit matters here more than usual, because this task touches five crates' worth of
integration.

### Smaller

- `Ok(CaptureStatus::Stopped) => break CaptureStatus::UserStopped` labels our own stop as a user
  action. Nothing reads it yet, but this is the same truthfulness category as the rest of the
  session record.
- The picker presenting at app launch is a defensible interim and the ownership reasoning behind
  it was right. It must not survive: opening the app to change a setting should not demand you
  choose a capture target. T012 needs to expose the start/stop action; that is now its highest
  priority, since T022 is otherwise finished.

### The Whisper model gap is real and is not this task's fault

`SOTTO_WHISPER_MODEL` is required because **nothing in the workspace downloads a Whisper model**.
`AGENTS.md` promises `base.en` on first run; that was a decision, never an implementation. So a
user cannot start a session without manually fetching weights. Refusing to start with a clear
message is the right behaviour for now. Tracked separately as T023.

## Review round 2 — accepted, with one regression to fix

All three landed, and verification is real this time: fmt clean, clippy `--workspace
--all-targets --all-features` clean, 79 passed / 0 failed / 7 ignored.

The drain is better than what I asked for. Spawning `stop()` and draining concurrently means the
drain is what *lets* shutdown make progress, rather than something attempted after it — a
sequential version could have deadlocked against a full timeline channel. The application-support
path handles an empty `HOME` and creates the directory.

### `Stopped` is now silently ignored, which is worse than mislabelling it

```rust
Ok(CaptureStatus::Stopped | CaptureStatus::Starting | CaptureStatus::Running
   | CaptureStatus::Stopping) => {}
```

My objection was to calling our own stop a *user* action. The fix removed the handling
altogether, so a terminal state now falls through and the loop keeps waiting for events that
will never arrive.

That matters more than it looks, because `CaptureStatus::from_code` maps **every unrecognised
code** to `Stopped`:

```rust
_ => Self::Stopped,
```

So any status the bridge grows later — or any value that arrives garbled — becomes `Stopped`,
gets ignored here, and hangs the session with the indicator still claiming capture. That is
exactly the failure mode point 4 of this task exists to prevent, reached through the fallback
branch rather than the obvious path. It is also the same shape as the `.none` filter style and
the `_ => TargetKind::Window` fallback: an unknown value quietly becoming a benign-looking one.

Break on `Stopped` with its own outcome. Ending a session because capture stopped is correct and
truthful; only the *label* was wrong.

While there: the drain loop runs until the bus closes, which assumes every sender is dropped by
`stop()`. If one ever leaks, the session never returns and the UI never learns it ended. A
timeout around the drain, falling back to a `try_recv` sweep, turns a hang into a logged
imperfection.

## Transcript-first acceptance clarification (2026-08-11)

The real session still unlocks the board and reasoning validation, but it no longer exists
primarily to justify eager OCR. It must preserve timestamped utterances, speaker/prosody,
capture-target metadata, and bounded frame intervals so T028 can request screen evidence later.
The default reasoning canary is transcript-only.

## Notes — changes-requested fix (2026-08-11)

- `CaptureStatus::Stopped` now ends the session as `Stopped`; it is neither ignored nor
  relabelled as a user action. Focused unit coverage pins that outcome and keeps the three
  transitional statuses non-terminal.
- Shutdown event draining and the pipeline stop join are independently bounded to two seconds;
  the ready-event fallback is bounded to 100 ms as well. If a leaked sender or a full UI ingress
  prevents progress, the app reports the timeout and completes shutdown instead of leaving the
  recording indicator hung. `EventReceiver` exposes no `try_recv`, so the final sweep uses a
  one-poll `now_or_never` equivalent through its public API.
- `rustfmt --edition 2024 --check crates/app/src/session/mod.rs` and scoped `git diff --check`
  passed. The first focused app test attempt stopped in `gpui`'s build script because this machine
  lacks the Xcode Metal Toolchain (`xcodebuild -downloadComponent MetalToolchain`), before reaching
  the app crate; a later workspace-format retry was also blocked by T023's concurrently incomplete
  `crates/asr/src/model` module. Focused test and Clippy execution therefore remain environment- and
  concurrent-work-blocked, not reported green.

## Product-integration handoff

T022 now owns only the picker-to-pipeline session controller and truthful terminal/shutdown
semantics. Its review may close that component once the focused verification can run. The
historical acceptance wording about pressing a visible Start button, first-run model readiness,
recording indication, and removing the launch-time picker is transferred to T032, which owns the
necessary `main.rs`, `session/**`, and `settings/**` surfaces together after their sequential
handoffs. T022 must not grow a second UI lifecycle while T032 is pending.

## Independent controller review (2026-08-11)

- Re-traced startup, terminal handling, event forwarding, persistence path, and shutdown against
  `PipelineHandle::stop` and `EventReceiver`. `Stopped`, `Failed`, `TargetEnded`, and
  `UserStopped` preserve their causes; only Starting/Running/Stopping remain transitional.
- The stop future starts concurrently with event draining. The ordinary drain is bounded, its
  final public-API ready sweep is separately bounded even if UI ingress is full, and the stop
  join is bounded and aborted on timeout. No new queue or T032 lifecycle surface was introduced.
- Added focused regressions covering every terminal cause, a leaked event sender during the
  ordinary bounded drain, and a nonblocking ready sweep while a sender remains alive. The test
  ingress factory is `cfg(test)` and preserves the production one-seam contract.
- Formatting succeeds, but the new tests did not reach a completed execution. The normal focused
  app test is blocked before Sotto code by generated Whisper bindings and the missing GPUI Metal
  Toolchain. A fresh runtime-shader/bundled-bindings run reached the Swift capture bridge, then
  stopped on the sandboxed Clang module cache; its escalated rerun was interrupted before a
  result. No green test or Clippy claim is made.

No further in-scope controller defect was found, but T022 remains `in-review` because its new
regressions have not completed a typecheck/test run. `crates/app/src/session/**` and
`crates/app/src/main.rs` are therefore **not yet released to T032**. Rerun the focused app tests
after T033/toolchain access and, if green, the reviewer may change T022 to `done` and record the
handoff without another implementation pass.

## Final review and T032 handoff (2026-08-11)

- Registered `session` once through the app library instead of compiling a second copy in the
  binary test target. This let the controller tests share the single diagnostic ingress without
  exposing a second production channel.
- The first clean-target run reached Sotto and found that one regression constructed
  `tokio::time::timeout` before entering its runtime. Moving that construction into the
  `block_on` future fixed the test rather than weakening it.
- With T033's declared bundled bindings and GPUI runtime shaders, a fresh target completed
  `cargo test -p app --locked --features gpui/runtime_shaders`: 21 passed, 0 failed; binary and
  doc tests also passed. The runtime-shader feature is a test-machine workaround for the missing
  local Metal compiler, not a product feature change.

The narrowed picker-to-pipeline controller and truthful bounded shutdown contract are accepted.
`crates/app/src/session/**` and `crates/app/src/main.rs` are released to T032.
