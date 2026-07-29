# ADR-0004: Make the append-only session timeline the product spine

- Status: Accepted
- Date: 2026-07-29
- Decision owners: Sotto maintainers

## Context

The board, advisor, post-call summarizer, and retrieval ingester need one replayable
account of a call. The earlier pipeline event enum had no session identity, durable
event identity, or correction history. Treating ASR partials as mutable values keyed by
speaker and start time would also make already-rendered board content move when a
correction arrived.

Raw audio is produced at roughly 100 frames per second on each of two streams. Storing
or broadcasting those frames with durable UI-facing events would swamp both consumers
and SQLite without adding useful timeline history.

## Decision

Each call has one canonical, heterogeneous `TimelineEvent` log. Events have a
session-scoped monotonic id, session-relative timestamp, optional superseded event id,
and a typed payload. Partial and final utterances and suggestions are distinct payload
variants.

The session record stores the capture target selected through the system picker:
optional bundle id and window title, the displayed name, application-versus-window
kind, and whether audio was actually scoped to that target. This scope is recorded once
per session rather than repeated on events. `audio_scoped = false` preserves the fact
that a timeline may contain system-wide audio even when its video was target-scoped.
It also stores caller-supplied Unix-millisecond start and optional end wall clocks.
Core never reads the system clock: capture/session orchestration supplies both values,
so session start reflects the actual lifecycle boundary rather than a later persistence
batch and timeline tests remain deterministic.

The log is append-only. A correction is a new event that references an earlier event
from the same session. The corrected event remains in history. Builders enforce
same-session, earlier-id supersession, and replay validates the same invariants.

Builders checkpoint committed payload batches to persistence to keep live memory
bounded. They retain only compact known/active id sets, so an active checkpointed event
may still be superseded without retaining its payload in the pending buffer. T011 must
durably write each checkpoint before discarding it.

Replay has two deliberate modes. Strict replay rejects the first malformed event and is
used to validate newly built or trusted logs. Lenient replay is the persisted-board
loading path: it reports and skips malformed events while preserving valid content, so
one bad row cannot make the post-call artifact unopenable.

Audio frames remain on a separate capture broadcast channel and never enter the
timeline. VAD, ASR, and prosody turn audio into lower-rate timeline events.

Core owns the event types and the SQLite schema contract. The schema stores sessions
and JSON event payloads, indexed by `(session_id, ts)` and `(session_id, kind)`. The
`rag` crate owns SQLite I/O, migrations, timeline ingestion, and retrieval.
Every RAG connection must execute `PRAGMA foreign_keys = ON`; SQLite does not enforce
declared foreign keys by default.

## Consequences

- All producers and consumers share one stable event vocabulary and ordering model.
- Replaying a session is deterministic, and corrections never retroactively disturb
  board layout.
- Consumers that want current state must resolve supersession rather than mutate the
  log.
- Persisted-load consumers must surface lenient replay issues for diagnosis while still
  rendering the recovered state.
- Session-relative timestamps keep exported logs portable; wall-clock start belongs
  on the session record.
- A persisted call remains explicit about what the user selected and whether its audio
  scope matched that selection.
- Payload JSON remains inspectable and evolvable, while indexed envelope columns make
  timeline filtering efficient.
- Raw audio remains ephemeral and cannot be reconstructed from the timeline.

## Revisit if

- A required event cannot be represented without adding a new payload kind; such a
  schema change requires a follow-up ADR.
- Profiling shows JSON payload persistence cannot meet replay or ingestion budgets.
- Session-scoped monotonic ids cannot support a future merge or import workflow.
- A consumer demonstrates that append-only correction history prevents, rather than
  enables, stable and understandable board behavior.
