# T015 — Screen crate: low-rate frame sampling + Apple Vision OCR

**Status:** done

**Wave:** 1 — new crate, parallel with the other stage crates

**Depends on:** T014 (`ScreenSnapshot`, `TimelineEvent`) · T002 (the ScreenCaptureKit
session that produces frames — but develop against PNG fixtures, not live capture)

**Owns:** `crates/screen/**`

## Goal

`AGENTS.md` promoted screen context from "later phase" to Phase 1 and made it a first
class timeline producer. What the customer is *looking at* — the pricing slide, the
competitor's logo in a comparison deck, the contract clause on screen — is context the
audio never carries. OCR text flows into the timeline by default; sending actual images
to an LLM stays opt-in per user on cost grounds.

## Plan

1. New crate, registered in the workspace root by **T014's owner**, not by you — the
   root `Cargo.toml` stays single-owner. Coordinate before starting.

2. **Frame intake.** Consume frames from the same ScreenCaptureKit session T002 owns —
   do not open a second session (double permission prompts, double battery cost).
   T002 exposes the frame callback; you own everything after it. Sample at 0.1–0.2 fps
   *or on significant change*, whichever is less frequent.

   Frames arrive already scoped to the user's chosen target, so "active app" is largely
   known up front from the session record rather than inferred per frame. Window title
   still changes within a target (a slide name, a document name) and is worth capturing.

3. **Change detection.** A slide that sits still for four minutes must produce one
   snapshot with a long `visible_from..visible_to` interval, not 48 identical ones. Use a
   cheap perceptual difference (downscaled luma hash) before spending OCR on a frame.
   Tune the threshold so a mouse cursor moving does not count as a change but a slide
   transition does — and note the threshold you chose, since it directly sets both OCR
   cost and timeline noise.

4. **OCR via Apple Vision** (`VNRecognizeTextRequest`) through a small Swift/objc2
   bridge. Prefer extending T002's existing bridge package over creating a second FFI
   surface; agree the boundary with T002 before writing it. Run OCR off the capture
   thread — Vision is not free and must never stall frame delivery.

5. **Frame storage and pruning.** `AGENTS.md` is explicit: *"Screen frames are sampled,
   referenced, and pruned — never accumulate raw video."* Write frames to a
   content-addressed cache under app-support, emit a `FrameRef` into the timeline, and
   implement a retention policy (cap per session, prune oldest, drop on session delete).
   A two-hour call must have a bounded on-disk footprint — state the bound.

6. **Active-app metadata.** Capture the frontmost app and window title alongside each
   snapshot (`"Zoom fullscreen"`, `"Keynote — Pricing.key"`, a slide-changed marker).
   This is often more useful per byte than the OCR text itself, and it is nearly free.

7. **Scope replaces the exclusion list — this task got smaller.** `AGENTS.md` now makes
   capture scope structural: the user picks an application or window at session start and
   the OS content filter enforces it. Do **not** build a blocklist of apps to exclude. The
   password manager was never in frame, so there is nothing to filter, redact or prune.

   What you own instead is honesty about the scope you were given. Record the target on
   every snapshot, and if T002 reports that audio cannot be scoped per-application, make
   sure that asymmetry is visible in the data rather than papered over — the UI has to be
   able to tell the user that screen is scoped but audio may not be.

8. Tests against committed PNG fixtures (a slide deck sequence, a static screen, a
   scrolling document): assert change detection collapses static runs, OCR extracts
   expected strings, and snapshots carry correct `visible_from`/`visible_to` intervals.
   Bench OCR cost per frame — it shares a laptop with Whisper and Zoom.

## Contract for downstream tasks

`screen::ScreenSampler` emitting `EventPayload::ScreenSnapshot`. T016's board pins
thumbnails by `FrameRef`; T017's summarizer and the advisor read `ocr_text`.

## Acceptance

- A static screen over five minutes produces one snapshot, not dozens.
- OCR text matches expectation on fixture decks.
- Bounded on-disk frame footprint over a simulated two-hour session, with pruning shown
  to work.
- Runs without stalling audio capture — verify against T002's soak.

## Out of scope

Sending images to LLMs (Phase 5, opt-in), video recording, screen *sharing* detection,
the consent UI.

## Review round 1 — approved, with OCR carried as unverified

Solid work, and the collision I flagged was resolved better than either option I proposed.

**`crates/capture` and `bridge-macos` are untouched.** Rather than extending T002's Swift
package as the brief suggested, this consumes the existing `RawFrame` boundary and binds
Vision through `objc2-vision` directly. That is strictly better: no second Swift package, no
extra build step, no shared ownership to negotiate, and maintained Rust bindings instead of
hand-written FFI. Deviating from the brief was the right call — note it in the crate docs so
the next reader knows it was deliberate.

Also right: five-second sampling with perceptual change detection, stable
`visible_from`/`visible_to` intervals, SHA-256 content addressing, and a 128 MiB per-session
cap with pruning — which is exactly the *"sampled, referenced, and pruned — never accumulate
raw video"* discipline `AGENTS.md` requires. Static-screen, slide-change, interval and
retention tests all present. Scope replaced the exclusion list as intended.

### Open: OCR has never extracted a character

You flagged this honestly, which is the right instinct. Recording precisely where it stands,
because the summary could be read as more settled than it is:

- Running the pipeline against a text-bearing PNG returns `"ocr_text": ""`.
- **A plain Swift `VNRecognizeTextRequest` on the same file also returns zero
  observations.** So the failure is at the Vision level with these images, not demonstrably
  in your binding.
- Therefore the binding is **unproven, not broken.** It compiles, it is wired correctly, and
  it has never successfully read text.

Do not treat "production Vision compiles" as evidence it works. This is the same shape as
T002's 2×2 frames and T009's tone-burst audio: a green suite over an artifact that exercises
nothing.

Settling it is cheap and belongs with T009, which owns the fixture corpus — it needs to
produce a frame whose text Vision demonstrably reads, verified before committing. Once that
lands, add an assertion here that expected strings come back.

### And it may not matter

Worth knowing where this is heading: **T017 now owns deciding whether screen context earns
its place at all**, by running the same session three ways — capture-target metadata only,
metadata + OCR text, metadata + images to a multimodal model — and reporting which actually
changed the recap. Window title and app name are nearly free and may carry most of the
signal.

So do not invest further in OCR quality or tuning until that measurement exists. If metadata
alone proves sufficient, the right outcome is to stop maintaining Vision and keep the frame
capture, which every option needs anyway.
