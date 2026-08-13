# T038 — Meeting history and notes review workspace

**Status:** done

**Wave:** N2 — minimum AI product UI

**Depends on:** T037; T032; T016's explicit 2026-08-12 board-code ownership handoff. T035 is required
only for T044's manual product verdict, not for automated UI implementation.

**Owns:** `crates/app/src/notes/**`, `crates/rag/src/store.rs`, focused RAG tests,
`crates/app/src/session/mod.rs`, `crates/app/src/board/**` after T016's recorded handoff, and sequential integration edits to
`crates/app/src/lib.rs`, `crates/app/src/main.rs`, `crates/app/Cargo.toml`, `Cargo.lock`,
the narrow cancellation plumbing in `crates/insight/src/notes/**` and
`crates/insight/src/context/mod.rs` without changing note schema/prompt/cache semantics,
`.tasks/T038-meeting-notes-review-ui.md`

## Goal

Make cited meeting notes the primary post-call AI surface while retaining the board as the
underlying chronological evidence.

## Plan

1. Add a session catalogue and surface the exact durable session id after finalization.
2. Add a notes controller with one pinned, cancellable generation per selected session and stale
   result fencing.
3. Compose one meeting workspace with Notes and Board lenses; keep Settings separate.
4. Render disabled, generating, ready, cached, failed, and retry states truthfully.
5. Navigate a note citation to the corresponding stable board item.
6. Rename product speaker labels to `You` and `Meeting audio`; inferred attendee names remain
   derived note content.

## Contract for downstream tasks

The selected session is a single shared app entity. Notes and Board observe it; reopening a
meeting does not start capture or reasoning until the user requests or enables notes.

## Acceptance

- Cold launch performs no provider request, MCP contact, capture, or notes generation. The existing
  T027 controller may read OpenAI credential readiness from Keychain locally so persisted settings
  can render truthfully; it must not validate or transmit the key until an explicit action.
- A completed durable session appears once and can be reopened.
- No-reasoning mode says the transcript was saved and explains how to enable AI notes.
- Cancellation and session switching prevent stale notes from overwriting the selected meeting.
- Every displayed note citation navigates to a known event on the board.
- Automated UI tests cover all states; signed real-meeting usability remains T044.

## Out of scope

MCP configuration, proposal cards, overlay productionization, and visual redesign beyond the
meeting workspace.

## Implementation handoff — 2026-08-12

- Added the newest-first durable meeting catalogue and exact completed `SessionId` publication
  only after the finalized session record and timeline reload successfully.
- Added the Notes controller and composed Notes/Board workspace. A selected persisted meeting
  replays its exact timeline into the board; a citation changes lens only when its `(SessionId,
  EventId)` resolves to a stable board item. Starting a new session restores the live board.
- Notes generation pins the resolved backend, forwards one caller cancellation token through both
  reasoning passes, fences generation/session results, and owns worker handles. Switching,
  disabling reasoning, and controller teardown cancel without joining a non-cooperative worker on
  the GPUI thread; finished retired workers are reaped opportunistically.
- Added explicit no-meeting, disabled, generating, generated, cached, failed, and retry copy, plus
  neutral `You` / `Meeting audio` board labels.

### Automated evidence

- PASS — `cargo check -p app --locked --features gpui/runtime_shaders` with
  `CARGO_TARGET_DIR=/private/tmp/sotto-t038`, `WHISPER_DONT_GENERATE_BINDINGS=1`, and bounded Swift/
  Clang module caches (outside the outer sandbox for SwiftPM): app compile completed successfully.
  This pass preceded the final non-blocking retired-worker refactor; no later app compile result was
  obtained before handoff.
- PASS — `cargo test -p insight --test meeting_notes --locked`: 7 passed, including caller
  cancellation reaching the provider, cache identity, exact-window citation validation, and
  adversarial owner/due-date citations.
- PASS — focused RAG persistence test
  `session_catalogue_is_unique_newest_first_and_preserves_completion`: 1 passed.
- PASS — `cargo fmt --all -- --check`.
- PASS — `git diff --check`.
- INCOMPLETE — focused app Notes tests were attempted twice on the existing target but produced no
  result before user interruption (first after about 299 seconds, second after about 138 seconds).
  Do not count these as passed; investigate the hanging test/build process and rerun with a bounded
  individual-test command.
- NOT RUN — app clippy after the final refactor.
- NOT RUN — signed app, real persisted meeting, citation usability, visual calm, or any other manual
  product acceptance. Those remain T044/T035 gates and are not implied by automated fixtures.

### Review-blocker fix — 2026-08-12

- The session worker now publishes its exact live `SessionId` before any event from that session can
  enter the shared UI timeline. The board observes that stable identity and filters every retained
  or later-arriving event by it, so a second meeting cannot replay the first meeting's cards.
- Workspace switching is identity-based and happens once per new live session. Repeated
  `ProvisioningModel`, progress, `Running`, and `Stopping` notifications do not recreate the board
  or move its suffix boundary.
- PASS — two-consecutive-session exact-filter regression, including a late first-session event:
  1 passed.
- PASS — switch-once regression across absent, repeated first-session, terminal, and repeated
  second-session observations: 1 passed.
- PASS — focused Notes UI/controller tests: 8 passed.
- PASS — full app library tests with runtime shaders: 65 passed.
- PASS — app runtime-shader `cargo check`.
- PASS — app all-targets `cargo clippy -- -D warnings` with runtime shaders.
- PASS — `cargo fmt --all -- --check` and `git diff --check`.
- NOT RUN — signed/manual T035/T044 gates remain unchanged.

### Independent final review — 2026-08-12

Accepted T038's automated implementation scope. The worker emits the exact live `SessionId` on
its generation-fenced lifecycle channel before it can forward session events; stale-generation
identity, completion, and disconnect updates cannot mutate the active or completed identity. The
workspace switches the live board once per distinct active id. `BoardState` scans the shared
append-only seam but admits only events whose session id exactly matches that board, including
when an old session's tail arrives after a later session is active. Persisted review still reloads
the exact selected session, and citation focus remains keyed by `(SessionId, EventId)`. Notes
generation remains pinned, cancellable, stale-result fenced, and non-blocking for retired workers;
disabled, generating, generated, cached, failed, and retry states remain truthful. Reviewer reruns
passed both cross-session regressions, the RAG catalogue regression, all 65 app library tests,
runtime-shader app check, all-target app clippy with warnings denied, and scoped diff check. Signed
real-session usability and visual acceptance were not run and remain exclusively T035/T044 work.

### Owner live-UI correction — 2026-08-12

The first owner capture exposed two deterministic workspace issues. A new live session populated
the Board while leaving the visible lens on the empty post-call Notes state, so the transcript was
present but hidden. The workspace now switches to Board exactly once when a distinct active
`SessionId` arrives; repeated lifecycle updates do not override a later manual lens choice. Notes
catalogue startup also creates a missing database parent before opening SQLite, removing the
misleading retrieval-storage error produced by a fresh absolute `SOTTO_DATABASE` path. Focused
regressions and the full app library suite pass (80 tests); strict app Clippy passes.
