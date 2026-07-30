# T022 — Live session: picker to transcript

**Status:** open — this is the critical path

**Wave:** Phase 2 — the first end-to-end use of the product

**Depends on:** T021 (`PickedMacCapture` implements `CaptureBackend`) · T011 (the pipeline) ·
T012 (the timeline seam, `attach_ingress`)

**Owns:** `crates/app/src/session/**` (new), `crates/app/src/main.rs`

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
