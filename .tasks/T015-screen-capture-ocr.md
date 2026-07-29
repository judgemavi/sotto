# T015 — Screen crate: low-rate frame sampling + Apple Vision OCR

**Status:** blocked (on T014; frame delivery from T002)

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

7. **Privacy is a product constraint here**, not a nicety. Screen capture sees password
   managers, Slack DMs, and email. Provide an exclusion mechanism — by app bundle id at
   minimum — and default to excluding nothing silently: whatever the policy, the user
   must be able to see and change it. Note what you implemented; the consent UI in
   Phase 4 builds on it.

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
