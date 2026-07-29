# T006 — Prosody crate: annotation extraction

**Status:** todo (unblocked — T001 approved)

**Wave:** 1 — fully parallel, pure computation over timings + text

**Depends on:** T001 (`Annotation`, `VadSegment`, `Utterance`)

**Owns:** `crates/prosody/**`

## Goal

`AGENTS.md` is explicit that text is the LLM payload, not audio — so everything the
model would have heard has to be recovered as inline annotations. This crate turns
timing and audio statistics into the `[customer, hesitant, 2.5s pause]` form. It is
what makes a text-only pipeline competitive with audio-native APIs at a fraction of the
cost, and it is cheap to build: no ML, just measurement.

## Plan

1. Pure-Rust crate, no ML dependencies. Input is `VadSegment`s, `Utterance`s and
   optionally raw frames for energy/pitch; output is `Vec<Annotation>` attached to
   utterances. Keep it a pure function of its inputs where possible — that makes it
   trivially testable and side-effect free in the pipeline.

2. **Pause detection.** Gap between speech end and next speech start, within and across
   speakers. Only annotate pauses past a threshold (~700 ms default) — annotating every
   micro-gap floods the prompt with noise and burns tokens. Cross-speaker gaps
   (customer stops, rep starts) and within-speaker gaps (customer hesitating
   mid-thought) mean different things; label them distinctly.

3. **Interruption detection.** Overlapping speech across the two streams, attributed to
   whoever started talking while the other still held the floor. Requires
   cross-stream timestamp comparability — read T002's measured drift figure and, if
   drift is material, apply the correction before comparing. Guard against acoustic
   echo: the customer's audio leaking into the mic is not an interruption. Note the
   guard you chose.

4. **Speech rate.** Words (or syllable proxies) per minute over a rolling window, and
   rate *change* relative to the speaker's own baseline. Absolute WPM says little;
   "this customer just sped up 40% over their own norm" is the signal worth spending
   tokens on. Maintain a per-`Source` baseline that adapts over the call.

5. **Talk-time ratio.** Rolling rep-vs-customer speaking ratio over the call and over a
   recent window. This is the classic sales-coaching metric and doubles as UI data
   later — expose it as a queryable value, not only as an annotation.

6. **Hesitancy / emphasis.** Derive from what is already available before adding signal
   processing: filler words and false starts from the text, and short-pause density
   within an utterance. Only reach for energy/pitch from raw frames if the text-derived
   signal proves too weak. Prefer being under-eager — a wrong `[hesitant]` actively
   misleads the suggester model, which is worse than no annotation.

7. **Rendering — you do not own the renderer.** T001 shipped both
   `Annotation::render_inline` (one fragment) and `Utterance::render_inline` (the whole
   `[customer, hesitant, 2.5s pause] "sure, sounds fine"` line) in `core`, and that is
   the canonical format. Do **not** write a second renderer here.

   What this crate owns is *selection*: which annotations are worth attaching, in what
   order, and a token-budget mode that drops the low-salience ones first for the Phase 2
   prompt assembly. Emit the chosen `Vec<Annotation>`; let `core` render it.

8. Tests: hand-built `VadSegment`/`Utterance` sequences with expected annotations.
   Include adversarial cases — rapid back-and-forth, one speaker monologuing,
   simultaneous starts, echo leakage.

## Contract for downstream tasks

`prosody::Annotator::observe(&mut self, event) -> Vec<Annotation>` plus a
budget-aware `select(&[Annotation], budget) -> Vec<Annotation>`. Prompt assembly in
Phase 2 attaches the result to an `Utterance` and calls `core`'s
`Utterance::render_inline`. What you select is what the suggester model sees, so treat
selection changes as prompt changes.

## Acceptance

- Annotations match expectations on all fixtures.
- No false interruptions on echo-leakage fixtures.
- Selected annotations rendered through `core`'s `Utterance::render_inline` match the
  `AGENTS.md` format exactly — no renderer of your own.
- Deterministic: same inputs → same annotations.

## Out of scope

Emotion classification, ML-based prosody models, anything requiring a network call.
