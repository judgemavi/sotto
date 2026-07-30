# T012 — GPUI dev window: live timeline + settings screens

**Status:** in-progress (timeline, filters, follow and key handling done and reviewed; start/stop
blocked on T021; device enumeration and the manual runs outstanding)

**Wave:** 2

**Depends on:** T003 (GPUI verdict and pinned version — **do not start until the spike
passes**; if it fails, this task is rewritten against the Electron fallback) ·
T011 (timeline events) · T007 (provider registry, for the settings screens)

**Owns:** `crates/app/src/devwindow/**`, `crates/app/src/settings/**`

## Goal

The first real GPUI code beyond the spike (`AGENTS.md` Phase 1): a dev window rendering
the raw live timeline, plus the settings screens for keys and model selection. Both are
grouped here because they share the tokio↔GPUI seam and the gpui-component widget set —
splitting them would mean two agents solving the same bridging problem.

This is deliberately **not** the board. It is the debugging view — a flat, honest,
scrolling dump of timeline events as they arrive, which is what you want when diagnosing
why the board looks wrong. T016 builds the spatial canvas on top of the same seam.

Settings ship here rather than later because Phase 2's summarizer (T017) is BYOK and
needs keys configurable before the note-taker gate can be dogfooded at all.

## Plan

1. **The seam.** Exactly one place where timeline events cross into GPUI entities, built
   the way T003's ADR prescribes. Everything else in the UI reads GPUI state. Keep it
   in one module and document it — `AGENTS.md` calls for a single well-defined seam and
   this is where that promise is kept or lost. **T016's board consumes this same seam**,
   so treat its shape as an interface, not an internal detail.

2. **Timeline view.** A flat chronological list of every event kind — utterances per
   speaker with inline prosody, VAD transitions, screen snapshots, errors. Show `id`,
   `ts` and `supersedes` so append-only behaviour is directly observable; a partial being
   superseded should be visibly a *new event referencing an old one*, not a mutation.
   Include a kind filter. Auto-scroll that yields when the user scrolls back.

3. **Performance.** Partials arrive several times a second for a call lasting an hour.
   Virtualise the list; do not re-render the whole transcript per event. Measure frame
   time with a long synthetic transcript before calling this done.

4. **Settings screens** with `gpui-component` at the version pinned by T003:
   - Provider/key management — add, validate (a real cheap test call), and delete keys
     per provider. Keys go to the keychain via T007; the UI never persists them itself
     and never renders them back after entry.
    - Model selection per role (watcher, suggester, summarizer), including the Ollama
     fully-local path with no key at all.
   - Audio device selection and a level meter per stream.
   - Session start: the target picker flow. Starting a session is turn-on-then-pick, using
     `SCContentSharingPicker` rather than a chooser of our own. Stopping is always one
     obvious action away, and the app never resumes a session by itself. There is no
     app-exclusion list any more — scope replaced it.
   - Speculation aggressiveness — `AGENTS.md` makes this a user setting because it is
     their tokens and their tradeoff. Present the cost implication honestly in the UI.

5. **Error surfacing.** Bad key, rate limit, revoked capture permission, model not
   downloaded — each with a clear message and an action. Distinguish *your key is bad*
   from *the network is down*, using T007's error taxonomy.

6. **Recording indicator — show *what*, not just *that*.** A visible, always-present
   indicator whenever capture is live, naming the captured target and distinguishing audio
   from screen. A user who knows their audio is recorded may not realise their screen is,
   and a user who picked one window should be able to confirm at a glance that it is still
   the only thing being captured. This is a consent feature and a core differentiator, not
   decoration: it cannot be hidden, and no setting may disable it.

   If T002 reports that audio cannot be scoped per-application, the indicator must say so
   — "screen: Zoom · audio: system" is honest; implying both are scoped is not.

## Acceptance

- Live timeline from a fixture run renders smoothly, with supersessions visibly arriving
  as new events rather than in-place edits.
- Frame time stable over a one-hour synthetic session.
- Keys round-trip through the keychain and never render back.
- Recording indicator provably visible whenever capture is active, naming the target and
  distinguishing audio from screen — and truthful about which of them is actually scoped.
- The seam is documented well enough for T016 to build the board on it without changes.

## Out of scope

The board canvas (T016), the production overlay lens (Phase 4), suggestion rendering
(T013), tray/menubar, auto-updater.

## Notes — implementation pass 1 (2026-07-29)

Added the single bounded `TimelineEvent` ingress under `app::devwindow`; it batches into one
shared GPUI `Entity<TimelineState>`, which is the interface T016 should consume. The diagnostic
window uses a virtualised list and renders ids, timestamps, kinds, payload detail, and
`supersedes` without mutating prior rows. The executable feeds it synthetic partial/final pairs
so supersession is visible.

Added settings state for write-only keychain operations, independent role/model choices,
audio-device fields, speculation level, a truthful capture-scope indicator, and a
`gpui-component` settings shell. The UI actions for entering/validating keys, selecting devices,
opening the system picker, stop capture, filtering, and yielding auto-scroll are not wired yet.
The one-hour frame run and real keychain round-trip also remain. This task stays in progress.

## Interim review — the seam is the part that matters, and it looks right

Not approving yet, given the outstanding list. Two notes so they land before the remaining work.

**One bounded seam, as required.** A single `tokio::sync::mpsc` receiver drained on the GPUI
foreground every 16 ms through `AsyncApp::update` is exactly what `AGENTS.md` asks for and what
ADR-0003 prescribes. Keep it that way: T016's board consumes this same seam, so its shape is an
interface, not an internal detail. Any second path from tokio into GPUI is the thing to refuse.

**The diagnostic view showing ids, timestamps, kinds, payloads and supersessions** is the right
call. It makes append-only behaviour directly observable — a partial being superseded reads as a
*new event referencing an old one* rather than as an edit, which is precisely what someone
debugging a wrong board needs to see.

Two things for the remaining work:

- **The one-hour performance validation should reuse T020's non-cumulative reporting.** T020
  fixed the harness to report frame intervals at 1/10/20/30 minutes specifically so accumulation
  drift cannot hide in an average. Report the same way here rather than a single figure — and
  note that T016 now carries the 30-minute board measurement as an acceptance criterion, so
  matching the format lets the two be compared.
- **Key validation touches secrets.** T007 keeps credentials in `SecretString` end to end and
  keychain failures report as `CredentialStore` rather than `Network` — so surface *that*
  distinction in the UI. "Your key is wrong" and "the keychain is locked" and "the network is
  down" are three different messages, and the taxonomy exists to make them distinguishable.
  Validate with a real cheap call, never render a key back after entry.

## Notes — implementation pass 2 (2026-07-30)

The dev timeline now has interactive kind filters and a follow control. Any user scroll pauses
auto-scroll; choosing a filter or pressing the follow control resumes at the newest matching
event. The virtualised list still consumes the original shared `TimelineState`; no second
timeline ingress was introduced.

The T012 harness now emits non-cumulative `T012_FRAME` intervals at 1/10/20/30 minutes, clearing
the sample set after each report so results are directly comparable to T016/T020. Instrumentation
is present, but the one-hour manual run has **not** been performed in this pass; no frame-time
figures are claimed.

Settings now provide a masked, write-only key field which is erased immediately after keychain
storage, provider selection, deletion, and a real one-token validation call. Validation preserves
T007's actionable distinction between rejected credentials, a locked/unavailable credential
store, and an unreachable network (with rate limiting separate as well). Speculation level is an
interactive cost choice.

The scoped picker/start-stop flow remains blocked on the capture-side interface: the current
macOS bridge exposes neither `SCContentSharingPicker` selection nor a selected target to the app,
and its start path constructs an unscoped display filter. Wiring that path here would falsely
present whole-display capture as user-scoped capture. Audio-device enumeration/selection and the
manual real-key/keychain round-trip also remain validation gates.

## Review of pass 2 — the refusal was right; four things to fix

Stopping at the capture boundary rather than wiring "Turn on and choose target…" to
`SCContentFilter(display:…)` is the single best decision in this pass. I verified it:
`CaptureBridge.swift:73` builds a whole-display filter, `sotto_capture_start` takes no target,
and nothing in `crates/capture` can express one. A start button on that path would have shipped
unscoped capture behind a label promising the opposite. **T021 now owns that interface** and is
the critical path; come back for start/stop when it lands.

### 1. `select_default_audio` does the thing you refused to do everywhere else

```rust
self.state.mic_device = Some("System default microphone".to_owned());
self.state.target_audio_device = Some("Selected target audio".to_owned());
```

Those are labels, not devices. Nothing was enumerated and nothing is bound, but the state now
says a device is selected, and any indicator reading that state will report a selection that
does not exist. This is the same class of untruth as the unscoped filter — a UI asserting a
scope it does not have. Leave the control absent or disabled until enumeration exists. A missing
button is honest; a button that writes a fake device name is not.

### 2. "No key stored" is being reported as "This key was rejected"

Two paths reach `BadKey` without a provider ever rejecting anything: an empty input in
`save_input_key`, and `Ok(None)` from `load_key` in `validate_provider_key`. The user sees *"This
key was rejected. Replace it and try again"* when in fact they have not entered one. That defeats
the point of the taxonomy — the reason T007 distinguishes these is so the message tells you what
to actually do. Add a `NoKey` variant: "No key stored for this provider."

While there: `CredentialStore(_) => "Keychain is locked. Unlock it and try again."` asserts a
cause we did not observe. That variant also covers denied access and missing items. Say "Could
not read the keychain: {reason}" — still distinct from a bad key and from a network failure,
without naming a cause we are guessing at.

### 3. A dead validation thread locks the button out for the session

`validate_selected_key` polls `receiver.try_recv()` and only exits on `Ok`. If the worker thread
ever dies without sending, `try_recv` returns `Disconnected` forever: the task spins at 50 ms for
the life of the process, the status stays `Validating`, and the `if self.key_status ==
Validating { return; }` guard means the user can never retry. One match arm on
`TryRecvError::Disconnected` fixes it. The 50 ms poll itself is fine — it is the same idiom as
the seam.

### 4. The instrumentation stops halfway through the run it was built for

Acceptance here is *frame time stable over a one-hour synthetic session*, but `REPORT_SECONDS`
ends at 1800. Minutes 30–60 produce no report at all, while `record_frame_interval` keeps
pushing into `frame_intervals` — the push happens before the `report_index` exhaustion early
return, so the vector grows unbounded for the rest of the process's life (~110k samples over
that second half). Add a 3600 point, and stop pushing once the schedule is exhausted.

Separately: `window.request_animation_frame()` is unconditional in `DevTimeline::render`. In
T020's `Lens` that was a benchmark harness; here it is the window someone actually runs, so the
dev window now redraws 60 times a second forever whether or not events arrive. That makes the
idle-CPU figure meaningless and costs real battery for a measurement nobody is taking. Gate the
raf and the instrumentation behind an env var — `SOTTO_FRAME_VALIDATION=1` — so the measured run
is deliberate and the normal window is idle when idle.

### What the frame numbers can and cannot show

Worth naming before the one-hour run, because it changes how the result is read. This measures
the interval *between renders* under raf, not the cost of a frame. T020's headline p50 of
8.332 ms is 1/120 s — it is the ProMotion refresh period, not frame cost, and p50 will keep
reading the refresh period no matter how expensive rendering gets until cost exceeds the frame
budget. So the drift signal is p95 and any p50 rising off the refresh period; a frame getting
three times more expensive inside the budget is invisible. Report a max and a count of intervals
over budget per window alongside p50/p95 and the metric detects what T016 needs it to detect.

### Accepted as-is

Filters and the follow control read correctly — any user scroll pauses following, choosing a
filter or pressing follow resumes at the newest matching event, and the virtualised list still
consumes the one shared `TimelineState`. No second ingress was introduced; the seam T016 inherits
is unchanged. The masked write-only key field cleared immediately after storage, never loaded
back, with a real one-token validation call, is exactly right.

One correction: `default_model` returns `claude-3-5-haiku-latest` for Anthropic. Use
`claude-haiku-4-5-20251001`. And note that validation always calls the provider's default model
rather than the model selected for the role — so a key that lacks access to the selected model
validates green and fails in real use. Either validate the selected model, or say in the UI that
this checks the credential only.

## Review of the four fixes — all four accepted

`select_default_audio` is gone rather than papered over. `NoKey` now covers both the empty-input
and `load_key` → `Ok(None)` paths. `CredentialStore(String)` carries the reason instead of
asserting a lock, and `credential_store_label_does_not_guess_the_cause` pins that. The
`Disconnected` arm ends the poll and restores the button. Frame validation is opt-in behind
`SOTTO_FRAME_VALIDATION=1`, the schedule reaches 3600, and the exhaustion check now precedes the
push so the vector stops growing. `max_ms` and `over_budget` are in the output, which is what
makes the number readable given p50 pins to the refresh period.

Remaining here is unchanged: start/stop (blocked on T021), audio device enumeration, and the
one-hour and real-keychain manual runs.
