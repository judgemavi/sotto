# T085 — Let a recording be named

**Status:** in-review

**Wave:** N7 — v2 workspace

**Depends on:** nothing.

**Owns:** the session title in `crates/core/src/types.rs` (planner-amended core ownership), the
title persistence path in `crates/rag/**`, `crates/app/src/workspace/library.rs`, and this task

**Concurrency (planner, 2026-08-14):** T084 holds `crates/app/src/workspace/layout.rs`,
`crates/app/src/workspace/mod.rs` and `crates/app/src/settings/mod.rs`. Do not edit them. If the
view bar needs the rename gesture, say so and stop — it can be added after T084 closes.

## Why this exists

A recording's name is whatever the captured window happened to be called. In practice that means
entries like `Chat | BTU Daily Standup | Microsoft Teams` and
`The Mental Health Crisis No One Is talking About - YouTube`: the browser tab's title, the meeting
app's window chrome, and the app's own name, in whatever order that application writes them.

It is a reasonable *default* — it is the only thing Sotto knows at capture time — but it is a poor
permanent identity. The rail is the product's main navigation, titles ellipsize in 248px, and the
distinguishing part is often at the end where it gets truncated. A person cannot fix it.

## What to build

1. **A title on the session record**, separate from the capture target. The capture target is a
   captured fact about what was recorded and must not be rewritten; the title is the person's label
   for it. Keep them distinct in the store rather than overwriting the target's display name.
2. **A default that is what happens today**, so nothing changes for a recording nobody renames, and
   an untitled recording never renders as blank.
3. **A rename gesture in the rail**, on the entry a person is looking at when they decide the name
   is wrong.
4. **The chosen title everywhere the recording is named** — rail, the open recording's bar, Ask's
   scope line, the delete confirmation. A rename that leaves the old name somewhere is worse than
   no rename, because the two names then disagree about the same recording.

## What must not change

- **The capture target stays a captured fact.** ADR-0006 and the timeline's append-only rule cover
  what was recorded; renaming is a label over it, not an edit of it. A recording's scope claim —
  which app, whether audio was scoped — must still be the truth from capture time.
- **Search must keep matching both.** Someone who renames a recording to "Standup" must still find
  it by the Teams window title they remember, so index the original alongside the chosen name.
- An imported recording (T071, unbuilt) will have no capture target at all, so the title must stand
  on its own rather than being a decoration on one.

## Acceptance

- A recording can be renamed from the rail, and the new name survives a relaunch.
- Every surface that names a recording shows the same name.
- A recording nobody renames is titled exactly as it is today.
- An empty or whitespace-only name is rejected or falls back to the default, never rendering blank.
- Search matches the chosen name and the original capture target.
- The stored capture target is unchanged by a rename, asserted by test.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Renaming from the view bar while T084 holds it, editing a transcript, tags or folders, and
automatic titles derived from the summary — which is a good idea and a different task.

---

## Result

### Where the title lives, and why there

`RecordingTitle` in `crates/core/src/types.rs` — a newtype, not a `String`. It normalizes on
construction (trim, collapse interior whitespace, bounded at 200 characters) and **returns `None`
for anything blank**, so "never render blank" is a property of the type rather than a rule each
call site has to remember. `Deserialize` runs the same constructor, so a blank title cannot enter
through storage either.

It is deliberately **not** a field on `CaptureTarget`. The capture target is a captured fact
(ADR-0006, ADR-0019); a title is a person's label over the recording. They also have different
lifetimes and different futures — an imported recording (T071) will have a title and *no* capture
target at all, so the title had to stand on its own rather than decorate one.

Persistence mirrors that separation: a new `session_titles` table (schema **v11**), not a column on
`sessions`. Every `capture_target_*` column stays untouched by a rename by construction — a rename
writes one row in a different table. The table cascades on session delete and carries
`CHECK(length(trim(title))>0)`, the storage-level half of the type's guarantee. `SessionSummary`
gained `title: Option<RecordingTitle>`, joined in by `list_sessions`, so one query answers both
"what is this called" and "what was recorded".

### What a rename does and does not touch

Does: inserts/updates/deletes one `session_titles` row; re-reads the catalogue; rebuilds the rail's
search index; re-syncs Ask's scope line.

Does not: touch `sessions`, the timeline, the recording, derived views, or any already-ingested RAG
document. `renaming_a_recording_never_rewrites_what_was_captured` asserts the capture target is
byte-identical after three renames and a clear.

### The gesture

The **open** recording's rail row carries a small ghost `Rename` control (T082 vendored no pencil
and `icons.rs` is not this task's to edit, so it is a word, which is also the honest a11y answer
under GPUI 0.2.2). Clicking it swaps that row's title line for a text input seeded with the name
the row is showing. Return saves, Escape cancels, and an empty field clears the chosen name so the
recording goes back to being called what was captured — the only way back to the default.

Only the selected row offers it: visual calm is a hard requirement, and forty rename controls in
the product's main navigation would be forty controls nobody asked for.

The editor's state (input entity plus its subscription) lives in a `gpui::Global`, following
`transcript.rs`'s `CitationFlash`. `MeetingWorkspace`'s fields are in `mod.rs`, which T084 holds.

### Search

`group_rail` matches the chosen name **and** the captured one directly, and `search_index` writes
both into each recording's search text. Someone who renames a Teams capture to `Standup` still
finds it by `BTU Daily`, `Microsoft Teams`, or `Standup`.

### Left for T084 — three call sites, no plumbing

`library::recording_name(&SessionSummary)` is the name of a recording. `library::target_title` is
now documented as what was *captured* — the default and the search key, not the display name. These
three still call `target_title` and must call `recording_name`, or they will disagree with the rail
about the same recording:

- `crates/app/src/workspace/layout.rs` — the open recording's view bar title
- `crates/app/src/workspace/mod.rs` — `delete_prompt`
- `crates/app/src/workspace/mod.rs` — `sync_ask_scope` (its `"Untitled recording"` fallback is now
  `library::UNTITLED_RECORDING`)

Adding the rename gesture to the view bar itself is still out of scope.

### Verification

`cargo test --workspace` (510 passed, 0 failed), `cargo clippy --workspace --all-targets
--all-features -- -D warnings`, `cargo fmt --all --check`, `git diff --check`. Focused tests:

- `core::types::tests` — normalization, the blank refusal, the length bound, the serde round trip
- `rag` `persistence.rs` — `renaming_a_recording_never_rewrites_what_was_captured`,
  `the_store_refuses_a_blank_title_and_an_orphan_one`,
  `migrating_version_ten_adds_titles_and_keeps_every_captured_fact`
- `workspace::library::tests` — `renaming_from_the_rail_persists_the_name_and_never_the_capture_target`
  drives the real controls in a window mounted as `main.rs` mounts it, then re-opens the store to
  prove the name survives; plus the clear-restores-the-captured-name and Escape-writes-nothing
  paths, and the both-names search assertions.

### Not automated — a human must check

1. The rename control reads as a control and not as noise in a rail full of recordings, in light
   and dark, at the 240 px rail width and the narrow one.
2. Typing, IME, and paste in the inline input; that the field is focused on open; that the caret is
   where a person expects.
3. Clicking elsewhere while an editor is open leaves it open (deliberate: only Return and Escape
   end a rename). Judge whether that is right in use.
4. The rename control on a **running** recording — it is offered, and it writes, which looks
   correct but has only been exercised against a persisted session.
5. That the view bar, delete prompt, and Ask scope line agree with the rail once T084 lands the
   three-call-site change above. Until then they show the captured name.
6. Renaming a recording that was already ingested for cross-session search does not re-title its
   `prior_meeting` document — retrieval still shows the captured name. Deliberate: re-ingesting
   would change the content hash. Decide whether that is acceptable or wants a follow-up.
7. Opening a pre-v11 database from a previous build, and confirming the upgrade is silent and the
   library is unchanged.
