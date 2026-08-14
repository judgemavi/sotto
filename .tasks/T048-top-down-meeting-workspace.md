# T048 — Top-down transcript and notes workspace

**Status:** done

**Wave:** N3 — product UI simplification

**Depends on:** T038; T047

**Owns:** `crates/app/src/main.rs`, `crates/app/src/notes/view.rs`,
`crates/app/src/settings/mod.rs` for product copy, `AGENTS.md`,
`docs/adr/0015-top-down-meeting-workspace.md`, `.tasks/README.md`, and this task

## Goal

Replace the mounted Board lens with one readable top-down meeting workspace containing the live or
persisted transcript, cited AI notes, and selected MCP source context.

## Acceptance

- The product shell does not construct or expose `BoardCanvas`, Board lens controls, or zoom/pan.
- Live speech renders as chronological wrapped text with timestamps and neutral speaker labels.
- Superseded partials are hidden; each stream has at most one current live partial.
- The transcript follows new rows until the user scrolls away and exposes an explicit Follow live
  action.
- Notes remain below the transcript; citations focus the exact transcript EventId.
- Persisted meeting selection reopens the exact session transcript without provider or MCP contact.
- Focused and full app tests, strict Clippy, formatting, and diff checks pass.

## Out of scope

Deleting historical board implementation files, changing timeline persistence, adding speaker
diarization, live proposal generation, or claiming signed-app visual acceptance.

## Notes

- 2026-08-12 implementation: the product window no longer constructs `BoardCanvas` or exposes a
  Board tab. The shared timeline now renders directly as a plain chronological transcript above
  meeting notes. Final utterances persist as rows; only the latest active partial for each stream
  is shown. Follow-live pauses on manual scroll and citation buttons focus transcript rows.
- Focused Notes workspace tests passed 12/12, including chronological settled/live projection and
  consecutive-session switching. The full app library suite passed 81/81 with
  `gpui/runtime_shaders`; the app compile check and strict all-target Clippy passed; workspace
  formatting and diff checks passed. The current binary was not launched for a visual acceptance
  claim; T035 still owns the signed real-session verdict.

### Closure — 2026-08-13 (planner)

Closed as superseded rather than re-reviewed. T049 deleted `notes/view.rs` and rewrote `main.rs`, so
this task's owned slice no longer exists as shipped code. What survives and remains binding is
ADR-0015 plus the behaviours T049 carried forward as a move: citation focus by exact `EventId`, one
live partial per stream, follow-live pausing on manual scroll, and exact-session replay. Its unrun
signed-app acceptance was never claimed and passes to T035.

