# T014 — Session timeline: the canonical event model in core

**Status:** changes-requested (R5 approved; R6 — session wall-clock — outstanding)

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

## Notes

- Added the immutable `TimelineEvent` envelope, all reviewed payload variants and
  discriminants, session-scoped id allocation, validated append/supersede construction,
  and deterministic replay state. Event fields are private with read-only accessors so
  callers cannot mutate an appended event.
- Removed payload-level `is_final`; partial and final utterances/suggestions are now
  separate event variants. Suggestions carry their triggering event anchors so T013
  does not need to reopen the contract. Raw `AudioFrame`s remain excluded from the
  timeline bus.
- Added serde coverage for every payload variant plus replay, supersession-chain,
  foreign-session, and later-event rejection tests.
- Defined the SQLite schema contract in core and documented that T008 owns its I/O and
  migrations in ADR-0004.
- Registered empty `screen` and `advisor` stubs without adding workspace dependencies.
- Formatting plus the integrated all-feature headless test and strict Clippy suites
  pass across core, capture, VAD, ASR, prosody, screen, providers, advisor, RAG, MCP,
  and CLI. A complete workspace check is temporarily blocked on T003's GPUI dependency
  requiring full Xcode/Metal on this host; T003 records that environmental gate
  separately.
- Review round 1: `TimelineBuilder` now uses O(1) known/active id sets and supports
  checkpointing pending payloads for persistence without losing supersession validity.
  T011's persistence-before-drop rule is documented in the module and ADR.
- Review round 1: strict replay remains the invariant validator; `replay_lenient`
  reports and skips malformed persisted rows so the valid remainder of a call renders.
- Review round 1: the schema contract now requires `PRAGMA foreign_keys = ON` for every
  T008 SQLite connection.

## Review round 1 — changes requested (light)

The model is right. Immutable envelope with private fields and read-only accessors,
partial/final as separate variants, `is_final` correctly removed from the payloads,
`Suggestion.anchors` added so T013 will not need to reopen the contract, deterministic
replay, foreign-session and later-target supersession both rejected, every payload variant
round-tripping through JSON, and the "audio frames are never timeline events" invariant
documented where someone will actually read it. ADR-0004 records the core-defines /
rag-implements split. Verified clean.

Two changes before this re-freezes, because both are shaped by the API rather than hidden
behind it — cheap now, expensive once six crates depend on them.

### R1. `supersede()` is O(n) per call, on the live path

```rust
if !self.events.iter().any(|event| event.id() == target.id())
```

Sliding-window ASR supersedes its previous partial roughly twice a second per speaker, so
over a two-hour call this scans a log of tens of thousands of events tens of thousands of
times. Keep a `HashSet<EventId>` (or check presence via the id range plus a set of
superseded ids) and make it O(1).

### R2. The API makes T011's bounded-memory requirement unachievable

`TimelineBuilder` holds every event for the life of the session, and `supersede()` requires
the target to still be in `self.events`. T011's acceptance criterion is bounded memory over
a two-hour call, and its only escape today is `into_events()`, which consumes the builder
and ends the session.

These two requirements cannot both hold as written. Resolve it here, in the owning task:
give the builder an eviction or checkpoint operation that drains committed events for
persistence while keeping enough state to validate future supersessions — the id set is
small, the payloads are what is large. Then document the rule T011 must follow: which
events may be evicted, and what happens if something supersedes an evicted target.

### R3. Decide strict-vs-lenient replay, and say which

`replay()` returns `UnknownSupersededEvent` when a target is not active — correct for
validating a log you just built, but it also means one bad row makes a persisted session
**unopenable**: T008 loads, T016 replays, the whole call fails to render. Given the board
is the post-call artifact, a timeline that partly renders beats one that refuses to.

Either keep strict replay and add a lenient loading path for T008/T016, or document that
persisted logs are trusted and say why. Note that `serde` already bypasses the constructor,
so deserialized events are not covered by the builder's guarantees regardless.

### R4. Note for T008, no change here

The composite `FOREIGN KEY (session_id, supersedes)` in `SQLITE_SCHEMA` is inert unless the
connection sets `PRAGMA foreign_keys = ON` — SQLite defaults it off per connection. Add that
to the schema contract comment so T008 does not inherit a guarantee that silently is not one.

## Re-review

R1–R3 addressed, the eviction rule documented for T011, and the existing test suite still
green.


## Review round 2 — approved, with one small addition

All four items landed and verified: `HashSet`-backed O(1) supersession lookup,
`checkpoint()` with a test proving supersession survives eviction, strict and lenient
replay both documented and tested, and `PRAGMA foreign_keys = ON` in the schema with the
per-connection caveat spelled out. Workspace is clean: 25 passed, 0 failed, 5 ignored.

### R5 — record the capture target on the session (do this before re-freezing)

`AGENTS.md` now scopes every session to a user-chosen application or window, and states
that the session record carries what the timeline is *of*. Add it while this task is still
open:

```rust
pub struct CaptureTarget {
    pub bundle_id: Option<String>,
    pub display_name: String,      // what the picker showed the user
    pub window_title: Option<String>,
    pub kind: TargetKind,          // Application | Window
    pub audio_scoped: bool,        // false if audio is system-wide — see T002
}
```

Hang it off the session, not off every event — it is per-session, and the events already
carry `session_id`. Include it in the `sessions` table in `SQLITE_SCHEMA`.

`audio_scoped` matters more than it looks: T002 is still determining whether
ScreenCaptureKit can scope audio per-application. If it cannot, every consumer needs to
know the recording may contain audio from outside the chosen target — T012's indicator has
to say so, and a persisted timeline should still be honest about it a year later. A
timeline that cannot explain its own scope cannot be explained to the person in it.

**Note on `Source`:** the earlier review floated generalising `Source` beyond two values,
on the assumption that ambient capture might be a direction. It is not — `AGENTS.md` now
lists ambient capture as an explicit non-goal, and scoped sessions preserve
mic-equals-rep / target-equals-customer. **`Source` stays two-valued.** Disregard that
suggestion.

### Re-review

R5 landed, schema updated, existing tests still green. Then the contract re-freezes and six
tasks unblock.

## R5 implementation notes

- Added `CaptureTarget` and two-valued `TargetKind` (`Application | Window`) to the
  immutable session record. `Session::new` now requires the selected target, and both
  `Session` and `TimelineBuilder` expose it read-only.
- Added nullable bundle id/window title plus required display name, constrained kind,
  and constrained integer `audio_scoped` columns to `sessions` in `SQLITE_SCHEMA`.
- Added capture-target retention and JSON round-trip coverage, including the stable
  snake-case target-kind representation used by SQLite. `Source` remains exactly
  `Mic | System`.
- Updated ADR-0004 to record session-level capture scope and why an unscoped-audio fact
  must remain durable.
- Verified `cargo fmt --check`, strict Clippy across the workspace/all targets/all
  features, and `cargo test --workspace --all-features`: 29 passed, 0 failed, 5 ignored
  manual live-provider tests.

## Review round 3 — R5 approved; one small gap (R6)

`CaptureTarget` and `TargetKind` match the spec, `Source` stayed two-valued, accessors are
read-only, ADR-0004 explains why scope is recorded once per session rather than per event,
and `audio_scoped = false` correctly preserves the fact that a timeline may hold
system-wide audio even when its video was target-scoped. Verified: fmt clean, strict
clippy clean, 29 passed / 0 failed / 5 ignored.

Worth calling out: the test asserting the JSON encodes `"kind":"window"` exists to keep
the Rust enum and the SQL `CHECK (capture_target_kind IN ('application','window'))` from
drifting apart. That mismatch would have failed silently at the persistence boundary and
only in production. Good instinct — keep doing that where a Rust type and the schema encode
the same vocabulary.

### R6 — the session record is missing its wall-clock

`SQLITE_SCHEMA` declares `started_at_unix_ms INTEGER NOT NULL` and `ended_at_unix_ms`, but
`Session` carries neither. `AGENTS.md` specifies the session record as *"start and end
wall-clock, and the capture target"* — the capture target landed, the clock did not, so
T008 has no source for a column the schema requires.

This matters because of *when* the value would otherwise be taken. T008 persists events on
a background task in batches, so the obvious implementation stamps `started_at` at first
write — which is not when the session started, and is wrong by however long the first
batch took. Nothing detects it, and it is baked into persisted data forever.

Add both to `Session`, supplied by the caller rather than read from the clock inside
`core`:

```rust
pub const fn new(
    id: SessionId,
    capture_target: CaptureTarget,
    started_at_unix_ms: u64,
) -> Self
pub fn end(&mut self, ended_at_unix_ms: u64)
pub const fn started_at_unix_ms(&self) -> u64
pub const fn ended_at_unix_ms(&self) -> Option<u64>
```

Keep `core` free of `SystemTime::now()` — the caller passes the timestamp, which keeps the
crate testable and deterministic, the same discipline `ts` already follows by being
session-relative.

### Re-review

R6 landed, existing tests still green. Then the contract re-freezes and the six-way
fan-out goes out.
