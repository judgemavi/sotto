# T086 — The entry above the recording

**Status:** in-progress

**Wave:** N8 — entry workspace

**Depends on:** ADR-0021. T085 (`in-review`) owns the title persistence path in `crates/rag/**`
until it closes — build the migration on top of whatever schema head exists when this task starts,
and do not edit the `session_titles` path while T085 holds it. If T085 is still open when this
task reaches the title-adoption step, stop and report rather than racing it.

**Owns:** the entry types in `crates/core/src/types.rs` (planner-amended core ownership, following
the T085 precedent), the entry persistence module and its migration in `crates/rag/**`, and this
task

## Why this exists

An entry in the library *is* a recording session today: nothing exists before capture starts, and
one occasion cannot span two captures. ADR-0021 decides the entry is the unit — a titled object
holding a notes document and zero or more recording sessions — because meetings exist before they
are recorded (prep, agenda, brief) and occasionally across captures (the dropped call, the huddle
before the call).

This task is the data model only. No UI changes; the workspace continues to render sessions
exactly as it does until T089.

## Plan

1. Add the entry type to `core`: identity, created-at, title, and the attachment relation to
   sessions. The rule from ADR-0021 is the contract: **facts hang off sessions; documents hang off
   entries.** Nothing currently on a session moves except the title.
2. Persist entries in a migration on top of the current schema head. Every existing session gets
   an entry created for it mechanically, carrying its `session_titles` title up; the session keeps
   only captured facts. A fresh database and a migrated one must be indistinguishable to every
   query.
3. Support an entry with zero sessions as a first-class row: creatable, titleable, deletable,
   listable. It must be representable in the catalogue the rail reads, even though the rail does
   not render entries until T089.
4. Support attaching a new session to an existing entry, and detaching is not supported — a
   session belongs to exactly one entry for its whole life. If capture starts with no entry
   chosen, an entry is created implicitly (today's behaviour, one level up).
5. Deleting an entry deletes its sessions, recordings, derived artifacts, and index documents —
   the cascade the store already performs per session, lifted one level. Deleting a single session
   out of a multi-session entry keeps the entry and its documents.
6. Decide and record where the search-index document hangs during the transition. Today it is
   per-session (`prior_meeting`); leave it per-session and note that T089/T090 read through the
   entry. Do not rebuild the index in this task.

## Contract for downstream tasks

`core` exposes the entry type and the session-to-entry relation. `rag` can create, list, title,
and delete entries; attach sessions; and answer "which entry does this session belong to" in one
query. T087 hangs the notes document off the entry. T089 renders entries. Until T089, the shipped
UI remains session-shaped and nothing user-visible changes.

## Acceptance

- A migrated library has one entry per pre-existing session, titles carried over, and every
  existing query and test over sessions still passes unchanged.
- An entry with zero sessions can be created, listed, renamed, and deleted.
- A second session can be attached to an existing entry, and both sessions' timelines, recordings,
  and captured facts are untouched by the attachment, asserted by test.
- Deleting an entry cascades to everything its sessions own; deleting one session of two keeps the
  entry, asserted by test.
- Re-running the migration is a no-op; opening a pre-migration database upgrades silently.
- No file in `crates/app/**` changes.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Rendering entries (T089), the notes document and overlay (T087), the vault projection (T088),
series pages, prep notes content, and any change to timeline events or capture.

---

## Partial result — independent entry model (2026-08-14)

- Review follow-up: deleting a session removes its automatically-created entry when that was the
  entry's final session, while preserving an entry that still owns another session.
- Residual: `delete_entry` still crosses a filesystem/SQLite atomicity boundary by removing managed
  media before its row transaction. A durable quarantine plus compensating restore needs its own
  storage contract; this patch does not pretend the two resources can share one transaction.

Schema v13 adds `entries` plus the one-owner `entry_sessions` relation without broadening or
rebuilding the captured-fact `sessions` row. `core` now distinguishes `EntryId` from `SessionId`
and exposes an `Entry` with creation time, optional normalized title, and its attached recording
ids. `rag` can create/list/rename/delete prepared entries, resolve a session's entry in one query,
save a capture directly into a selected entry, and create an implicit entry when ordinary
`save_session` has no selected destination. Re-saving into the same entry is idempotent; attempting
to move a session to another entry is refused because detach is not a supported lifecycle.

The v12-to-v13 migration mechanically creates one entry per existing session and is idempotent.
Entry deletion lifts the existing session cascade over every attached session, including timeline,
recording state, derived views, session-owned documents/chunks, and vector rows. Deleting one
session from a multi-session entry leaves the entry and its other session intact. The
`prior_meeting` search document deliberately remains per-session; T089/T090 must read it through
the entry relation, and this task does not rebuild the index.

### Exact T085 handoff still open

T085 remains `in-review` and explicitly owns `session_titles` plus its persistence/catalogue path.
Therefore v13 leaves `session_titles` byte-for-byte intact and does **not** copy its title into
`entries.title`; migrated entry titles are temporarily `NULL`. Once T085 closes, the remaining
step is to adopt `session_titles.title` into the owning entry and redirect/retire the per-session
title APIs and `list_sessions` join without changing capture-target facts. Until that handoff is
done, the first acceptance item (titles carried over) is not satisfied and T086 cannot close.

### Verification so far

- `cargo test -p core --lib` — 23 passed.
- `cargo test -p rag` — 39 passed, 1 ignored performance check.
- Focused v12 migration, prepared-entry, multi-session, and entry-cascade tests pass.
- `cargo test --workspace` — passed; live/model/performance gates remained explicitly ignored.
- `cargo clippy -p core -p rag --all-targets --all-features -- -D warnings` — passed.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo clippy --workspace --all-targets --all-features --
  -D warnings` — passed after the concurrent T070 lane settled.
- `cargo fmt --all --check` and `git diff --check` — passed.
- No file in `crates/app/**` changed in the T086 lane.
