# T013 — Intelligence loop: two-tier watcher/suggester with speculative execution

**Status:** blocked (on T007, T008, T011)

**Wave:** 2 — the highest-judgement task in the project

**Depends on:** T007 (providers) · T008 (RAG) · T011 (pipeline + session state) ·
T006 (annotations, via T011)

**Owns:** `crates/core/src/intelligence/**`, `prompts/**`, `crates/core/tests/intelligence/**`

> Same constraint as T011: may add modules under `crates/core/src/intelligence/`, must
> not change the frozen `types.rs` / `traits.rs`.

## Goal

The product. Everything upstream exists to feed this: decide *when* to speak, and *what*
to say. `AGENTS.md` is unambiguous that the hard part is restraint — "a copilot that
fires constantly gets closed" — so the watcher saying "no suggestion" is the common
case and the default behaviour, not a fallback.

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

5. **Prompt assembly** (`prompts/`, versioned files not string literals in code, so
   prompts can be diffed and reviewed):
   - static cached prefix: company/product context, battlecard corpus summary, role
     instructions;
   - dynamic: recent annotated conversation from `prosody::render_turn`, retrieved
     chunks, account context;
   - trigger-specific instructions per `TriggerKind`.
   Keep the static/dynamic boundary aligned exactly with the provider cache breakpoint —
   a misplaced boundary silently voids caching and multiplies cost.

6. **Trigger types v1** — competitor mention, pricing question, objection,
   discovery-gap. Each gets its own retrieval strategy and output shape. Discovery-gap
   is the different one: it fires on what the rep *hasn't* asked, so it needs session
   state (T011) rather than the current turn.

7. **Suggestion streaming and supersession.** Stream tokens to the UI as they arrive.
   A newer suggestion supersedes an older one that is still rendering; define whether
   an in-flight suggestion is killed or allowed to finish, and make it consistent —
   text mutating under the rep's eyes mid-read is worse than a slightly stale card.

8. **Quiet by default, measurably.** Build an evaluation harness over the T009
   fixtures: annotate where a suggestion *should* fire, then measure precision and
   recall — weighting false positives heavily, since they are what gets the app closed.
   Do not ship a watcher prompt without a measured false-positive rate.

9. **Latency measurement** end to end: customer pause → first suggestion token, against
   the ~1s budget. Report the breakdown per stage so regressions are attributable.

## Acceptance

- Fixture with a competitor mention produces a grounded, cited suggestion within ~1s of
  the pause.
- Watcher false-positive rate measured and documented on the eval set.
- Speculative aborts verified to cancel in-flight provider calls (no token leak).
- Prompt caching verified working — cached-token counts reported from a live run.
- Suggestions cite the chunks they came from.

## Out of scope

MCP context (Phase 4), screen/OCR context (Phase 4), production overlay UI (Phase 3),
suggestion history persistence.
