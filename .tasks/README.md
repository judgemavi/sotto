# Sotto task board

Task files are the unit of assignment. One agent per task file. The planner/reviewer
(not the implementer) creates and closes tasks.

## File format

Every task is `Tnnn-slug.md` with:

- `# Title`
- `**Status:**` — one of `todo` | `blocked` | `in-progress` | `in-review` |
  `changes-requested` | `done`
- `## Plan` — the ordered steps the implementer follows

Plus the supporting sections: `Depends on`, `Owns` (file paths this task may write),
`Contract` (what other tasks rely on), `Acceptance`, `Out of scope`.

## Status lifecycle

The status value is machine-readable and must contain exactly one vocabulary value; put
explanations in `Notes` or a review section, never inside the `Status` field.

| Status | Meaning |
|---|---|
| `todo` | Contract is ready, but no implementer has started it. |
| `blocked` | No meaningful implementation progress is possible until a named dependency, external action, or decision changes. |
| `in-progress` | An implementer is actively working the accepted contract. |
| `in-review` | The implementer has handed off a complete claimed slice for independent review. |
| `changes-requested` | Review found concrete work inside the task contract; use this while that hardening is active. |
| `done` | The reviewer accepted the task's owned slice and any remaining product work is assigned elsewhere. |

Do not use `blocked` merely because review found defects. Move `in-review` to
`changes-requested` while those defects are being fixed. A task may be `done` with a
manual or product-integration residual only when that residual has an explicit owner and
the completed task's acceptance has been narrowed in a recorded handoff.

## The parallelism rule

**An agent may only create or modify files under its `Owns` list.**

This is what makes concurrent work safe. Consequences:

- The root `Cargo.toml` is owned by T001 only. T001 pre-registers *every* workspace
  member up front, including crates that are still empty stubs, so no later task ever
  needs to edit it.
- Each crate declares its own dependencies in its own `crates/<name>/Cargo.toml`.
  Do **not** add to root `[workspace.dependencies]` — that file is frozen after T001.
  Duplicate version drift between crates is the planner's problem to reconcile at
  review time, not a reason to touch a shared file.
- Cross-crate types live in `crates/core` and are frozen by T001. If a task needs a
  type change in `core`, it stops and reports back rather than editing `core`.
  The planner amends T001's contract and notifies affected tasks.
- `Cargo.lock` is a shared planner-reconciled artifact. A task changing a crate manifest
  may update the lockfile only when its `Owns` list names `Cargo.lock` for a sequential
  handoff. Never let two concurrent tasks resolve or rewrite it. The planner performs the
  final lock reconciliation after the manifest-owning tasks hand off; generated lockfile
  changes do not transfer ownership of unrelated manifests.

## House lint idiom — read before writing a test

The workspace lint table is **deliberately strict**. Do not weaken it, and do not add
blanket `#[allow]` — `clippy::allow_attributes` is denied, so suppressions must be
`#[expect(..., reason = "...")]`, which self-reports when it goes stale.

Two consequences that will otherwise cost every implementer the same hour:

**There is no warn tier.** `[workspace.lints.rust] warnings = { level = "deny" }` denies
the whole warnings group, so clippy entries written as `"warn"` are emitted as errors —
including `cast_possible_truncation`, `missing_assert_message`, `panic` and
`expect_used`. Treat the entire table as deny. Casts on the audio path (`f32`↔`i16`,
sample counts to `u32`) need explicit handling, not a silent `as`.

**`unwrap()`, `expect()` and `panic!()` are denied in tests too.** `AGENTS.md` permits
them in tests; this workspace does not. The house idiom, verified clean under
`cargo clippy --workspace --all-targets --all-features -- -D warnings`:

```rust
#[test]
fn parses_the_thing() -> Result<(), Box<dyn std::error::Error>> {
    let parsed: u8 = "7".parse()?;
    assert_eq!(parsed, 7, "parse should round-trip");   // message is mandatory
    Ok(())
}
```

`assert!`/`assert_eq!` are fine and do not trip `clippy::panic` — but every assertion
needs a message. Fallible setup uses `?`, not `unwrap()`.

Integration tests under `tests/` additionally trip `tests_outside_test_module`, since
the lint does not special-case integration targets. One inner attribute at the top of
each file, once:

```rust
#![expect(
    clippy::tests_outside_test_module,
    reason = "integration test files compile only under cfg(test)"
)]
```

## The verification rule

A green test suite over an artifact that exercises nothing is the failure mode of this
project. It has happened four times:

| What existed | Why it proved nothing |
|---|---|
| Screen frames captured at 2×2 pixels | every frame was 83 bytes; the frame path was never run |
| Audio fixtures of tone bursts | Silero correctly finds no speech in a tone, so nothing flowed |
| Frame fixtures with no rendered glyphs | OCR had never extracted a character |
| Silero missing its required `sr` input | VAD ran no real inference; tests passed anyway |

Every one passed review. Every one surfaced only by running the thing against real data.

So: **if your crate wraps a model, a device, or an OS API, at least one test must run real
input through it and assert on a known real answer.** Structure assertions over synthetic
input pass whether or not the thing under test does anything at all.

And when a stage produces nothing, say so — on stderr, with the cause. Silence that looks
like success is how all four of these survived.

## Current waves

Accepted task files are pruned from this directory once their slice is closed. They remain in git
history, and the decisions they produced live in `/docs/adr`. The board therefore lists only work
that is live: `todo`, `blocked`, `in-progress`, `in-review`, or `changes-requested`. Task-file status
is authoritative; this table records the intended handoff order so a blocked task is not mistaken
for unowned work.

| Wave | Track | Tasks | Current state and handoff |
|---|---|---|---|
| M0 | capture spike | T002 | In progress. The Swift ScreenCaptureKit bridge, CPAL microphone path, and soak harness are built; the later T061/T050 handoffs have released and completed their scoped edits in `crates/capture/**` and ADR-0002. |
| M2 | record acceptance | T035 | Todo and blocking ship: real signed-app picker, provisioning, scoped audio/transcript, terminal/persistence, no-key transcript review, and 10-minute resource observations. T052 adds its scroll and reflow observations to this gate. |
| M4 | recording | T062 → T064, T065 | T058, T059, T060 and T063 closed 2026-08-13. T062 holds the app-side recording seam; its first slice — the reference persisted before capture starts — is accepted, with the rail refresh and re-transcription gesture outstanding. T063's benchmark was accepted but its conclusion was not: an 11-second studio sample could not separate the models, so T065 re-runs it on real meeting audio. T064 is the maintainer-run signed acceptance gate. |
| M3 | capture | T050 | In review: microphone-only capture, audio-only recording, explicit persisted scope, v10 migration, and single-source transcription are implemented with focused automated evidence. Fresh-profile signed capture, physical-microphone speech, and mounted-control acceptance remain manual `NOT RUN` gates. |
| MV | map validation | T019 | Blocked: meeting-general migration is implemented with focused automated evidence; no independent or participant acceptance is claimed, and real validation awaits T035/T029. |
| R1a | reasoning | T025 | Done 2026-08-13: the explicit experimental path disables hosted web search, accepts declared-but-read-only-denied `apply_patch`, passes the live inventory and injection sentinel, and works with the default model without pinning deprecated `gpt-5.4`. T030's empty-built-in-tools verdict remains FAIL and disclosed. |
| R1b | reasoning | T067 | Done 2026-08-13: backend capabilities now govern centralized, observable request normalization; Codex remains fail-closed at its connector and OpenAI retains supported controls. |
| R3 | reasoning | T029 | In review: automated CLI cutover/privacy/provenance/eval slice accepted; live Keychain-backed OpenAI evidence still pending T035. |
| N5 | workspace finish | T055 | Done 2026-08-13. T060 subsequently closed the deferred rail-title ellipsis and terminal-control clipping defect. T048-T054 closed 2026-08-13 with visual acceptance narrowed and handed here. One window, session bar, design tokens, and the inherited per-frame render cost. `docs/design/workspace-mock.html` is normative for the shipped look. Planner review on 2026-08-13 filed five defects against it, including an empty transcript when reopening a past meeting. |
| N7 | v2 workspace | T072, T073, T074, T075 | Aligning the built UI with `docs/design/workspace-v2-mock.html` under ADR-0019: recording-centric vocabulary, two title bars with a tabbed stopped-session stage, three equal entry points, source-attributed transcript rows, and an adaptive summary. Split by file so all four run concurrently; T072 owns `mod.rs` and registers modules for the others. |
| N6 | UI primitives | T060 | Done 2026-08-13. The control-row primitive makes essential, ellipsizing, and expendable roles explicit; the 680 px regression keeps the clock and clickable Stop in bounds and closes T055's deferred rail-title ellipsis. |
| N5 | workspace finish | T056 | Blocked on T055: annotate a meeting after it ends, anchored to a chosen transcript row. Reverses a T051 contract choice, not a defect. |
| P1 | proposal engine | T013 | Blocked on T035/T029 live gates; T039/T041 evidence and T042 proposal-event contracts are accepted. Planner note 2026-08-14: the watcher's first pass must be local and keyless, reusing T093's classifier infrastructure. |
| P2 | proposal UI | T043 | Blocked on T013/T040/T042: anchored cards and source receipts. |
| E0 | AI acceptance | T044 | Blocked on T035/T029 and the Notes/MCP/Proposal product slices. |
| N8 | entry workspace | T086 → T089 | ADR-0021. T086 (todo) puts the entry above the recording in `core`/`rag` — data model only, no UI. T089 (blocked on T086 + N7 close) cuts the workspace over to entries: prepared entries before capture, record-again into an entry, `docs/design/workspace-v3-mock.html` normative. |
| D1 | living notes | T087, T092 | Blocked on T070/T086 and T066/T070 respectively. T087 makes the summary an editable two-layer document with deterministic regeneration merge over T070's block ids. T092 finishes screen consultation: an inspection budget, Ask's inspector seam, and a disclosure that survives caching. |
| V1 | vault | T088 | Blocked on T086/T087: the markdown vault projection — one `.md` per entry, `^blockid` anchors, `sotto://` deep links; notes two-way through the overlay, the record one-way. |
| B1 | preparation | T090 → T093 | Blocked on the N8/D1 chain. T090 is the pre-meeting brief into a prepared entry (cited, one-shot, deterministic open-items section works keyless). T093 closes the loop: local ONNX commitment/decision detection, suggested action blocks, open items resurfacing in the brief. |
| A2 | ask surface | T091 | Blocked on T077/T069 close: a T077 selection becomes an explicit, bounded Ask scope that never widens silently. |

## Standing decisions from closed work

The scaffold, capture, VAD, ASR, prosody, timeline, pipeline, persistence, notes, MCP and reasoning
waves are accepted and their task files pruned. What still binds current work:

- The evaluated v1 reasoning target was Codex CLI plus the OpenAI Responses API. T030 returned FAIL
  because the installed supported protocol cannot guarantee an empty model-visible built-in tool
  inventory. T047 permits a separately acknowledged, off-by-default experimental Codex subscription
  path while preserving that warning; direct OpenAI API remains the supported optional BYOK path.
- ADR-0015 retired the Board from the mounted product. ADR-0016 (T049) amends only its vertical
  ordering; every other ADR-0015 ruling stands. ADR-0017 (T053) admits the Ask surface as user-
  initiated, always-cited model output.
- `docs/design/workspace-mock.html` is the normative reference for the shipped workspace look.
  Where prose and the mock disagree, the mock wins unless `AGENTS.md` or an ADR overrides both.
- UI tasks must carry a launch-and-render check. Five workspace tasks passed 91 tests, strict
  Clippy, formatting and diff checks while the app aborted on launch from a GPUI double lease; no
  automated test built a render tree.
- ADR-0011 orders AI product work: cited meeting notes first, read-only MCP context second, quiet
  optional proposals third. Realtime proposal work does not start until the recorded conversation is
  accurate and readable in the transcript.
- Notes are generated on user request, not automatically. That is existing accepted behaviour, not a
  pending change.
- `crates/asr` now follows the committed recording prefix behind capture and emits media-time finals;
  the old LocalAgreement stabilizer, rolling ring, and unstable-tail mechanism are removed. T058
  remains in progress on signed real-media acceptance and the model benchmark, not construction.
- ADR-0018: the session recording is the source of truth, transcript time is media time, and screen
  evidence is decoded on demand rather than retained as change frames. It supersedes the frame-store
  mechanism in ADR-0006 while leaving the transcript-first and explicit-inspection rulings intact.
  "Audio is never written to disk" is no longer a true claim; the honest claim is that the recording
  stays on this Mac, is visible, and can be deleted.
- The local-record gate remains real: focused automated tests do not prove a signed app can capture a
  readable conversation. T035 owns that explicit manual ship verdict, including the ten-minute no-key
  transcript session and resource observations.
- The local record is a shipping no-key product tier and the factual source for every derived note,
  answer, or proposal.

- **ADR-0021 (2026-08-14) sets the next product direction:** the entry above the recording
  (documents hang off entries, facts hang off sessions; a recurring meeting is linked entries,
  never one entry accumulating recordings), the notes document as generated-artifact ⊕ append-only
  user-edit overlay with content-addressed block ids and a deterministic — never model-mediated —
  regeneration merge, and a markdown vault projection (Obsidian-compatible, notes two-way, record
  one-way). `docs/design/workspace-v3-mock.html` is normative for entry-era UI tasks (T089+);
  the v2 mock remains normative for the in-flight N7 wave. Emotion inference is now an explicit
  AGENTS.md non-goal; deterministic prosody is the permitted layer.
- **Codex is the only reasoning backend for now** (maintainer, 2026-08-14). Other providers stay
  in `crates/providers` but are not surfaced in Settings until deliberately added later. The v2 mock
  originally offered a local-model option and claimed "use a local model and nothing leaves at all";
  that claim was removed rather than shipped, because the Ollama path exists in the provider layer
  but is unreachable from the app. A design reference that promises an unbuilt capability is how
  "audio is never written to disk" survived past the point it was true.

## Reporting back

On completion the implementer updates `**Status:** in-review` in its own task file
(that file is always implicitly owned by its assignee) and appends a `## Notes`
section: what was built, what deviated from the plan, what the next task needs to know.
Deviations from `AGENTS.md` decisions need an ADR in `/docs/adr/` — see T001 for the
template.
