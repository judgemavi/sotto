# T096 — A cached summary must still say what its run lost

**Status:** in-review

**Wave:** R3 — reasoning product

**Depends on:** nothing. T075 landed the display; this is the persistence behind it.

**Owns:** the downgrade persistence in `crates/rag/**`, the report field in
`crates/insight/src/notes/**`, and the cached path in `crates/app/src/notes/controller.rs`, and
this task

## Why this exists

T067 records when a backend silently downgrades a control Sotto asked for — a requested reasoning
effort the provider declined, a schema constraint it ignored. T075 now surfaces those on the
summary: a run that lost something says so instead of presenting its result as unqualified.

But only a *fresh* run can. `GroundedMeetingNotesReport.normalizations` is populated in memory and
never written, so `save_grounded_derived_view` does not carry it and
`load_latest_grounded_notes_status` cannot return it. Reopen the app and the same summary — the one
produced by a degraded run — presents itself as clean.

That is the worse of the two states, because it is the one the user sees most. A summary is
generated once and read many times, and every reading after the first has lost the qualification.
T075's acceptance says *"a downgraded one says what was lost"*; today it says so once and then
forgets.

## Plan

1. Persist the normalizations alongside the artifact they qualify. They belong to the run, not to
   the recording, so they travel with the stored derived view rather than becoming a session fact —
   the same reasoning that keeps `session_recordings` state off the `sessions` row.
2. Return them from the cached load path so `NotesState::Ready` and `Stale` carry them whether the
   report was just produced or read from disk.
3. Make the two paths indistinguishable to the column. T075's renderer already handles the fresh
   case; a cached summary must reach it in the same shape rather than through a second branch.

## Acceptance

- A summary produced by a downgraded run still names what was lost after the app is reopened,
  asserted by test — not only in the session that generated it.
- A summary from a clean run claims nothing, before and after a reopen.
- The stored artifact remains readable, and nothing about a capture fact changes.
- Full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Changing what counts as a downgrade (T067 owns that), the summary taxonomy, and the rendering
T075 already built.

## Notes

Found reviewing T075's handoff on 2026-08-15, where it is recorded as a residual: *"Cached downgrade
history remains unavailable because it is not persisted."* Filed as its own task rather than left as
a note, because a residual with no owner is how an acceptance item quietly stops being true.

## Implementation notes — 2026-08-15

Backend downgrade observations now travel with the grounded derived view they qualify. Schema v16
adds a non-null JSON `normalizations` column with `[]` as the upgrade default; the RAG persistence
contract returns it and treats a conflicting replay as an evidence-integrity error. The migration
touches only `grounded_derived_views`, not the session or timeline fact tables.

`insight` owns the durable wire projection for the provider-domain observations. It stores dispatch
id, open-ended backend id, and the requested control using an explicit snake-case enum, reconstructs
the checked provider types on both exact cache hits and latest/stale loads, and fails closed on
malformed persisted JSON or backend ids. The app controller therefore receives the same
`normalizations` shape for fresh, cached, and stale reports; no rendering branch was added.

Acceptance evidence:

- `grounded_artifact_bundle_commit_replay_conflict_and_delete_atomically` passes and asserts the
  downgrade JSON round-trip plus conflict rejection when only that run qualification changes.
- `grounded_artifact_from_before_downgrade_persistence_stays_readable_after_upgrade` passes against
  a simulated v15 database. The v16 migration restores the column with `[]`, preserves the older
  artifact as readable, and leaves the captured session target unchanged.
- `downgraded_summary_keeps_its_qualification_after_reopen` passes through a real normalized
  backend dispatch, disk persistence, store close, and a new `NotesController`; the reopened cached
  observations equal the fresh run. `cold_reopen_stays_ready_when_reasoning_is_disabled_without_new_provider_work`
  also passes and now asserts a clean run remains empty after reopen.
- `cargo test -p rag --locked` passes: 34 passed, 1 explicit model-cache performance check
  ignored. `cargo test -p insight --locked` passes: 46 tests across unit and integration targets.
- Strict focused Clippy passes for `rag` and `insight` with all targets/features and `-D warnings`.
- `rustfmt --edition 2024 --check` over all owned Rust files and `git diff --check` pass.

After T096 and T097 integration, `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test --workspace --locked`
passes across the workspace (only the repository's explicitly gated real-model, live-provider, and
manual tests remain ignored). Strict workspace Clippy passes over all targets and features with
`-D warnings`; repository-wide formatting and `git diff --check` pass. No signed-app or visual gate
is required: T075 owns the unchanged renderer, while this task proves cached-load shape parity
headlessly.
