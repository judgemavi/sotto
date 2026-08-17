# T092 — Screen consultation, finished: a budget, Ask's reach, and a disclosure that survives

**Status:** in-review

**Wave:** D1 — living notes

**Depends on:** T066 (`in-review`) whose `## Open gaps` section is this task's charter, T070
(`todo`) for the notes surface and artifact schema this task extends, and T075/T076's close for
`workspace/notes.rs`. All four closed on 2026-08-16, so this is startable. T066's own open gap — a consulted frame is
disclosed on a fresh run and not on a cached reopen, because consultations live for the run rather
than for the artifact — is recorded there and is this task's to fix. The one-inspection rule this amends is
ADR-0009/T028's; the amendment is recorded here and needs a short ADR entry on acceptance.

**Owns:** the inspection budget in `crates/insight/**`, the Ask inspection seam in the
`crates/insight` ask module, persisted consultation records in `crates/rag/**` alongside the
derived artifact, the consultation receipt in `crates/app/src/workspace/notes.rs` and
`crates/app/src/reasoning/**` (sequential handoff from T066), and this task

## Why this exists

T066 joined the seam: a notes run can pull a frame, disclosed and image-denied. Three gaps keep
the capability smaller than the product needs, all recorded by T066 itself:

1. **One inspection per run.** Over a sixty-minute recording full of "as you can see here", a
   single inspection means the summary mostly cannot use the screen even when the transcript begs
   for it. The cap exists for cost and privacy discipline, not because one is the right number.
2. **Ask cannot look at all.** `AskEngine` has no inspector seam, so "what was on the slide when
   she said that" — the most natural screen question — is unanswerable.
3. **The disclosure dies with the run.** Consultations live in memory per run; a cached reopen
   serves notes that consulted the screen with no trace that they did. A model that looked at the
   screen and a record that does not say so is precisely what the transparency posture forbids.

## Plan

1. **Replace the single inspection with a budget of N per run** (small, fixed, stated — start at
   three and record the reasoning). Every inspection still goes through the same typed request,
   provenance, and image-deny policy; the budget exhausting is reported to the model and logged,
   never silent. The prompt tells the model its budget so it spends inspections where the
   transcript most needs them.
2. **Give Ask the seam notes has:** thread the inspector assembly through `AskEngine` the way
   `NotesController` does, budget included, live-recording resolution behaving exactly as T066
   left it (a still-growing recording is honestly unavailable).
3. **Persist the consultation log with the derived artifact** it informed. Reopening a cached
   summary shows the same disclosure the original run showed: which moments, what precision, what
   was refused, and that no image left the device. The record is append-only alongside the
   artifact version, not a mutation of it.
4. **Render the receipt in the notes column** beside the model/provenance receipt, per T066's
   handoff note — and in Ask's answer receipt when an Ask run consulted. Quiet: a line that
   expands, not a panel.
5. Keep every T066 invariant under the new budget, re-asserted: no decode on runs that need no
   visual context, image requests refused before decode, nothing serialized into a request beyond
   the OCR text and stated metadata.

## Acceptance

- A notes run can perform up to N inspections; the N+1th is refused, reported to the model, and
  logged; a run needing none still decodes nothing — all asserted through the app's own runtime
  assembly, not doubles.
- An Ask run over a retained recording can consult a frame and its answer receipt shows it.
- A cached reopen of a summary whose run consulted the screen shows the full consultation
  disclosure after app restart, asserted by test.
- No image reaches a backend without the separate opt-in, asserted over the serialized request —
  unchanged and re-proven under the budget.
- The ADR entry amending the one-inspection rule is written and referenced here.
  See ADR-0022 (`docs/adr/0022-bounded-screen-consultation-receipts.md`).
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

The image-transport opt-in itself (Phase 5), decoder or OCR changes, proposal-path inspection
(T013), and any UI beyond the two receipts.

## Evidence (2026-08-16)

- App-owned recording assembly: 5 focused screen tests pass, including three successful decodes,
  a fourth logged/refused request, zero-work default, pre-decode image denial, missing-media
  degradation, serialized-request inspection, and cached restart restoration.
- Retained-recording Ask: focused `AskEngine` inspection/receipt test passes; the app assembles the
  same product inspector for single-recording and selection scope and renders an expandable answer
  receipt.
- Persistence: schema v19 adds the consultation record to grounded-artifact identity; focused
  atomic replay/conflict/delete and pre-upgrade migration tests pass.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test --workspace --locked` passes in the approved
  environment. The sandboxed run reached only the known local-loopback MCP restriction; the exact
  `cargo test -p mcp --test rmcp_http --locked` gate passes 4/4 outside that sandbox.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo clippy --workspace --all-targets --locked -- -D warnings`,
  `cargo fmt --all -- --check`, and `git diff --check` pass.

## Planner review — 2026-08-16

Returned from `done` to `in-review`: the board reserves `done` for the planner. Same correction as
T087's handoff; the work is not in question.

Verified independently rather than from the handoff. All four T066 tests survive by name in
`reasoning/inspection.rs` — the 249-line reduction there is the consultation types relocating up
into `crates/insight`, which is where they belong once Ask and notes share a budget, not coverage
being dropped.

The inherited defect is genuinely fixed and the fix is pinned. Replacing the cached load's
`screen_consultations` with an empty vec makes
`a_notes_run_pulls_one_frame_through_the_apps_own_runtime_assembly` fail on *"a cached reopen after
app restart must restore the full consultation receipt"*, so the assertion is not vacuous. It
builds a fresh `NotesController` over the same database, which is the restart.

`SCREEN_INSPECTION_BUDGET = 3` with the refusal path covered by
`the_fourth_screen_request_is_refused_reported_and_logged_by_the_app_runtime`, and ADR-0022 is
written and amends ADR-0009 as required.

**One flake fixed during review, the third of this shape today.**
`the_view_bar_delete_asks_in_a_dialog_and_cancel_keeps_everything` failed roughly one full run in
three on *"Cancel must dismiss the dialog"* while passing five times out of five alone. Dismissal
is deferred, so `refresh` + `run_until_parked` is a race under load. The three dismissal assertions
in `layout.rs` now go through a bounded `wait_for_dialog_dismissed`, which still fails a dialog
that genuinely never closes. Three consecutive full runs pass.

Residual: the receipt's appearance in the notes column and in Ask is unverified by eye, as with
every visual claim on this board. The disclosure's *content* is asserted.
