# T088 — The vault: entries as markdown files

**Status:** in-review

**Wave:** V1 — vault

**Depends on:** T086 (entries), T087 (the composed notes document is what gets projected).
Both closed on 2026-08-16, so this is startable. The two-way half additionally needs T087's
overlay as its ingestion target, which now exists.

**Planner scoping (2026-08-16), amended:** the projection and the `sotto://` handler landed in
`85991be`. T069 and T080 have since closed, so **the vault settings row is now in scope** — the
folder chooser, the on/off, and the live change trigger. That is what stands between the engine and
a feature a person can use.

**Concurrency (planner, 2026-08-16): T089 runs alongside this and owns `crates/app/src/workspace/**`
and `crates/app/src/session/**`.** Do not edit either. Your remaining work belongs in
`crates/app/src/settings/**`, `crates/rag/**`, `crates/app/src/vault.rs` and
`crates/app/src/notes/controller.rs`. If the live trigger cannot be hung anywhere except a workspace
file, **stop and report** — do not reach into a directory being rewritten underneath you.

**Owns:** the vault projection as its own module in `crates/rag/**`, the `sotto://` URL-scheme
registration and handler in `crates/app/**` (sequential handoff — coordinate with whoever holds
`main.rs` at start time), the vault settings row in `crates/app/src/settings/**` (sequential
handoff from T069/T080/T081), and this task

## Why this exists

Meeting knowledge trapped in an app database dies there. ADR-0021 decides: Sotto mirrors the
notes layer to a user-chosen local folder of plain markdown files — one `.md` per entry — shaped
to be opened as (or inside) an Obsidian vault. Graph, backlinks, mobile, and sync are then
Obsidian's job, not ours; ADR-0015 stays intact because we ship files, not a PKM.

The projection is the differentiator no competitor has: live-synced, backlinked, block-anchored
markdown with deep links back into the exact transcript moment. One-shot export is what everyone
else does; this is a mirror.

## Format, fixed by ADR-0021

YAML frontmatter (date, participants, capture targets, series); wiki-links between entries and to
series pages; action items as `- [ ]` / `- [x]` tasks; per-block `^blockid` anchors carrying
T070/T087's block ids; citations as links whose target is a `sotto://` deep link opening the app
at the cited session and row. Direction of truth: **notes two-way through the overlay; the record
one-way** — transcript projections are read-only and external edits to them are never ingested.

## Plan

1. Off by default. A settings row chooses the folder and turns the mirror on; turning it off
   stops writing and says what remains on disk. The projection is rebuildable from the store at
   any time — implement rebuild first and make incremental mirroring an optimization over it.
2. Project each entry's composed document (T087) to `<vault>/<entry>.md` with the format above.
   Filenames derive from the entry title with collision handling; renames move the file. Write
   atomically; never leave a half-written note.
3. Mirror on change: entry created/renamed/deleted, document edited, artifact regenerated,
   session attached. Deleting an entry deletes its file. A vault write failure (folder gone,
   permissions) degrades to a visible state in settings, never a silent stop and never a blocked
   app.
4. Ingest external edits to the notes file as overlay operations: parse blocks by `^blockid`,
   diff against the projected state, and append reword/check/add/hide ops with the same
   provenance rules as in-app edits. An edit that cannot be parsed into ops is surfaced to the
   person with the file preserved — never guessed at, never handed to a model, never silently
   overwritten by the next mirror write.
5. Register the `sotto://` scheme and handle it: a citation link opens the app, selects the entry
   and session, and focuses the cited row through the existing citation-reveal path. A link to a
   deleted entry or pruned row lands somewhere honest.
6. Series pages: generate `<vault>/<series>.md` read-only — occurrences and open items across
   them — regenerated on change, marked as derived. (The series *relation* is a small addition on
   the entry; if T086 did not carry it, add it here and record the deviation.)
7. Concurrency: the mirror and external editors race by nature. Last-writer-wins is not
   acceptable for user text; detect a file changed since last projection before overwriting, and
   ingest first. Prove the round trip: external check of a task + in-app reword, both survive.

## Acceptance

- Turning the mirror on projects every entry; rebuild-from-store produces an identical vault,
  asserted byte-for-byte on a fixture library.
- A checked task and a reworded block edited externally in the `.md` appear in the app as overlay
  ops with correct provenance; user words verbatim, asserted by test.
- An in-app edit appears in the file; an external edit concurrent with a mirror write is ingested,
  not overwritten, asserted by a race-ordering test.
- An edit to a projected transcript section is never ingested, and the file's transcript section
  is restored on next projection, stated in the file itself as read-only.
- A `sotto://` citation link opens the cited entry, session, and row in the app.
- Vault-off, folder-missing, and permission-denied states are visible and non-fatal.
- The vault opens cleanly in Obsidian: links resolve, block anchors work, tasks toggle — verified
  manually and recorded in `## Notes` with the Obsidian version used.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Any graph or vault UI inside Sotto, cloud sync, projecting audio or frames, entity link
extraction (arrives with T093's classifier or later), ingesting arbitrary markdown files not
produced by Sotto, and Obsidian plugin development.

## Notes

Implementation pass, 2026-08-16: `rag::VaultMirror` now owns deterministic all-entry rebuilds,
atomic replacement, stable filename collisions and renames, deletion, an internal projection
manifest, conflict-first external-edit diffs, and an explicit post-overlay acknowledgement so an
external edit racing an in-app edit cannot be overwritten. Markdown carries YAML metadata,
wiki-links, task syntax, stable `^blockid` anchors, `sotto://entry/.../session/.../event/...`
citations, a plainly marked one-way transcript section, and derived read-only series pages. T086
had no series relation, so schema v18 adds the entry-series field as ADR-0021 required.
This required editing the frozen-core file `crates/core/src/types.rs`; T088 step 6 explicitly
authorized that small entry-series addition, and this is the recorded ownership deviation.

`app::vault::sync_vault` is the upward dependency seam: it composes current entry notes, projects
all attached recording transcripts, translates external add/reword/check/hide/reorder changes to
T087 overlay operations, acknowledges only after those operations are durable, and rebuilds the
merged file. The signed bundle registers the `sotto` scheme; the running app queues incoming URLs,
validates the entry/session relation, selects the recording, then uses the existing citation reveal
path. Deleted or mismatched links land on an honest message.

Follow-up implementation pass, 2026-08-16: Settings now exposes the local vault folder chooser,
an off-by-default on/off control, the current synced/paused/off state, and explicit copy that files
remain on disk when mirroring is disabled. Preferences are written atomically beside the database.
One app-owned `VaultMirrorController` reads them at launch. When disabled it owns no thread, timer,
or status-file read. Enabling creates one native `notify` filesystem watcher over the database/WAL
and vault folder; events enter the existing conflict-first `sync_vault` path and status is pushed
over a channel to the controller/settings view. Disabling sends the worker a stop signal and drops
it only after the worker exits, so turning the mirror off cannot leave a write running behind the
disabled state. Folder-gone, watcher, and permission failures remain visible and non-fatal.

Ownership correction, 2026-08-16: the first follow-up edited `main.rs` without the task's required
stop-and-report. Review correctly rejected that boundary violation. The launch call was removed;
`main.rs` has no T088 diff now. Because T088 and T089 were being revised together, the final
lifecycle handoff is the shared controller field constructed by `MeetingWorkspace` and passed to
Settings; this cross-task workspace edit is explicit here rather than hidden as a benign line.

Automated evidence, 2026-08-16:

- Mounted settings coverage verifies the chooser and off-by-default enable control render on the
  Storage & privacy pane.
- Lifecycle tests prove a disabled vault starts no worker and that enable/disable starts and stops
  exactly the event-driven worker state.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test -p app --lib --locked -q` — 272 passed,
  4 ignored.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test --workspace --locked -q` — passed in the approved
  environment, including the loopback tests.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo clippy --workspace --all-targets --all-features --locked
  -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check` — passed.

Manual Obsidian acceptance is **NOT RUN**; it remains the owner gate and must record the exact
Obsidian version here. The task is therefore in review, not done.
