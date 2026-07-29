# T014 — Session timeline: the canonical event model in core

**Status:** todo

**Wave:** 0.5 — blocks every crate that emits or consumes events (T004, T005, T006,
T008, T009, T011, T015). T002, T003, T007 and T010 are unaffected and continue.

**Depends on:** T001 (landed as `42af01c`)

**Owns:** `crates/core/src/timeline/**`, `crates/core/src/types.rs`,
`crates/core/src/lib.rs`, `docs/adr/0004-session-timeline.md`, **root `Cargo.toml`**,
`crates/screen/` and `crates/advisor/` stubs

> Root-manifest ownership transfers from T001 to this task, and only for the duration of
> it. The `AGENTS.md` reframe adds two crates (`screen`, `advisor`); create both as empty
> stubs with `[lints] workspace = true` and register them as members here, so T015 and
> T013 never touch a shared file. Same rule as T001: crate-specific dependencies stay in
> the owning crate, and `[workspace.dependencies]` does not grow.

> This task **reopens the T001 freeze deliberately.** `AGENTS.md` now makes the session
> timeline the spine of the product, and the frozen `PipelineEvent` cannot express it.
> Better to break the freeze once, now, in a single owned task than to let nine crates
> discover the mismatch independently. After this lands the contract re-freezes.

## Why this is its own blocking task

`AGENTS.md`: *"The canonical data structure of the entire product is the session
timeline… Everything is a producer into it or a consumer of it."* Four consumers already
depend on its exact shape — the board UI, the advisor, the post-call summarizer, and
RAG ingestion. Two of T001's decisions are now actively wrong:

- `PipelineEvent` carries no identity, no session, no timestamp. Every event kind in the
  new model shares `{id, session_id, ts, kind, payload}`.
- T001 froze supersession as *"key on `(source, start)`"*. `AGENTS.md` now requires
  **append-only** semantics: a correction is a *new event referencing the superseded id*,
  never a mutation. Layout stability in the whiteboard depends on this — the UI must be
  able to replay the log and never move something the user already read.

## Plan

1. **The envelope** in `crates/core/src/timeline/event.rs`:

   ```rust
   pub struct TimelineEvent {
       pub id: EventId,                    // monotonic + unique within a session
       pub session_id: SessionId,
       pub ts: Duration,                   // offset from session start, not wall clock
       pub supersedes: Option<EventId>,    // correction target; None for fresh events
       pub payload: EventPayload,
   }
   ```

   `kind` is discriminated by `EventPayload`'s variant rather than a parallel string
   field — one source of truth. Expose `TimelineEvent::kind() -> EventKind` returning a
   cheap `Copy` discriminant for filtering and for the SQLite `kind` column.

   `ts` is session-relative so a timeline is portable and replayable; keep wall-clock
   session start on the session record, not on every event.

2. **Reuse the T001 payload types** — do not re-model them. `EventPayload` wraps what
   already exists plus the new kinds, matching the `AGENTS.md` list exactly:

   ```rust
   pub enum EventPayload {
       UtterancePartial(Utterance), UtteranceFinal(Utterance),
       Vad(VadSegment),
       Prosody(ProsodyDelta),
       ScreenSnapshot(ScreenSnapshot),
       Trigger(Trigger),
       SuggestionPartial(Suggestion), SuggestionFinal(Suggestion),
       UserAnnotation(UserAnnotation),
       Error(PipelineError),
   }
   ```

   Partial/final are **separate variants, not an `is_final` bool** — `AGENTS.md` calls
   both first-class and the note-taker consumes only finals while the copilot needs
   partials. A variant lets a consumer filter without inspecting payload internals.
   Drop `is_final` from `Utterance` and `Suggestion` accordingly.

3. **New payload types** the reframe introduces:
   - `ScreenSnapshot { frame_ref: FrameRef, ocr_text: String, active_app: Option<String>, window_title: Option<String>, visible_from: Duration, visible_to: Option<Duration> }`.
     `frame_ref` is a content-addressed path or id, **never inline image bytes** — the
     memory-discipline rule says frames are sampled, referenced and pruned, and the bus
     clones every payload per subscriber.
   - `ProsodyDelta` — the rolling talk-time ratio and speech-rate updates that T006 emits
     independently of any utterance.
   - `UserAnnotation { anchor: EventId, text: String, mark: MarkKind }` — the rep's own
     marks on the board.

4. **Append-only invariants**, enforced in code and stated in the module doc:
   - events are never mutated after construction (no `&mut` accessors on payload);
   - `supersedes` must reference an earlier id in the same session;
   - a superseded event stays in the log — consumers that replay see the correction, and
     the board's layout never retroactively deletes what a rep already read;
   - ids are allocated by the session, monotonic, so ordering is total and stable.

   Provide `Session::next_event_id()` and a `TimelineBuilder` that makes constructing a
   well-formed event the easy path. Unit-test that a supersede chain resolves to the
   latest version and that replaying the log twice yields identical state.

5. **Bus integration.** `EventBus` now carries `TimelineEvent` in place of
   `PipelineEvent`. Keep the existing lag-counting behaviour untouched — it was
   reviewed and is correct. Keep capture's separate `broadcast::Sender<AudioFrame>`
   channel exactly as is: raw audio frames are **not** timeline events and must never
   enter the log (~100/s × 2 streams would swamp both the bus and SQLite). Say so
   explicitly in the module doc; it is the single most likely misreading of "everything
   is a timeline event".

6. **Persistence contract, not persistence.** Define the SQLite schema for
   `sessions` and `events` here — `events(id, session_id, ts, kind, supersedes, payload)`
   with the payload as JSON, indexed on `(session_id, ts)` and `(session_id, kind)` —
   plus `serde` round-trip guarantees. **T008 owns the implementation** in the `rag`
   crate against the same SQLite file. Core defines the shape; `rag` does the I/O.
   Record this split in ADR-0004 since `AGENTS.md` mentions persistence under both.

7. **ADR-0004** covering: the timeline as spine, append-only supersession and why
   (layout stability), session-relative timestamps, audio frames excluded from the log,
   and the core-defines/rag-implements split. `AGENTS.md` now makes timeline schema
   changes ADR-worthy — this is the baseline that future ADRs amend.

## Contract for downstream tasks

`TimelineEvent`, `EventPayload`, `EventKind`, `EventId`, `SessionId` and the SQLite
schema are **frozen** on landing. Producers emit via `TimelineBuilder`; consumers match
on `EventPayload`. Adding a new event kind is an ADR.

## Acceptance

- `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  and `cargo test --workspace --all-features` all clean.
- Replay test: applying a log with supersede chains twice yields identical state.
- A supersede referencing a later or foreign-session id is rejected.
- Round-trip test: every `EventPayload` variant survives JSON serialisation.
- No API allows mutating an event after construction.

## Out of scope

SQLite I/O (T008), screen capture itself (T015), any UI, the advisor.
