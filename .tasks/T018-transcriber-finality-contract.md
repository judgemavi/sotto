# T018 — Fix the `Transcriber` contract so it can express finality

**Status:** done

**Wave:** blocking — T011 cannot wire ASR into the timeline correctly until this lands

**Depends on:** nothing (T005 is done and will adopt it afterwards)

**Owns:** `crates/core/src/traits.rs`, `crates/core/src/lib.rs`, `crates/asr/**`,
`docs/adr/0005-transcriber-finality.md`

## Why this exists

This is a defect in the frozen contract, introduced by us, not by an implementer.

T001 defined `Transcriber::poll() -> Vec<Utterance>` back when `Utterance` carried
`is_final: bool`. T014 then removed that field, correctly, because partial and final became
separate `EventPayload` variants — and the review approved it without checking that
`Transcriber` could still express the distinction. It cannot:

```rust
pub trait Transcriber: Send {
    fn push(&mut self, frame: &AudioFrame);
    fn poll(&mut self) -> Vec<Utterance>;   // partial or final? unknowable
}
```

T005 built a correct commit policy internally — an unstable tail plus N agreeing passes,
with committed audio never re-entering an inference window — and then had to throw the
result away at the trait boundary. It flagged this rather than editing frozen `core` or
guessing, which is exactly right.

Left unfixed, T011 would have to infer finality when turning transcripts into timeline
events. Everything downstream rests on that: finals are what the note-taker and summarizer
consume, and supersession only terminates because something is eventually final.

## Plan

1. Add to `crates/core/src/traits.rs`:

   ```rust
   /// A transcription result and whether it is still subject to revision.
   ///
   /// Mirrors the timeline's `UtterancePartial` / `UtteranceFinal` split rather than
   /// carrying a bool, so a consumer matches the same way it will match the event it
   /// becomes.
   pub enum TranscriptUpdate {
       Partial(Utterance),
       Final(Utterance),
   }
   ```

   with `utterance()` and `into_utterance()` accessors so callers that genuinely do not
   care are not forced to match.

2. Change the trait to `fn poll(&mut self) -> Vec<TranscriptUpdate>;` and export the new
   type from `lib.rs`.

3. Update `crates/asr` to emit the variant its stabilizer already knows. The commit policy
   does not change — only what it is allowed to say. Keep
   `identical_partials_are_suppressed_and_commits_do_not_retract` green, and extend it to
   assert that a committed result is emitted as `Final` and an uncommitted one as
   `Partial`.

4. **Do not** re-add `is_final` to `Utterance`. Partial-versus-final is a property of the
   emission, not of the utterance, and the timeline already models it that way.

5. Write `docs/adr/0005-transcriber-finality.md`: what the gap was, why the fix mirrors the
   payload variants, and the general lesson — when a field moves from a struct into an enum
   discriminant, every trait that returns that struct has to be re-checked. That is the
   review miss worth recording, not the one-line fix.

## Acceptance

- `Transcriber::poll` returns `Vec<TranscriptUpdate>`; `Utterance` gains no `is_final`.
- `asr` emits `Final` only for committed output, asserted by test.
- `cargo fmt --check`, strict workspace clippy, and `cargo test --workspace --all-features`
  all clean.
- ADR-0005 written.

## Out of scope

Any other change to `core`. The rest of the contract stays frozen.

## Review round 1 — approved

`TranscriptUpdate::{Partial, Final}` added with `utterance()` / `into_utterance()`
accessors, `Transcriber::poll` returns it, `Utterance` gained no `is_final`, and the ASR
stabilizer assigns finality at its commit decision — asserted directly:

```rust
assert!(matches!(second.as_slice(), [TranscriptUpdate::Final(_)]));
```

ADR-0005 records the general lesson rather than just the fix: when a field moves from a
returned struct into an enum discriminant, every trait returning that struct has to be
re-checked. That is the review failure worth remembering — the one-line signature change
was never the hard part.

The contract is frozen again. T011 can now wire ASR into the timeline without inferring
finality.
