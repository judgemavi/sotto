# T013 — Intelligence loop: two-tier watcher/suggester with speculative execution

**Status:** blocked (on the Phase 2 note-taker gate: T016 + T017; plus T007, T008, T011)

**Wave:** 4 — Phase 3, the highest-judgement task in the project

**Depends on:** **the note-taker gate must pass first** — `AGENTS.md` now requires that
the fused timeline be accurate and readable before any advising work begins (T016, T017).
Then: T007 (providers) · T008 (RAG) · T011 (timeline + session state) · T015 (screen
context) · T017's findings on what timeline content a model actually uses well.

**Owns:** `crates/advisor/**`, `prompts/advisor/**`

> Moved out of `core` into its own crate per the `AGENTS.md` repo-structure change. This
> is a real improvement for parallelism: the advisor no longer edits `core` at all, so it
> is fully file-disjoint from T011. The crate is registered in the workspace root by
> T014's owner, not by you.

## Goal

The product. Everything upstream exists to feed this: decide *when* to speak, and *what*
to say. `AGENTS.md` is unambiguous that the hard part is restraint — "a copilot that
fires constantly gets closed" — so the watcher saying "no suggestion" is the common
case and the default behaviour, not a fallback.

You start with an advantage the earlier plan did not have: a working timeline, real
recorded sessions to replay through `sotto-cli replay`, and T017's evidence about which
timeline content helps a model and which is noise. Use it rather than re-deriving it.

## Plan

1. **Watcher tier.** A cheap/fast model classifying every partial: suggestion warranted,
   and if so which `TriggerKind`. Constraints: single call per partial, tight token
   budget, cached static prefix (T007's cache hints), structured output that is cheap
   to parse and hard to get wrong. It runs constantly on the user's own key — cost per
   call is a design constraint, not an afterthought.

2. **Bias toward the customer's turns.** Per `AGENTS.md`, suggestions fire during
   *customer* speech so the rep reads while listening; stay quiet during the rep's own
   turns. Implement this as a hard gate before the watcher call, not as prompt advice —
   it also cuts watcher spend roughly in half.

3. **Speculative execution.** On a promising partial, start the suggester call
   immediately with a tokio task and an abort handle. If the final transcript changes
   the meaning, abort and re-fire. Aggressiveness is a user setting (T012). Track and
   expose the wasted-token rate — the user is paying for speculation and deserves to
   see what it costs.

4. **Parallel retrieval.** RAG retrieval (T008) starts *concurrently with* the watcher
   classification, not after it, so battlecard context is already in hand when the
   trigger fires. This is the main structural trick for the ~1s budget. Retrieve
   against the last customer turn; discard the result if the watcher says no.

5. **Prompt assembly** (`prompts/advisor/`, versioned files not string literals in code,
   so prompts can be diffed and reviewed):
   - static cached prefix: company/product context, battlecard corpus summary, role
     instructions;
   - dynamic: the recent timeline window rendered via core's `Utterance::render_inline`
     with T006-selected annotations, **plus current screen context** (the OCR text and
     app metadata from the snapshot visible right now), retrieved chunks, and account
     context including what this account objected to on past calls (T008);
   - trigger-specific instructions per `TriggerKind`.
   Keep the static/dynamic boundary aligned exactly with the provider cache breakpoint —
   a misplaced boundary silently voids caching and multiplies cost.

   Screen context is a genuine edge here: the customer saying *"this is more than we
   budgeted"* while a pricing slide is on screen is a materially different situation from
   the same words with a contract on screen. Start from T017's findings on what actually
   helped rather than assuming all of it does.

6. **Trigger types v1** — competitor mention, pricing question, objection,
   discovery-gap. Each gets its own retrieval strategy and output shape. Discovery-gap
   is the different one: it fires on what the rep *hasn't* asked, so it needs session
   state (T011) rather than the current turn.

7. **Suggestions are timeline events, anchored.** Emit `SuggestionPartial` /
   `SuggestionFinal` carrying the `EventId`s that triggered them — `AGENTS.md`
   differentiator #6 is "the board, not a toast", and the anchor is what lets a card bud
   off the utterance that caused it so the rep sees *why* it exists. A suggestion without
   a valid anchor is a bug, not a degraded case.

   Supersession follows the append-only rule: a newer suggestion is a **new event with
   `supersedes` set**, never an edit. Define whether an in-flight suggestion is killed or
   allowed to finish and be consistent — text mutating under the rep's eyes mid-read is
   worse than a slightly stale card.

8. **Quiet by default, measurably.** Build an evaluation harness over recorded timelines
   (`sotto-cli replay`, T009): annotate where a suggestion *should* fire, then measure
   precision and recall — weighting false positives heavily, since they are what gets the
   app closed. Do not ship a watcher prompt without a measured false-positive rate.
   Replaying real recorded sessions rather than synthetic fixtures is the point of having
   done the note-taker milestone first — by now we have them.

9. **Latency measurement** end to end: customer pause → first suggestion token, against
   the ~1s budget. Report the breakdown per stage so regressions are attributable.

## Acceptance

- Recorded session with a competitor mention produces a grounded, cited suggestion within
  ~1s of the pause.
- Watcher false-positive rate measured and documented on the eval set.
- Speculative aborts verified to cancel in-flight provider calls (no token leak).
- Prompt caching verified working — cached-token counts reported from a live run.
- Suggestions cite the chunks they came from and anchor to valid trigger `EventId`s.
- Screen context measurably changes suggestion quality — or is documented as not worth
  its tokens, which is an equally useful finding for the prompt budget.

## Out of scope

MCP context (Phase 5), opt-in image context to LLMs (Phase 5), the board's suggestion
card rendering and the production overlay lens (Phase 4), open-objection tracking.
