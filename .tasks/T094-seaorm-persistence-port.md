# T094 — Persistence on SeaORM, end to end

**Status:** done

**Wave:** M4 — recording

**Depends on:** the SeaORM feasibility spike (2026-08-15, summarized under `## Spike findings`
below — read it before planning, it answers most of the questions this task would otherwise
rediscover). Blocks nothing, but **holds `crates/rag/**` and every `rag` call site for its
duration**, so T074, T076, and the `crates/insight` executor fix must land after it, not beside it.

**Owns:** `crates/rag/**`, and — for the call-site conversion in step 3 only — the `rag` call sites
in `crates/app/**`, `crates/insight/**`, and `crates/cli/**`. This is a planner-amended exception
to the usual per-file ownership, because an API-shape change cannot be split by file. It does
**not** grant redesign of anything it touches: convert the call, change nothing else.

## Why this exists

Two reasons, one confirmed by the spike and one by this repo's own history.

The migration chain is hand-rolled and has produced three separate correctness defects in a single
day: steps stamping the final schema version rather than their own, so a crash between them left a
database claiming to be current with work unapplied; an ordering hazard where a crash could put a
repair step permanently out of reach; and a backfill that had to be reasoned about interleaving by
interleaving. A framework that records applied migrations and runs each atomically makes that class
structurally impossible rather than merely less likely.

Separately, row decoding is entirely by hand — roughly 70 `row.get` calls, 15 `query_row`, 9
`query_map`, 11 `prepare` — with positional indices that no compiler checks. Typed entities remove
that layer.

The owner has decided on full adoption rather than the narrower migrations-only option, and that
existing databases are not a concern: a clean wipe is acceptable and preferred.

## Spike findings that constrain the design

These are measured, not assumed. A working prototype is at
`.claude/worktrees/agent-af2867bb1730c3a70/spike/` (delete it once this task closes).

- **`vec0` works under sqlx.** `sqlite3_auto_extension` is a property of the linked SQLite, not of
  rusqlite, so registering `sqlite-vec` through `libsqlite3-sys` covers every sqlx connection opened
  afterwards. `Store::hybrid_search`'s query — the 3-way JOIN with `MATCH ?1 AND k=?2` and reused
  numbered placeholders — returns correct results verbatim.
- **rusqlite and sqlx coexist** in one binary over one `libsqlite3-sys 0.35.0`. This is what makes
  the port incremental rather than a one-way door.
- **The DDL stays raw SQL, permanently.** `sea-query 1.0.2` has no `CREATE TRIGGER` and no
  `CREATE VIRTUAL TABLE`. `chunks_fts`, `vec_chunks`, the `chunks_ai`/`chunks_ad` triggers, and the
  `documents_identity_idx` expression index are all hand-written forever. They move inside migration
  `up()` bodies; they do not become entities. Note `execute_unprepared` is the `execute_batch`
  equivalent — the prepared path accepts exactly one statement and fails on a trigger body.
- **No domain invariant moves into an entity.** The `session_recordings` four-state CHECK still
  rejects at the database, but the entity is 11 loose `Option` columns and `ActiveModel` lets an
  illegal combination compile, failing only at runtime. The narrowing back to a variant enum stays a
  hand-written `TryFrom<Model>`. `RecordingTitle`, `has_valid_scope()`, and the `EntryId`/`SessionId`
  distinction all stay exactly where they are. The one real gain is `state` as a typed
  `DeriveActiveEnum`.
- **Entities must live in `crates/rag`, never `crates/core`.** sea-orm pulls +95 crates including
  sqlx-mysql and sqlx-postgres, which `default-features = false` does not remove. Putting entities
  in `core` makes `capture`, `vad`, `prosody`, `asr`, and `screen` compile two server database
  drivers to record audio.
- **The async ripple is the whole cost.** 75 production call sites (`app` 52, `insight` 17, `cli` 6)
  plus 112 in tests. Zero are inside `render` — the store is already off the frame path
  deliberately. 29 are on the GPUI main thread with **no tokio runtime in reach**: `crates/app` pulls
  tokio with `["rt","sync","time"]` only, has no long-lived runtime, and builds 9 ad-hoc
  current-thread runtimes instead. GPUI 0.2.2 brings smol, and sea-orm 2.0 offers only
  `runtime-tokio` or `runtime-async-std`.
- **The real obstacle is ownership, not `async`.** `insight` and `cli` hold `&'a Store` in struct
  fields (`summarizer.rs:144`, `clustering/mod.rs:86`, `notes/mod.rs:421`,
  `cli/src/reasoning/mod.rs:67,75,93,101`), which blocks `'static` tasks. Roughly half of all sites
  are an ad-hoc `Store::open` that becomes a shared handle.
- `Retriever` and `PersistenceSink` already return `BoxFuture`; no `async_trait` is needed.
- Workspace lints (`unreachable_pub`, `unwrap_used`, `allow_attributes_without_reason`, all `deny`)
  pass clean over derive-generated entities kept `pub(crate)`. This was a plausible blocker and is
  not one.
- A `max_connections(1)` write pool reproduces today's single-serialized-writer rule; 4 concurrent
  writers with `busy_timeout(5s)` produced 0 busy errors over 100 commits.

## Plan

**Step 1 — collapse the schema to one baseline.** Delete `migrate()`'s chain and its migration
tests. There is one schema, at one version. `Store::open` refuses anything older with a plain
statement that the database predates the current schema and should be deleted — it must not attempt
a partial upgrade. This precedes SeaORM so the framework starts from one clean migration instead of
inheriting fifteen steps that are being discarded anyway.

**Step 2 — entities, migrations, and an async `Store`.** Register `sqlite-vec` before the pool
opens. Keep the reader/writer split's guarantee: a single serialized writer. Port the DDL into
migration `up()` bodies unchanged — resist rewriting SQL that currently works while also changing
the driver. Preserve every `TryFrom`-shaped narrowing that today turns a loose row into a checked
domain type; the entity is the wire, not the model.

**Step 3 — convert the call sites and delete the scaffold.** Between steps 2 and 3 a temporary
synchronous wrapper over the async core keeps the tree compiling. It is scaffolding with a defined
death, not a compatibility layer: **removing it is part of this step's acceptance**, and the task
cannot close while it exists. Convert `app`, `insight`, and `cli`. Give `crates/app` its tokio
runtime, resolve the `&'a Store` fields into shared handles, and fix the 10 sites already inside an
`async fn` that would otherwise panic on `block_on`.

## Contract for downstream tasks

`rag` exposes an async `Store` and owns its own migrations. No other crate learns about SeaORM,
sqlx, or entities — the boundary is `Store`'s public API and the `core` domain types it returns.

## Acceptance

- One schema, one baseline migration, no version chain. Opening an older database fails with a
  stated reason rather than a partial upgrade.
- `vec0` KNN and FTS5 search return the same results as before, asserted against the existing
  retrieval tests rather than new ones written to fit.
- The quarantine/recovery contract on `delete_entry` survives intact, including
  `delete_entry_restores_every_file_when_the_row_transaction_fails` and
  `recover_quarantined_media_resolves_each_tombstone_from_the_row_that_survived_a_crash`.
- Every existing `rag` test passes unweakened. A test may change shape to await a future; it may not
  lose an assertion.
- The temporary sync wrapper is deleted.
- No blocking SQLite or embedding call runs on a tokio executor thread — this task subsumes the
  `crates/insight` executor defect the spike found.
- `crates/core` does not depend on sea-orm, sqlx, or any database driver.
- Full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Changing any schema shape, table, or constraint beyond what the port requires; redesigning `Store`'s
public API beyond making it async; the notes taxonomy; anything in `crates/capture`.

---

## Result — 2026-08-15

`crates/rag` now opens SQLite through SeaORM/sqlx, registers `sqlite-vec` before either pool is
created, and exposes only the async, cloneable `Store`. The writer pool remains serialized at one
connection while reads use a separate four-connection pool. All base tables have crate-private
entities; recording state remains a typed `DeriveActiveEnum` narrowed into the existing checked
domain variants. SQLite-specific virtual tables, triggers, and the expression index remain raw DDL
inside one irreversible `m0001_current_schema_baseline` migration, as the spike required.

The fifteen-step upgrade chain and its historical migration tests are gone. A database carrying a
pre-baseline `user_version` is refused with explicit delete-and-reopen guidance; a focused test
asserts both the refusal and the stated remedy. The async conversion reaches app, insight, and CLI.
Long-lived app persistence work uses the process runtime, already-async workers await directly,
shared consumers own cloned `Store` handles, and embedding and filesystem work that can block is
kept behind `spawn_blocking`. No synchronous `Store` compatibility wrapper remains.

The existing retrieval and persistence assertions remain: vec0/FTS hybrid ordering, append-only
timeline replay, grounded artifact atomicity, recording retention, entry deletion, quarantine
rollback, and crash recovery all pass. The feasibility spike directory was removed after the gates
closed.

Verification:

- `cargo test -p rag` — 33 passed, 1 performance/model-cache check ignored.
- `cargo test -p app --lib` — 236 passed, 4 explicit real-media/model checks ignored.
- `cargo test --workspace --quiet` — passed across the full workspace; only the repository's
  explicit live, real-media, model-cache, and manual checks remained ignored.
- `cargo clippy --workspace --all-targets -- -D warnings` — passed.
- `cargo fmt --all -- --check` and `git diff --check` — passed.
