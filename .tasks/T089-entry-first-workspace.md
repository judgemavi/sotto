# T089 — The workspace speaks in entries

**Status:** blocked

**Wave:** N8 — entry workspace

**Depends on:** T086 (the entry model), and the close of the N7 wave (T072–T085) — this task
edits the same workspace files those tasks hold. `docs/design/workspace-v3-mock.html` is the
normative reference for this task; where it and prose disagree, the mock wins unless an ADR
overrides both.

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
