# T047 — Explicit experimental Codex subscription reasoning

**Status:** done

**Wave:** R1c — user-authorized no-API-key reasoning

**Depends on:** T025, T027, T030, T037, T038

**Owns:** `crates/providers/src/codex/**`, `crates/app/src/reasoning/**`,
`crates/app/src/settings/**`, `crates/app/src/notes/view.rs` for the Summarizer capability gate and
backend-neutral copy, `crates/insight/tests/meeting_notes.rs` for the no-key fake-Codex product
request regression, `AGENTS.md`, `docs/adr/0008-openai-first-reasoning.md`,
`docs/adr/0011-meeting-notes-and-mcp-proposals.md`,
`docs/adr/0014-experimental-codex-subscription.md`,
`.tasks/T029-reasoning-backend-integration-evals.md`, `.tasks/README.md`, and this task

## Goal

Let a user explicitly opt into their installed, ChatGPT-authenticated Codex CLI for meeting notes
without a Sotto API key, while preserving T030's truthful FAIL verdict about provable zero-tool
isolation.

## Plan

1. Keep the existing fail-closed descriptor as the default and add a separately named experimental
   descriptor mode that never upgrades a missing or logged-out CLI.
2. Probe `codex --version` and `codex login status` asynchronously without reading credential files,
   blocking app launch, or contacting the model.
3. Persist a versioned explicit acknowledgement plus Codex model id and the Summarizer selection;
   migrate prior settings safely with Codex disabled. Do not expose experimental Codex for the
   realtime Watcher or Suggester roles.
4. Render installed/login/checking/error states and a clear residual-risk disclosure. Codex has no
   API-key field. Selection is impossible until acknowledgement and login readiness are both true.
5. Use the supported output-schema CLI contract for JSON-object reasoning needed by meeting notes;
   keep JSON Schema and image input unadvertised unless separately implemented and tested.
6. Preserve the empty temporary working directory, read-only sandbox, ignored config/rules,
   feature disables, environment scrub, stdin prompt, process cleanup, and fail-closed tool parser.
7. Prove no-key fake-Codex meeting-note generation, persisted selection/reload, cancellation, stale
   probe fencing, capability truthfulness, and removal of acknowledgement.

## Acceptance

- An installed fake CLI with ChatGPT login reports authenticated readiness; it becomes selectable
  as the Summarizer only after explicit experimental consent.
- Missing CLI, logged-out CLI, and probe failure remain actionable and cannot resolve.
- Codex can generate cited JSON meeting notes through the product request shape with no Sotto API
  key; a tool event fails the call.
- Experimental Codex is selectable only for the Summarizer role; realtime proposal suitability and
  latency remain unclaimed.
- No auth file/token/key is read, copied, logged, or persisted by Sotto.
- Cold launch remains no reasoning and does not run a model call.
- T030 and its evidence remain unchanged as a FAIL; UI and docs do not say Codex is tool-free.
- Focused provider and app tests, strict Clippy, formatting, and independent review pass.

## Out of scope

Declaring Codex tool-free, enabling Codex by default, realtime proposal suitability, screen-image
input, MCP tool delegation, reading Codex credentials, or weakening the sandbox and event parser.

## Notes

- 2026-08-12 implementation: the installed `codex-cli 0.147.0` and its ChatGPT login are now
  detected through the documented version/login-status commands. Authenticated readiness is
  distinct from the persisted experimental acknowledgement. The default descriptor remains
  unavailable under T030; only the acknowledged connector advertises JSON-object output.
- The product selection is intentionally notes-only. Watcher/Suggester reject Codex, the Settings
  UI exposes it only for Summarizer, and the disclosure names transcript egress to OpenAI plus the
  residual unproven-zero-tools risk. Cold reload survives an unrelated OpenAI Keychain failure,
  while logout, consent removal, stale probes, and cancellation remain fenced.
- Automated evidence on the settled tree: focused provider Codex tests passed 22 with 2 manual live
  canaries ignored; full insight tests passed 29 including a fake authenticated Codex executable
  producing cited meeting notes without an API key; full app lib tests passed 78 with
  `gpui/runtime_shaders`; strict all-target app and insight Clippy passed with warnings denied;
  workspace format and scoped diff checks passed. No quota-using Codex generation, real meeting,
  screen-image transfer, or realtime proposal run was performed.

## Independent review — accepted (2026-08-12)

- Confirmed authentication readiness remains distinct from explicit experimental consent and from
  T030's failed zero-tool isolation result. The default descriptor stays unavailable; only a ready,
  acknowledged Summarizer can resolve, and Watcher/Suggester reject Codex.
- Confirmed the acknowledged JSON-object path uses an isolated `--output-schema` file, normalizes
  only unsupported note tuning controls, keeps strict text calls unchanged, rejects stop/schema/
  image shapes, and retains the fail-closed tool-event and subprocess cleanup boundaries.
- Confirmed settings persist only the versioned acknowledgement, model id, and role selection;
  no API key is required. Startup remains no reasoning, probes do not call a model, stale results
  are fenced, logout becomes unavailable, and Codex reload remains independent of OpenAI Keychain.
- Confirmed Notes gates on `JsonObjectOutput`, the fake authenticated Codex path produces validated
  cited meeting notes through `MeetingNotesGenerator`, and the UI discloses transcript plus enabled
  source-excerpt network egress and the residual unproven-zero-tools risk.
- Fresh review evidence: provider Codex tests passed 23 with 2 manual live tests ignored; the focused
  fake-Codex cited-notes test passed; app reasoning tests passed 21; strict all-target provider/app
  Clippy, workspace formatting, and scoped diff checks passed. No live Codex generation, signed-app
  meeting, screen-image transfer, realtime proposal, or visual UI acceptance was run or claimed.
- Post-review owner reproduction found that `codex-cli 0.147.0` exits zero but prints both its
  PATH-alias warning and `Logged in using ChatGPT` to stderr. The probe had matched authentication
  mode only on stdout, causing a false signed-out UI. It now checks both bounded captured streams
  while still requiring successful exit and the exact ChatGPT mode. The exact stderr shape has a
  regression; focused Codex tests remain 23 passed with 2 quota-using manual canaries ignored, and
  strict provider Clippy passes.
