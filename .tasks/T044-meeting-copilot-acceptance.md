# T044 — Meeting-copilot acceptance and evals

**Status:** blocked

**Wave:** E0 — AI product gate

**Depends on:** T029; T035; T038; T041; T013; T043; T045

**Owns:** `crates/cli/src/meeting/**`, `crates/cli/tests/meeting_*`,
`fixtures/meeting/**`, `docs/experiments/meeting-copilot-acceptance.md`,
`.tasks/T044-meeting-copilot-acceptance.md`

## Goal

Prove on accepted real meetings that Sotto produces useful, faithful notes and restrained,
source-grounded proposals without weakening the local record or MCP trust boundary.

## Plan

1. Run cited notes against a T035-accepted meeting through the Keychain-backed OpenAI path.
2. Have a participant verify overview, decisions, actions, owners/dates, open questions, and
   citation navigation.
3. Evaluate transcript-only and MCP-enriched notes separately, including source unavailability
   and malicious-resource fixtures.
4. Replay proposal fixtures and real meetings; measure precision, quiet rate, missed-useful rate,
   annoyance, latency, cancellation, and source fidelity.
5. Verify no-reasoning and no-source modes remain complete and truthful.

## Contract for downstream tasks

This is the ship gate for AI notes and proposals. T035 remains the separate signed capture/map
gate and cannot be inferred from these results.

## Acceptance

- Participant-verified notes have valid moment-level citations and no unsupported owner/date.
- Captured meeting-event rows and bytes are identical before and after every reasoning run. Notes,
  clustering, and MCP synthesis append no timeline events. Proposal runs may append only typed
  proposal/system-output audit events and never modify or supersede pre-existing captured evidence.
- MCP evidence ids resolve to the exact approved source receipts; injected instructions do not
  alter policy or cause disclosure/action.
- Proposal false-positive/quietness and pause-to-first-token latency are recorded on real inputs.
- A ten-minute no-reasoning run remains useful and performs no provider/MCP contact.
- Every live, manual, and unrun gate is labeled explicitly in the evidence document.

## Out of scope

Side-effecting MCP actions, non-OpenAI providers, Windows, ambient capture, and meeting bots.
