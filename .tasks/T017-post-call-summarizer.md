# T017 — Post-call summarizer: structured recap from the timeline

**Status:** blocked (on T007, T008, T011)

**Wave:** 3 — Phase 2, the note-taker dogfood gate

**Depends on:** T007 (BYOK providers) · T008 (persisted timelines) · T011 (timeline
production). Not blocked on any UI — this is a headless consumer and must stay one.

**Owns:** `crates/insight/**`, `prompts/summary/**`, root `Cargo.toml` (to register the
new crate only)

## Goal

The second half of the Phase 2 gate, and the first real consumer proving the timeline is
reasoning-grade. It is also strategically cheap: a good recap is most of the value a
note-taker product delivers, and we get it from the spine we already have.

Unlike the advisor, this runs **once, after the call, with no latency budget** — which
makes it the ideal place to learn what timeline content is actually useful to a model
before betting the real-time loop on it.

## Plan

1. **Headless consumer of a persisted timeline.** Input is a `session_id`; output is a
   structured recap. It must run from the CLI with no display — that keeps it testable
   in CI and honest about the headless-core rule.

2. **Prompt assembly from the timeline**, in versioned files under `prompts/summary/`
   (not string literals). Render final utterances through core's
   `Utterance::render_inline` so prosody annotations survive into the prompt, and fold in
   `ScreenSnapshot` OCR text at the point it was visible. **This is the first test of
   whether the fused audio+screen timeline actually reads well to a model** — report what
   helped and what was noise, because the advisor's prompt assembly inherits your
   findings directly.

3. **Structured output**, not prose: attendees and talk-time split, topics discussed,
   questions the customer asked, objections raised *and whether they were resolved*,
   commitments and next steps with owners, and competitor mentions. Objection resolution
   is the highest-value field and the hardest — it feeds Phase 4's open-objection
   tracking on the board.

4. **Long calls exceed context.** A two-hour call will not fit. Implement map-reduce:
   summarise per topic-window, then reduce. Keep the windowing rule simple and
   documented; a clever scheme that no one can debug is worse than a plain one.

5. **Cite the timeline.** Every claim in the recap references the `EventId`s it came
   from, so the board can later link a recap line back to the moment in the conversation.
   This is the same citation discipline the advisor uses and is far easier to build now
   than to retrofit.

6. **Cost transparency.** Report tokens and cost per summary using T007's `Usage`. Users
   pay for this with their own key — the number must be visible, not buried.

7. Tests over recorded fixture timelines from T009 with hand-written expected recaps.
   Assert structure and citation validity mechanically; judge quality by reading. Mock
   provider responses so CI needs no API key.

## Contract for downstream tasks

`summarizer::summarize(session_id) -> Recap` with `Recap` carrying `EventId` citations.
Phase 4's board review UI renders it; T008 ingests it into RAG as account memory.

## Acceptance

- A real recorded call produces an accurate structured recap, verified by someone who was
  on the call.
- Every recap claim carries valid `EventId` citations.
- A two-hour timeline summarises without exceeding context.
- Runs headless from the CLI; full test suite passes with no API keys.
- Findings on which timeline content helped versus added noise written up for T013.

## Out of scope

Live/incremental summarisation, the advisor loop (T013), recap UI (Phase 4), sharing or
exporting recaps anywhere off-device.


## Amendment — moved out of `core` (2026-07-29)

This task previously owned `crates/core/src/summarizer/**`. That is now wrong and would
have broken the architecture: the summarizer calls an LLM through `providers`, so putting
it in `core` makes `core` depend on `providers` — and `AGENTS.md` now states that the map
tier must work with **no API key at all**, with the crate graph as the thing that enforces
it. Reasoning code in `core` would quietly make the no-key tier a fiction.

Build it in a new **`crates/insight`** crate — the offline LLM path, as distinct from
`advisor`'s realtime path. You own registering it in the workspace root; nobody else
touches that file while you do.

`insight` is also where the **screen-context question gets settled**, because it is the one
place with no latency budget and no prompt-cache concern. Run the same recorded session
three ways and compare the recaps:

1. capture-target metadata only (app name, window title — nearly free);
2. metadata + OCR text from `screen.snapshot`;
3. metadata + the frame images themselves, sent to a multimodal model.

Report which actually changed the recap. Right now nothing has measured whether screen
context earns its tokens in any form, and T015's OCR has never been verified to extract a
character. That measurement decides three things downstream: whether `advisor` includes
screen context at all, whether we keep maintaining Vision, and whether image context is
worth its cost and its privacy trade-off. Do not assume — `AGENTS.md` keeps images opt-in
precisely because sending a customer's shared screen off-device is a different promise from
sending redacted transcript text.
