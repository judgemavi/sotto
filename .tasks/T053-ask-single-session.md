# T053 — Ask: grounded questions over one session

**Status:** done

**Wave:** A0 — ask surface

**Depends on:** T049 for `workspace/ask.rs`. T024, T026, T031 and T034 are accepted history.

**Owns:** an ask module under `crates/insight/src/**`, `crates/app/src/workspace/ask.rs`,
`docs/adr/0017-ask-surface.md`, and this task

## Why this exists

Notes answer "what happened". They do not answer "what did I commit to", asked twenty minutes later
in the user's own words. That question is the difference between a record and a copilot, and it is
the one interaction users reach for first.

No ADR covers it. ADR-0011 fixed notes as the minimum AI product and proposals as the optional
third; ADR-0015 defined the shipped workspace as transcript plus notes and explicitly excluded other
surfaces. A conversational surface is neither. It needs its own decision record before it is built,
not after.

## The rules it inherits

None of these are negotiable, and the ADR must restate them:

- **Model output, never meeting fact.** An answer is derived, recomputable, and typed as system
  output. It never enters the timeline as captured fact. If a run is audited to the timeline, it is
  audited the way proposals are (ADR-0012), not appended as speech.
- **Every claim cites.** Answers cite `EventId`s in the session record, and selecting a citation
  focuses that transcript row — the same focus path notes use. An answer that cannot cite is a
  refusal, not a paragraph.
- **Transcript text only.** The request carries timestamped, stream-labelled transcript with prosody
  annotations, per ADR-0009 and the ADR-0010 request shape. No audio. No eager screen inspection; a
  screen frame enters only through the existing on-demand inspection path and its separate opt-in.
- **Degrades, does not gate.** With no backend configured the panel renders disabled with an honest
  reason. The transcript and the record stay fully usable. `core` still never depends on
  `providers`.

## Plan

1. Write `docs/adr/0017-ask-surface.md`: what the surface is, why it is not a proposal and not a
   note, the rules above, the refusal contract, and the multi-turn state boundary.
2. Build the ask path in `crates/insight`, reusing the resolved-backend selection, cache identity,
   and request transport already accepted in T024/T026/T031/T034. Do not add a second reasoning
   client.
3. Multi-turn within one session: prior turns are conversational context only. The session record is
   the sole source of claims. A later turn may not treat an earlier answer as evidence.
4. Refusal is a feature. When the record does not contain the answer, the reply says so and names
   what the session does cover. Prove it with an eval in the T029 harness style: a question whose
   answer is absent must produce a refusal, not a plausible invention.
5. Wire `workspace/ask.rs`: collapsed by default, question input, streaming answer, citation chips
   that focus transcript rows, and a visible scope label naming the session being asked about.
6. Cancellation and cost: an in-flight question is cancellable, and the panel never issues a request
   the user did not initiate. This surface is quiet by construction — it only ever speaks when asked.

## Contract

- T054 extends this surface to multiple sessions. It takes over `workspace/ask.rs` by sequential
  handoff after this task is accepted, and reuses this task's answer, citation, and refusal shapes
  unchanged.

## Acceptance

- A question against a completed session returns an answer whose every factual claim carries a
  citation, and each citation focuses the correct transcript row.
- A question whose answer is absent from the record produces an explicit refusal naming what the
  session does cover, verified by eval rather than by inspection.
- With no reasoning backend configured, the panel is disabled with an honest reason and no request
  is issued.
- No ask output is appended to the timeline as a captured meeting fact.
- The request payload contains transcript text, labels and prosody only — asserted by a test over
  the serialized request, not by reading the code.
- An in-flight question is cancellable and leaves no partial state.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Cross-session retrieval (T054), MCP evidence in answers, voice input, editing the transcript from
the panel, and any automatic or unprompted question.

## Notes

- ADR-0017 defines Ask as user-initiated model output over one explicit session. It reuses the
  resolved Summarizer backend and provider-neutral reasoning stream; no second client was added.
- Every answer claim is validated against active final-utterance ids. Empty citations and ids not
  present in the supplied record fail closed. The absent-answer eval returns the explicit refusal
  shape, and the serialized-context test asserts no screen, OCR, frame, audio-byte or annotation
  payload enters the initial request.
- The dock is disabled honestly without a backend or completed session, keeps multi-turn context
  only within its current scope, streams receipt progress, supports cancellation without retaining
  partial output, and routes citation chips through the shared transcript focus path.
- Automated focused tests, the full workspace suite, strict Clippy, formatting and diff checks
  pass. Live-provider, visual, and cancellation-click acceptance are `NOT RUN` and remain part of
  the signed-app/manual boundary.

### Closure — 2026-08-13 (planner, narrowed)

Accepted: ADR-0017, citation validation against active final-utterance ids, fail-closed on unknown
or empty citations, the absent-answer refusal eval, the serialized-context test proving transcript
text only, cancellation, and honest disablement without a backend.

Live-provider evidence passes to T029 and T035. Visual acceptance of the dock passes to T055.

