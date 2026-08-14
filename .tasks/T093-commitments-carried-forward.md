# T093 — Commitments carried forward

**Status:** blocked

**Wave:** B1 — preparation

**Depends on:** T087 (action items as checkable blocks are the durable form a commitment lands
in), T090 (the brief is where carried-forward items surface). Blocked until both close. The
classifier model download follows the Whisper provisioning precedent (T023 lineage): first-run
download, integrity check, progress, lazy load and idle unload.

**Owns:** the local utterance classifier as its own keyless crate or module below the
`providers` boundary (planner will pre-register the crate if a new workspace member is needed —
stop and report rather than editing the root manifest), its model provisioning, the open-items
view in `crates/insight/**`, its resurfacing hooks in the brief (sequential handoff from T090),
and this task

## Why this exists

Every competitor generates action items; none carries them forward. The loop that makes Sotto a
different product is: a commitment is spoken → it lands as a checkable block → unchecked items
resurface when the topic returns. T087 and T090 build the second and third steps for items the
*summary* finds after the fact. This task adds the first step at capture time and closes the
loop: commitments detected as they are spoken, cheaply, locally, with no key.

## The classifier, scoped deliberately small

One small ONNX encoder classifier (DistilBERT-class, int8, riding the `ort` runtime Silero
already ships — no new inference runtime, no generative model) over final utterances, detecting
exactly two classes to start: **commitment** ("I'll send that over by Friday") and **decision
marker** ("let's go with option B"). Question detection and anything watcher-shaped stays out —
that is T013's territory when it unblocks, and this task must not grow into it.

Heuristics run first and free (modal-verb and first-person-future patterns on finals); the
classifier confirms. Precision over recall throughout: a missed commitment is invisible, an
invented one erodes the exact trust this feature is meant to build.

## Plan

1. Emit detections as timeline annotation events on the final they classify — keyless-tier
   events, same family as prosody, never mutating the utterance. New event payloads are
   timeline-schema territory: write the short ADR entry.
2. Provision the model per the Whisper precedent; the classifier loads lazily, unloads idle, and
   its absence (download refused, first run offline) degrades to heuristics-only with a visible
   state — never a blocked pipeline, never silence that looks like success. At least one test
   runs real speech-derived text through the real model and asserts a known answer, per the
   verification rule.
3. In the composed document, a detected commitment seeds a *suggested* action-item block the
   summary run can confirm or the person can accept or dismiss — typed as derived suggestion
   until accepted, never silently a fact. Accepting makes it a user-layer block (T087 ops).
4. Build the open-items view in `insight`: unchecked task blocks across entries, each carrying
   its source citation, queryable by series and by recency. Deterministic — no model call.
5. Resurface: the T090 brief's deterministic section reads this view; an entry in a series shows
   its predecessors' open items. Checked items retire from resurfacing the moment the overlay op
   lands.
6. Evaluate on annotated fixture meetings: precision/recall recorded, precision weighted, and
   the numbers in `## Notes` rather than asserted adjectives.

## Acceptance

- A fixture meeting's spoken commitments produce annotation events, suggested blocks, and — once
  unaccepted/unchecked — appear in the open-items view and the next brief, cited to the moment
  they were spoken.
- Checking or dismissing retires an item everywhere, asserted by test.
- The classifier path is keyless: no file in `crates/providers/**` is touched and the pipeline
  runs with no backend configured.
- Model-absent degradation is heuristics-only and visibly stated; the real-model known-answer
  test passes; memory behaviour (lazy load, idle unload) is asserted or measured and recorded.
- Precision/recall on the fixture set is recorded in `## Notes`.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Question detection and the realtime watcher (T013), sentiment or emotion of any kind (see
AGENTS.md non-goals), entity extraction, owner/due-date inference beyond what the utterance
states, and any reasoning-backend involvement in detection.
