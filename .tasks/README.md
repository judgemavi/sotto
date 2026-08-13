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

The original scaffold through pipeline waves are complete except for explicitly tracked
manual residuals. Task-file status is authoritative; this table records the intended
handoff order so a blocked task is not mistaken for unowned work.

| Wave | Track | Tasks | Current state and handoff |
|---|---|---|---|
| M0a | map tier | T012, T022, T023, T033 | Done: diagnostic/session/provisioner/build slices were accepted and handed product integration to T032. |
| M0b | map tier | T032 | Done: automated Start/Stop, managed Whisper, truthful lifecycle, and persistence integration accepted; app session/settings ownership released to T027. |
| M1 | historical canvas | T016 | Done and superseded by ADR-0015/T048: automated canvas work remains, while unrun canvas-specific manual gates are retired rather than claimed. |
| M2 | record acceptance | T035 | Todo and blocking ship: real signed-app picker, provisioning, scoped audio/transcript, terminal/persistence, no-key transcript review, and 10-minute resource observations. |
| MV | map validation | T019 | Blocked: meeting-general migration is implemented with focused automated evidence; no independent or participant acceptance is claimed, and real validation awaits T035/T029. |
| R0 | reasoning | T024 | Done: OpenAI-first extensible backend contract and ADR. |
| R1a | reasoning | T025, T030 | T030 returned FAIL: Codex 0.147.0 has no supported zero-built-in-tools contract, so its default hardened descriptor remains unavailable. |
| R1c | reasoning | T047 | In review: explicit, off-by-default, notes-only Codex subscription mode preserves T030 FAIL while allowing a user-authorized ChatGPT login with no Sotto API key. |
| R1b | reasoning | T026, T028 | Done: Responses adapter and transcript-first/on-demand inspection accepted. |
| R2 | reasoning | T027, T031, T034 | Done: no-reasoning/OpenAI settings, request transport, registry, and credential seams are accepted. |
| R3 | reasoning | T029 | Automated CLI cutover/privacy/provenance/eval slice accepted; in review pending T035 and live Keychain-backed OpenAI evidence. |
| N0 | meeting copilot | T036 | Done: Notes-first product contract and MCP trust boundary accepted in ADR-0011. |
| N1 | meeting notes | T037 | Done: general cited MeetingNotes derived artifact independently accepted. |
| N2 | notes product UI | T038 | Done historical foundation: meeting catalogue, cited review, exact-session replay, and cancellable cached-generation states were accepted; T048 replaces its mounted Board lens. |
| N3 | workspace simplification | T048 | In review: the mounted Board lens is replaced by one top-down live transcript, cited notes, and MCP context workspace under ADR-0015. |
| C0 | MCP context | T039 | Done: bounded HTTP resources-first context plane independently accepted; stdio stays disabled under T046. |
| C0b | MCP stdio | T046 | Done with FAIL verdict: bounded framing works, but a `setsid` descendant escapes process-group containment; stdio remains disabled. |
| C1 | MCP control UI | T040 | Done: HTTPS connections, Keychain credentials, explicit per-session resource/disclosure grants, immutable run identity, and transactional source changes independently accepted; stdio remains unavailable after T046 FAIL. |
| C2 | grounded notes | T041 | Done: immutable MCP grants now produce bounded meeting/external/mixed cited notes with atomic durable receipt replay and transcript-only degradation. |
| C3 | local knowledge | T045 | Done: one-way generic resource/project/meeting taxonomy, exact local receipts, bounded scoped retrieval, and session retention were independently accepted. |
| P0 | proposal schema | T042 | Done: one-way generic proposal events, contextual captured-fact anchors, stable streaming supersession, dispositions, and terminal run audits independently accepted. |
| P1 | proposal engine | T013 | Blocked on T035/T029 live gates; T039/T041 evidence and T042 proposal-event contracts are accepted. |
| P2 | proposal UI | T043 | Blocked on T013/T040/T042: anchored cards and source receipts. |
| E0 | AI acceptance | T044 | Blocked on T035/T029 and the Notes/MCP/Proposal product slices. |

The evaluated v1 reasoning target was Codex CLI plus the OpenAI Responses API. T030 returned
FAIL because the installed supported protocol cannot guarantee an empty model-visible built-in
tool inventory. T047 now permits a separately acknowledged, off-by-default experimental Codex
subscription path while preserving that warning; direct OpenAI API remains the supported optional
BYOK path. Completed T007/T015/T017 remain
historical evidence and are superseded by T024–T031 where their architecture conflicts.

T016's canvas work is retired from the mounted product by ADR-0015/T048. T031's sequential
request-contract work is done. T032's automated integration is done and has released `main.rs`,
`session/**`, and `settings/**`; T027/T034 are accepted and T030's FAIL verdict is recorded. T029's
credential-free automated slice is accepted, but its live evidence and final verdict remain
blocked on the T035 real-session gate.

The local-record gate remains real: focused automated tests do not prove a signed app can capture a
readable conversation. T035 owns that explicit manual ship verdict, including the ten-minute
no-key transcript session and resource observations.

AI product work now follows ADR-0011: cited meeting notes first, read-only MCP context second, and
quiet optional proposals third. T037 and T039 are the only immediate parallel implementation pair.
Realtime proposal work does not start until the recorded conversation is accurate and readable in
the transcript. The local record remains a shipping no-key product tier and the factual source for every
derived note or proposal.

## Reporting back

On completion the implementer updates `**Status:** in-review` in its own task file
(that file is always implicitly owned by its assignee) and appends a `## Notes`
section: what was built, what deviated from the plan, what the next task needs to know.
Deviations from `AGENTS.md` decisions need an ADR in `/docs/adr/` — see T001 for the
template.
