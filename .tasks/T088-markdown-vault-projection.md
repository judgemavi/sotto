# T088 — The vault: entries as markdown files

**Status:** blocked

**Wave:** V1 — vault

**Depends on:** T086 (entries), T087 (the composed notes document is what gets projected).
Blocked until both close. The two-way half additionally needs T087's overlay as its ingestion
target.

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
