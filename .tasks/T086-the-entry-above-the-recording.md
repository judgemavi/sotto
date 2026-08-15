# T086 — The entry above the recording

**Status:** in-review

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
- Closed: `delete_entry` now quarantines every managed file (rename to a `.{session_id}.deleting`
  tombstone) before opening its row transaction, restores every tombstone if that transaction fails
  or the entry id does not resolve, and only unlinks them for good after it commits. A crash between
  quarantine and commit is resolved deterministically on the next `enforce_recording_budget` sweep by
  `Store::recover_quarantined_media`, using the owning row's surviving state (not the tombstone) as
  the source of truth. See the doc comments on `Store::delete_entry` and
  `Store::recover_quarantined_media` in `crates/rag/src/store.rs` for the exact contract.

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

### T085 handoff — closed 2026-08-15

T085's manual rename acceptance passed and it is `done`, so `session_titles` came back to this
task. The entry is now the only titled object: `session_titles` is **retired outright** — the
table, its schema constant, `set_session_title`/`load_session_title`, and the `list_sessions`
join are all deleted, and `crates/app/src/workspace/library.rs` renames by resolving the owning
entry and calling `set_entry_title`. Schema v15 drops the retired table.

**The first acceptance item was retired, not met.** It required a migrated library to carry
pre-existing titles into `entries.title`. The owner decided against any legacy-compatibility path
for this repo — a clean wipe is acceptable and preferred — so titles are deliberately not carried
across, and a recording that predates entries migrates in untitled. `crates/rag/tests/persistence.rs`
asserts exactly that. Read the acceptance list above with this substitution in mind; it is a
recorded decision, not an unmet requirement.

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

### Quarantine residual closed (2026-08-15)

`Store::delete_entry` now quarantines before it commits: every session's managed file is renamed to
a `.{session_id}.deleting` tombstone (`Store::quarantine_recording_media`) before the row transaction
opens. If any quarantine step fails, or the row transaction fails or rolls back (including the
`NotFound` path for an entry id that never resolves), every tombstone renamed so far is restored and
the delete leaves no trace. Only after the row transaction commits are the tombstones unlinked for
good. `Store::recover_quarantined_media` resolves any tombstone still on disk after a crash between
those two steps by checking the owning row, not the tombstone: if the session row is gone, or its
`session_recordings.path` is already cleared, the row-side commit reached disk first and the tombstone
is finished (unlinked); otherwise the commit never landed and the tombstone is renamed back. It runs
at the top of `Store::enforce_recording_budget`, which is the one place the app already calls with a
`recording_directory` on both existing call sites (after a recording settles, and when the retention
budget changes), so both resolve any leftover tombstone with no new wiring.

Proved by four new tests in `crates/rag/tests/persistence.rs`:
`deleting_unknown_entry_returns_not_found_and_touches_no_file`,
`delete_entry_restores_every_file_when_the_row_transaction_fails` (forces the row transaction to fail
via a `BEFORE DELETE` trigger and asserts every file and row survives intact), and
`recover_quarantined_media_resolves_each_tombstone_from_the_row_that_survived_a_crash` (three
tombstones planted by hand — uncommitted, row-cleared, session-gone — resolved in one sweep, exactly
one restored). The pre-existing `deleting_entry_cascades_every_owned_recording_artifact_and_index_document`
and delete/entry tests in the same file pass unweakened.

- `cargo test --workspace --locked` — every crate passes except two pre-existing, non-deterministic
  failures in `providers::codex` process-group timing tests (`timed_out_probe_kills_and_reaps_its_process_group`
  in one run, `cancellation_interrupts_a_prompt_larger_than_the_stdin_pipe` and
  `dropping_consumer_interrupts_blocked_stdin_and_kills_process_group` in another) — different tests
  fail across repeated runs with no code changed, in a crate this task never touches, confirming
  environmental subprocess-timing flakiness rather than a regression from this change.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo clippy --workspace --all-targets --all-features -- -D
  warnings` — passed.
- `cargo fmt --all -- --check` and `git diff --check` — passed.
- Owns respected: only `crates/rag/src/store.rs`, `crates/rag/src/lib.rs`,
  `crates/rag/tests/persistence.rs`, and this task file changed.

**App path assessed, not touched (outside Owns):** `crates/app/src/workspace/mod.rs`'s
`confirm_delete_session` calls `RecordingLibrary::delete` (which wraps `remove_recording_media`) and
then `Store::delete_session` as two separate calls. This is a materially smaller version of the same
split — `remove_recording_media` already commits its own row update before unlinking its tombstone,
so a failure of the *second* call (`delete_session`) never leaves a dangling row pointing at a deleted
file; `session_recordings` already durably reads `deleted` either way. The residual there is narrower:
if `delete_session`'s transaction fails after media removal already committed, the session/entry rows
survive in a "recording deleted" state the user didn't ask to keep — a retryable rough edge, not a
leak, since nothing is inconsistent and `recover_quarantined_media` was never invoked (no tombstone
is left behind by that path). Recommend folding `remove_recording_media` into `delete_session` itself
(taking a `recording_directory` parameter) so both entry points obey the same quarantine-then-commit
rule `delete_entry` now does, and the app no longer has to sequence two `Store::open` calls correctly
by hand. Not implemented here: it requires editing `crates/app/src/workspace/mod.rs` and
`crates/app/src/session/recordings.rs`, both outside this task's Owns list.
