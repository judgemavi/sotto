# ADR-0005: Represent transcription finality at the trait boundary

- Status: Accepted
- Date: 2026-07-29
- Decision owners: Sotto maintainers

## Context

The timeline represents revisable and committed speech as distinct
`UtterancePartial` and `UtteranceFinal` payload variants. Earlier, `Utterance` itself
carried an `is_final` field. Removing that field correctly moved finality into the
timeline enum discriminant, but `Transcriber::poll` continued to return bare
`Utterance` values. The trait therefore erased information that its consumers need to
construct the right timeline payload.

The ASR stabilizer already knows whether an emission is an unstable partial or a
committed final. Asking downstream orchestration to infer finality from text, timing,
or repeated emissions would duplicate the commit policy and could misclassify output.
That would undermine the guarantee that finals never retract.

## Decision

Add `TranscriptUpdate::{Partial, Final}` to the core trait contract and make
`Transcriber::poll` return `Vec<TranscriptUpdate>`. The variants contain an unchanged
`Utterance`; finality is a property of an emission and is not restored as a bool on the
utterance. Borrowing and consuming accessors support callers that genuinely do not
care about finality without requiring them to duplicate a match.

The Whisper stabilizer creates the variant at the point where its agreement and
unstable-tail policy decides whether output is committed. The worker and transcriber
preserve that decision without interpreting it again.

## Consequences

- Timeline orchestration can map transcript updates directly to the matching partial
  or final payload variant.
- Only the stabilizer decides when an ASR result becomes final, preserving the
  non-retraction guarantee.
- Transcriber implementations and consumers must handle both variants explicitly,
  unless they deliberately use an accessor to discard finality.
- When a field moves from a returned struct into an enum discriminant, review must
  re-check every trait returning that struct. Missing that dependency caused this
  contract gap; the correction itself is intentionally small.

## Revisit if

- A transcriber needs an additional lifecycle state that cannot be expressed as
  partial or final.
- Timeline utterance payload semantics change so they no longer mirror ASR emission
  finality.
