# T091 — Ask about exactly this

**Status:** done

**Wave:** A2 — ask surface

**Depends on:** T077 (`in-review`) — the selection and anchor-range gestures this task hands to
Ask — and T069's close for `crates/app/src/workspace/ask.rs`. Blocked until both close.

**Owns:** the selection-scope plumbing between `crates/app/src/workspace/transcript.rs` and
`ask.rs` (sequential handoff from T077/T069), the span-scoped ask path in the `crates/insight`
ask module, and this task

## Why this exists

Mid-call, the useful question is almost never about the whole library or even the whole
recording — it is about the thing that was just said. "What did she mean by the Q3 number?"
wants the last two minutes, not five retrieved excerpts. T077 built exactly the units a person
would point at: a selected row, and the anchor-through-row range. Handing that span to Ask turns
the live-ask experience from a demo into something used during calls — user-initiated, bounded,
and honest about what the model saw, which is the entire ADR-0017/ADR-0020 posture applied to
one more scope.

## Plan

1. **The gesture:** with a row anchored or a shift-click range made (T077's existing gestures),
   the Ask dock offers "Ask about this selection" alongside its two existing scopes. The scope
   line names the span the way T077's copied ranges do — timecodes and sources — so the person
   sees exactly what will be sent before asking.
2. **Scope semantics per ADR-0020:** selection scope is a narrowing the person performs; it is
   never the default, never sticky across questions unless the selection still stands, and
   clearing the selection returns the control to its previous scope. On a live recording the
   span is labelled *so far*-style if it includes the provisional tail — or excludes the tail
   and says so; decide and state which.
3. **The request:** the span's finals with their stream labels and prosody qualifiers, verbatim,
   plus the existing minimal context — no RAG retrieval in selection scope (the person chose the
   evidence; honoring the choice is the feature). The span is bounded by construction; enforce a
   sane ceiling with an honest refusal for a thousand-row selection.
4. **Citations still bind:** answers cite `EventId`s within the span through the unchanged answer
   shape; a question the span cannot answer gets the existing refusal rather than silent scope
   widening. Never fall back to library scope on a refusal — offer it, do not perform it.
5. Prove the boundary: a question whose answer exists in the recording but *outside* the selected
   span produces a refusal in selection scope and an answer in recording scope, asserted by test.

## Acceptance

- A selection made with T077's gestures can be asked about; the scope control shows the span
  before the question is sent.
- The serialized request contains the span's finals and nothing retrieved from outside it,
  asserted over the serialized request.
- The outside-the-span question refuses in selection scope and answers in recording scope.
- Scope never widens silently; clearing the selection restores the prior scope visibly.
- Works during a live recording and on a completed one; the provisional-tail decision is stated
  and tested.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

New answer shapes, proposals, screen evidence in selection scope (T092 owns inspection reach),
multi-span selection, and any change to T077's selection mechanics.

## Closed — 2026-08-16

Implemented and committed in `2b6e10f`; the ownership override it waited on is resolved now that
T077 and T069 have both closed. Its selection scope is also what made T077's invisible-range defect
visible enough to fix: Ask was being scoped to a span of transcript the reader could not see, which
is recorded in T077 and fixed in `175efc7`.
