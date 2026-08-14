# ADR-0016: Put transcript and notes side by side

- Status: Accepted
- Date: 2026-08-12
- Decision owners: Sotto maintainers
- Amends: ADR-0015 ordering only

## Context

ADR-0015 correctly retired the Board and made the append-only meeting record the product surface.
It placed the transcript above generated notes. Notes are now generated only when the user asks for
them, so they are an active review surface rather than a passive appendix. Reading a cited note
against its evidence in a stacked layout turns every citation into a vertical context switch.

The workspace also needs a stable way to reopen persisted sessions and a quiet home for explicitly
asked questions without turning either into another product lens.

## Decision

The shipped workspace has four panes:

- a session rail listing persisted meetings, grouped by recency and filterable by transcript and
  saved-note text;
- a transcript column;
- a generated-notes column beside the transcript, separated by resizable dividers; and
- a collapsible Ask dock, collapsed by default, whose expanded state persists across launches.

Selecting a past session opens a clearly marked read-only projection without disturbing an active
capture session. The active session stays marked live in the rail, and `Back to live` restores its
transcript and follow-live state. Meeting citations continue to focus the exact transcript
`EventId` through one workspace-owned focus path; focusing evidence never scrolls the notes column.

The two-column change is justified specifically by on-request note generation: the user reads a
new derived artifact against its factual record. Side-by-side evidence makes that comparison
continuous.

## What does not change

Every substantive ADR-0015 ruling remains in force:

- there is no Board lens, spatial canvas, zoom/pan control, or canvas overlay in the product shell;
- the append-only timeline remains canonical, and model output remains derived rather than fact;
- citations focus exact transcript events;
- live following pauses after manual scrolling and resumes only through a visible action;
- screen evidence stays on demand; and
- visual calm remains a hard requirement.

The Ask dock is only a disabled, honest shell in this decision. ADR-0017 defines its reasoning and
privacy contract before T053 makes it interactive.

## Amendment: one-window visual system (2026-08-13)

The meeting workspace is the sole window opened at launch. While capture is preparing, running, or
finalizing, a session bar above the four panes names the selected target, reports screen, captured
audio, and microphone scope separately, shows elapsed time, and keeps Stop visible. The workspace
window owns the `requires_visible_control()` close guard: a close request first stops capture and is
vetoed until finalization reaches a terminal state.

Reasoning, experimental Codex, model, and MCP configuration no longer occupy an always-open control
window. Settings is closed by default and is opened explicitly from the Sotto application menu;
closing it has no effect on a running session.

[`docs/design/workspace-mock.html`](../design/workspace-mock.html) is the normative visual reference
for the workspace. Where its presentation and task prose disagree, the mock wins unless AGENTS.md
or an accepted ADR overrides both. Its light/dark palette, spacing and type scale, semantic
registers, transcript grid, empty states, and capture-scope language are implementation contracts,
not illustrative suggestions. This amendment does not reopen the pane ordering decided above.

## Consequences

Transcript and notes presentation now have separate source-file ownership, allowing later work on
typed annotations and streaming transcript presentation without shared-file edits. The session
rail becomes the single selection surface for live and persisted records. Window state gains one
small local field for Ask expansion; it contains no transcript, prompt, credential, or provider
data.
