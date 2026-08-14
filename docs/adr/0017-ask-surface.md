# ADR-0017: Ask is a cited derived view over an explicit meeting scope

- Status: Accepted
- Date: 2026-08-12

## Context

Meeting notes are a durable synthesis and proposals are optional, unsolicited-in-form suggestions.
Neither contract covers a user deliberately asking a follow-up question. Ask needs conversation
state, but must not weaken the append-only record or quietly widen the evidence boundary.

## Decision

Ask is a collapsible, user-initiated derived view. It is quiet by construction: only an explicit
question starts provider work, and an in-flight question can be cancelled. With no resolved
reasoning backend, the control is disabled and says why; the transcript and notes remain usable.

The initial scope is exactly one selected session. Its request contains capture-target metadata and
timestamped, stream-labelled final transcript text with local prosody annotations. It contains no
audio and no eager screen bytes or OCR. Any later screen inspection must use ADR-0009's existing
one-request, separately authorized path.

Every factual claim is a discrete answer item with one or more valid `EventId` citations. Selecting
a citation follows the existing transcript-focus path. Provider output with an uncited claim or a
citation outside the supplied record is rejected. If the record cannot answer, the only valid
result is an explicit refusal that says what the record does cover.

Question and answer turns may be retained in memory while the selected session remains the same.
Prior answers help interpret follow-up wording, but are never evidence: the immutable session
record remains the sole source of claims. Changing session clears the conversation. Ask output is
model output, not meeting fact, and is never appended as captured speech or otherwise used to
mutate the timeline. Any later audit event must follow ADR-0012's typed model-output approach.

Ask resolves the same provider registry/backend identity used by notes and uses the shared
provider-neutral reasoning transport. There is no second client. T054 may add an explicit
all-sessions scope, but must preserve these answer, citation, refusal, and cancellation shapes.

## Consequences

Ask degrades honestly when reasoning is unavailable, cannot manufacture an uncited answer, and
cannot silently turn conversational history into meeting evidence. The strict structured response
means partially streamed bytes are presentation-only; only the validated final object becomes an
answer.
