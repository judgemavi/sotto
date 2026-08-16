# T089 — The workspace speaks in entries

**Status:** in-review

**Wave:** N8 — entry workspace

**Depends on:** T086 (the entry model), and the close of the N7 wave (T072-T085) — this task
edits the same workspace files those tasks hold. **The wave closed on 2026-08-16**, so the full
workspace file set is released and this is startable. `docs/design/workspace-v3-mock.html` is the
normative reference for this task; where it and prose disagree, the mock wins unless an ADR
overrides both.

**Concurrency (planner, 2026-08-16): T088 runs alongside this.** It holds `crates/rag/**`,
`crates/app/src/vault.rs`, `crates/app/src/settings/**` and `crates/app/src/notes/controller.rs`.
Do not edit any of them; if this rewrite needs one, stop and report.

**Two pieces of existing behaviour in your own files must survive the rewrite**, because their
tests live elsewhere and a silent drop would not go red where you are looking:

1. `MeetingWorkspace::open_sotto_link` in `workspace/mod.rs` — T088's vault citations enter the app
   through it, reusing `select_meeting` and the citation-reveal path. Keep the method and its
   honest states for a link naming a recording that no longer belongs to its entry.
2. The `KeyboardRoot` unwrapping in `in_workspace` (`workspace/mod.rs:315`). The window is mounted
   `Root ▸ KeyboardRoot ▸ MeetingWorkspace`, and T100 proved the keyboard is unreachable without
   that middle layer. A downcast that expects the workspace directly under `Root` will compile and
   silently stop finding the window, which is exactly the failure `Sotto ▸ Settings…` had before
   T082.

**Owns:** `crates/app/src/workspace/**` (library, layout, mod — full set, since N7 has closed by
this task's start), the entry creation/attachment flows in `crates/app/src/session/**` where the
capture start path chooses an entry, and this task

## Why this exists

T086 puts entries in the store; nothing renders them. This task is the cutover: the library lists
entries, capture records *into* an entry, and an entry is a place you can stand before any
recording exists. It amends the shipped ADR-0019 vocabulary upward exactly as ADR-0021 rules:
below the entry, recordings stay recordings.

## Plan

1. **The rail lists entries.** Grouping, search, rename, Home, the footprint line, and the `Now`
   marker all carry over — over entries. Search matches entry titles, captured names, and note
   text as it does today. An entry's row states what it holds: a live capture, n recordings, or
   prepared-and-not-yet-recorded — the last must never read as a failed or empty recording.
2. **Create an entry from Home** as a first-class beginning alongside "start recording": name it,
   land in it, write prep notes (the T087 document, user layer only, no artifact yet). Starting a
   capture from inside an entry attaches the session to it. Starting one from Home keeps today's
   gesture — the implicit entry T086 provides.
3. **The entry page.** Title and meta (captured facts stay captured facts), the sessions strip
   when more than one session is attached — each with duration and its own transcript tab — and
   the notes document beside, exactly one document for the whole entry. A single-session entry
   looks essentially like today's view; the vocabulary and the document's position are what
   change.
4. **Record again into this entry.** The gesture that makes the dropped-call case work: from an
   entry, start another capture attached to it. The transcript surface makes plain which session
   a row belongs to; media time stays per-session and is never spliced into a fake continuous
   clock.
5. **Ask, delete, and every named surface** speak the entry's name and scope. Ask's single-entry
   scope covers all attached sessions; the delete prompt says what an entry-delete removes
   (sessions, recordings, notes), and single-session delete inside a multi-session entry exists
   and says what it keeps.
6. **Launch and landings** follow T078's decisions unchanged, restated over entries: launch shows
   Home; finishing a capture opens its entry; deleting the open entry lands Home.
7. Narrow-width and launch-and-render checks per the standing UI rule; the prepared-entry empty
   state, the two-session strip, and the record-again control all covered at minimum width.

## Acceptance

- The rail lists entries; a prepared entry with no recording reads as prepared, and every rail
  capability (group, search, rename, footprint, Now) works over entries.
- An entry can be created before any capture, hold typed prep notes, and later receive a
  recording attached by the start-from-entry gesture.
- An entry with two sessions renders both transcripts distinguishably and one notes document;
  citations from the document land in the correct session's row.
- Vocabulary: no surface calls an entry a recording; below the entry, recordings keep ADR-0019's
  language. Judged against the v3 mock.
- Delete semantics for entry-vs-session are distinct, stated in the prompt, and tested.
- Focused and full app tests, a launch-and-render check, strict Clippy over all targets,
  formatting, and diff checks pass.

## Out of scope

The notes document mechanics (T087), the vault (T088), the brief (T090), series pages beyond
showing an entry's series chip, and any capture-pipeline change.

## Notes

Implementation pass, 2026-08-16: the library rail now loads and groups entries, distinguishes a
prepared entry from a failed/empty recording, carries Home/search/rename/footprint/Now behavior
over the entry model, and searches entry titles, captured names, transcripts, generated notes, and
user overlay text. Home can create and enter a named prepared entry. Prep notes append only T087
user-layer overlay operations against an empty base document; they do not synthesize a generated
artifact.

Capture start now accepts an optional entry id and persists the session with
`save_session_in_entry`; Home retains T086's implicit-entry path. Entry pages retain one notes
document while a multi-recording strip switches the per-session transcript. Record again, delete
one recording while preserving its entry, delete the whole entry with explicit consequences, and
entry-wide Ask scope are wired. Launch remains Home, capture completion opens its entry, deleting
the entry returns Home, `open_sotto_link` remains on the citation-reveal path, and the
`Root -> KeyboardRoot -> MeetingWorkspace` lookup is unchanged.

Automated evidence, 2026-08-16:

- Mounted minimum-width prepared-entry and two-recording-entry tests cover prep notes, the record
  control, distinct transcript tabs, the single notes document, and delete-one-keeps-entry.
- Separate mounted destructive-control tests click the real entry-delete and recording-delete
  controls. Entry deletion removes the entry and both attached sessions; recording deletion keeps
  the entry and its other session. These tests use fresh mounted roots so stale `debug_bounds`
  cannot satisfy either assertion.
- Mounted regressions also prove a prepared-note search result opens the correct entry, Record
  again passes that entry id into the pending picker request, and This entry Ask contains both
  attached session ids.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test -p app --lib --locked -q` — 272 passed,
  4 ignored.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test --workspace --locked -q` — passed in the approved
  environment, including the loopback tests.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo clippy --workspace --all-targets --all-features --locked
  -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check` — passed.

The task is in review pending owner inspection against the v3 mock; no signed-app or browser gate
was claimed by this implementation pass.
