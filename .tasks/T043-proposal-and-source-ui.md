# T043 — Anchored proposal and source UI

**Status:** blocked

**Wave:** P2 — optional copilot UI

**Depends on:** T013; T040; T042

**Owns:** `crates/app/src/proposals/**`, sequential edits to `crates/app/src/notes/**`,
`crates/app/src/lib.rs`, `crates/app/src/main.rs`,
`.tasks/T043-proposal-and-source-ui.md`

## Goal

Render optional proposals as calm inline system-output rows with visible meeting and MCP evidence.

## Plan

1. Add proposals-off, enabled, waiting, streaming, final, unavailable, and failed states.
2. Render stable partial-to-final rows near their cited transcript evidence without duplicating or
   reflowing earlier transcript rows.
3. Provide citation navigation and a source receipt drawer that distinguishes meeting evidence
   from external evidence.
4. Support dismiss, copy, and explicit accept-as-user-note; none performs an external action.
5. Preserve bounded transcript rendering and Follow live behavior.

## Contract for downstream tasks

The UI renders advisor output but never treats it as a meeting fact or invokes MCP actions.

## Acceptance

- Proposals are off by default and no empty panel implies failure.
- Every proposal has valid visible meeting evidence and every external citation resolves to a receipt.
- Partial/final replay is stable and replaces one inline proposal row.
- Dismiss/copy/accept are local, explicit, and covered by tests.
- Missing or unavailable MCP evidence is stated in text and never hidden.

## Out of scope

External action execution, a separate overlay/canvas, and meeting-note generation.
