# T019 — Topical clustering: the second organising axis

**Status:** todo (unblocked — crates/insight exists)

**Wave:** Phase 3 — the reasoning layer

**Depends on:** T017 (owns `crates/insight` and registers it; settles what timeline content
a model actually uses) · T008 (persisted timelines) · T007 (providers)

**Owns:** `crates/insight/src/clustering/**`, `prompts/clustering/**`

## Goal

This is what makes the board a *map* rather than a transcript. `AGENTS.md` draws the line
precisely: chronological structure is deterministic and local; **topical structure requires
meaning and is therefore the model's job**. Add a model and the board gains a second
organising axis — pricing discussion accumulates as a region, an objection links back to
the one it echoes from forty minutes earlier. Remove the model and the board is still
correct, just chronological.

That degradation path is the requirement, not a nicety. Everything you produce is a
**derived view**: stored alongside the timeline, recomputable from it, and switchable off.

## Plan

1. **Derived views never mutate the log.** Emit a separate artifact keyed by `EventId`s —
   clusters, links, labels — persisted in its own table, not as timeline events and never
   as edits. `AGENTS.md` is explicit: a model's opinion is not a fact about what happened,
   and the record has to survive being reinterpreted by a different model or none.
   Recomputing must be idempotent and must not disturb the original.

2. **Cluster utterances into topic regions.** A sales call has recognisable movements —
   discovery, demo, pricing, objections, next steps. Produce regions with a label, a span,
   and the `EventId`s they cover. Regions may be discontiguous: pricing comes up twice.
   Prefer few confident regions over many speculative ones; a board cluttered with weak
   clusters is worse than a plain chronological one.

3. **Link related moments across time.** "This objection echoes the one at 00:12." "This
   commitment answers that question." These cross-references are the highest-value thing
   here and the hardest to get right — they are also what a rep cannot see for themselves
   while talking. Every link carries both endpoints as `EventId`s and a short reason.

4. **Track open threads.** A question asked and never answered; an objection raised and
   never resolved. `AGENTS.md` wants unresolved objections to remain visually *open* on the
   board. Detecting non-resolution is harder than detecting the objection — be conservative
   and say "unresolved" only when nothing plausibly answers it.

5. **No latency budget, so use it.** This runs after the call, or on demand mid-call at the
   user's request — never speculatively on every partial. That freedom is why the screen
   context and prompt-shape questions get settled here first (T017), and why map-reduce over
   long sessions is affordable.

6. **Cost transparency and cheap recompute.** Report tokens per run via `Usage`. Cache by
   timeline content hash so re-opening a call does not re-spend. The user pays for this
   with their own key.

7. **Degrade honestly.** With no model configured, the board shows chronology and says so —
   not an empty "clusters" panel implying something failed. The absence of the reasoning
   tier is a normal state, not an error.

## Contract for downstream tasks

`insight::cluster(session_id) -> DerivedView` with regions, links and open threads, all
keyed by `EventId`. T016's board renders it as an optional overlay that can be toggled off,
falling back to pure chronology.

## Acceptance

- A real recorded call produces topic regions a participant recognises as correct.
- Cross-references are precise: no fabricated links, verified by reading them against the
  timeline.
- Re-running on an unchanged timeline is idempotent and cheap.
- The timeline is byte-identical before and after clustering — proven by test.
- With no provider configured, the board renders chronologically with no error state.

## Out of scope

Realtime suggestion (T013), editing the timeline, freeform user-drawn mind maps
(`AGENTS.md` non-goal: the board is conversation-generated, not a drawing tool).
