# T035 — Manual signed-app map-tier acceptance

**Status:** todo

**Wave:** M2 — blocking map-tier ship gate

**Depends on:** T032 (done automated product integration); T048's top-down transcript workspace;
a signed/notarized macOS acceptance build and a real selectable call target

**Owns:** `.tasks/T035-manual-map-tier-acceptance.md`,
`docs/experiments/map-tier-manual-acceptance.md`. No production code.

## Goal

Establish with a real signed app, real devices, and a real scoped conversation that the no-key map
tier works as a product. This task records observation; it does not infer OS, network, audio,
performance, or readability behavior from mocks and does not repair defects in production code.

## Plan

1. Start from a cold app launch and record that Sotto remains idle: no picker, capture, model
   provisioning, session resume, Codex process, or OpenAI request occurs before the user presses
   Start. Press Start, cancel the system picker, and verify there is no session side effect.
2. With the managed `base.en` final and partial absent, select a target and observe real download
   progress. Cancel once during download, resume, and record final size/digest verification. Repeat
   from a deliberately corrupt managed final: record its quarantine path and replacement attempt.
   If the replacement service is unavailable, expect `OfflineNoCache` and retain quarantine
   evidence when available rather than expecting a separate corrupt-cache runtime state.
3. Disconnect the network after a verified model exists, relaunch, select a target, and prove the
   cached model is reused without network access. Keep `SOTTO_WHISPER_MODEL` unset for the managed
   path; record any explicit override run separately.
4. Run a real two-party conversation with microphone input and the system-picker-selected target's
   audio. Confirm real timestamped partial and final utterances, correct You/Meeting audio attribution,
   and useful transcript content. Record the exact picker target and the actual audio scope stated
   by the app; do not call system-wide audio application-scoped.
5. For the full Running interval, verify the non-disableable indicator names the selected screen
   target and distinguishes microphone from actual target/application/display/system-wide audio.
   Exercise user Stop, chosen-target close, macOS Stop Sharing, and one safe failure path in
   separate sessions; each must clear the indicator once, return to idle, and never auto-restart.
6. Reload each completed session and compare its persisted `CaptureTarget`, end time/outcome, and
   event tail with the live observation. Confirm the last partial/final and terminal events are not
   lost during bounded shutdown.
7. Use the plain transcript live and again in post-call review. Record whether timestamp order,
   neutral speaker labels, rolling-partial replacement, wrapped text, Follow live behavior,
   persisted replay, and citation focus are accurate, calm, and readable. List defects and severity;
   a subjective pass/fail sentence is required.
8. Complete at least one ten-minute conversation with no Codex installation usable by Sotto, no
   OpenAI API key, and no reasoning backend selected. Record that Start, transcription, timeline,
   Stop, persistence, and review remain complete with no reasoning error state.
9. During the real run, record process CPU and RSS at minimum at idle, 1 minute, and 10 minutes.
   Record transcript row counts and whether following remained responsive. Canvas scheduling,
   thumbnail residency, GPU, no-reflow geometry, and T016's former long-run gates were retired by
   ADR-0015 and must not be presented as current product acceptance.

## Evidence contract

Follow and complete `docs/experiments/map-tier-manual-acceptance.md`; it is the exact command
runbook and evidence schema for this gate. It includes recoverable model-cache preparation,
signed/notarized artifact verification, process/SQLite/log commands, terminal-path rows, the
transcript readability rubric, and idle/1/10-minute measurements. Write the resulting evidence
with build identity, signing/notarization identity, macOS/hardware, model artifact/digest, network
state, selected target, actual audio scope,
session ids, timestamped observations, terminal outcomes, persistence checks, transcript judgement, and
the measurement table. Redact participant content and secrets; do not attach raw call audio or
unredacted screen frames.

Each scenario is `PASS`, `FAIL`, or `NOT RUN`. A failure is recorded honestly and routed to a new
implementation task owning the affected production files; T035 itself remains evidence-only. Do
not mark this task done while any required scenario is `NOT RUN` or `FAIL`.

The historical board interval harness is not part of this gate. Use process observations and the
subjective transcript rubric; do not infer visual acceptance from automated tests.

## Acceptance

- Real cold launch and picker-cancel behavior pass with no session/capture/download side effects.
- First managed download cancel/resume/integrity, corrupt-final quarantine/replacement, and cached
  offline reuse pass with reachable error semantics recorded accurately.
- Real scoped mic plus selected-target audio produces useful timestamped partials/finals with
  correct speaker attribution, and the indicator describes screen and actual audio scope truthfully.
- User Stop, target close, Stop Sharing, and the exercised failure path terminate once, clear the
  indicator, persist target/end/tail, and never restart automatically.
- Live and replayed transcripts are judged accurate, calm, and readable, with defects recorded.
- A ten-minute no-key/no-Codex/OpenAI session completes through persisted review.
- CPU and RSS observations are recorded at idle, 1 minute, and 10 minutes; transcript row count and
  Follow live responsiveness are recorded explicitly rather than inferred.
- The evidence document contains no inferred manual result, secret, raw audio, or unredacted frame.

## Contract for downstream tasks

T013 and release planning remain blocked on this map-tier ship verdict as well as their other named
dependencies. T019/T029 may use only sessions whose relevant scenarios passed.

## Out of scope

Production-code fixes, provider/reasoning validation, advisor quality, OCR or image attachment,
model-quality benchmarking beyond usefulness of the observed transcript, or changing capture scope.

## Independent runbook review — 2026-08-11

The runbook now fails fast, validates absolute artifact/model/evidence paths, and uses no-clobber
moves for every cache transition. Deliberate corruption is allowed only after the verified final's
size and digest are recorded and its move into the unique backup succeeds. Repeated runs use unique
backup/evidence directories, and restoration stops on any destination collision instead of
overwriting user data. Evidence labels now say `eligible_thumbnail_paths`, not residency.

T035 remains `todo`. This review ran no signed app, picker, model download, network transition,
real call, Keychain mutation, performance measurement, or manual acceptance scenario.
