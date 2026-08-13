# T025 — Codex CLI backend using the user's ChatGPT login

**Status:** blocked

**Wave:** R1 — first no-API-key reasoning path

**Depends on:** T024

**Owns:** `crates/providers/src/codex/**`, Codex-specific provider tests and fixtures;
`crates/providers/src/lib.rs` and `crates/providers/Cargo.toml` only after T024 hands
them off

## Goal

Run Sotto reasoning through a user-installed, already authenticated Codex CLI. Sotto
must never ask for an OpenAI API key on this path and must never inspect Codex credential
storage.

## Plan

1. Probe executable/version and official login status, reporting `Unavailable`,
   `NeedsLogin`, or `Ready` without reading auth files.
2. Start with stable `codex exec --json`; use app-server only if a measured spike and
   ADR update justify depending on its experimental protocol.
3. Feed prompts over stdin and parse machine-readable JSONL into Sotto deltas, usage,
   structured results, and actionable failures.
4. Run ephemeral in an isolated empty working directory with user configuration,
   project instructions, rules, hooks, MCP, and persistence disabled where the CLI
   contract permits. Never use bypass-permission flags.
5. Treat transcript, OCR, and screen text as untrusted data. Prove they cannot cause
   filesystem reads/writes, shell execution, inherited tools, or connector use. If that
   isolation cannot be established, block this backend rather than relying on prompting.
6. Cancellation must terminate the child process tree and reap it. No zombie process
   may survive a cancelled speculative call or application shutdown.
7. Test with a deterministic fake `codex` executable; keep the real-login canary ignored
   and explicitly user-run.

## Contract for downstream tasks

The connector implements T024's backend contract and the existing
`CompletionProvider`. Consumers receive no subprocess or Codex protocol types.

## Acceptance

- Normal, malformed, partial, auth-failure, slow, and cancelled JSONL streams are tested.
- An existing Codex login produces a fixture recap with no Sotto API key configured.
- Sotto never reads or copies Codex auth files or tokens.
- A prompt-injection sentinel cannot read/write a canary file or invoke inherited tools.
- Cancellation kills and reaps the full process tree.
- No prompt, credential, or secret is written to logs or Codex session history.
- The live canary reports CLI/model/version and observed latency without claiming the
  realtime one-second budget.

## Out of scope

OpenAI API access, settings UI, Claude Code or other CLIs, and a realtime suitability
verdict.

## Notes

- Added `CodexProvider` behind `core::CompletionProvider`. Discovery uses only
  `codex --version` and `codex login status`; it never opens or copies Codex config,
  auth, token, or session files. The probe distinguishes missing CLI, required login,
  non-ChatGPT login, and ready ChatGPT login.
- Every reasoning invocation uses this exact shape, with the prompt supplied only on
  stdin: `codex exec --json --ephemeral --ignore-user-config --ignore-rules --sandbox
  read-only --skip-git-repo-check --color never --cd <new-empty-tempdir> --model
  <model> [--disable <feature>...] -`. It never uses either dangerous bypass flag.
- Explicit feature disables cover shell/unified exec, apps, hooks, plugins, MCP
  elicitation/install paths, browser/computer/image surfaces, memories, skills,
  multi-agent, tool suggestion, view-image, and workspace dependency discovery. The
  child environment is cleared and rebuilt from a small operational allowlist; API
  keys, access tokens, and unrelated secrets are not inherited.
- JSONL normalization covers multiple agent-message events, final usage, malformed
  output, auth/rate/context failures, and forbidden tool events. Unknown top-level
  protocol events now fail as decode errors, while unknown item types invalidate the
  isolation assumption; separate unit and fake-process tests prove both fail-closed
  paths. The stdout reader caps each read at one byte beyond the 1 MiB event contract,
  so a newline-free event cannot allocate without bound before rejection; a fake CLI
  emits an oversized no-newline payload to prove this path. Raw stderr and provider
  prose are not returned or logged because either may contain prompt text.
- Stdout and stderr drains start before any prompt bytes are written. Prompt write plus
  stdin shutdown select on both explicit cancellation and receiver drop, so a child
  that never reads a prompt larger than the OS pipe cannot strand the supervisor. Each
  child receives a new process group; cancellation sends TERM, waits, escalates to
  KILL, and reaps the parent. Separate greater-than-pipe tests prove cancellation and
  consumer drop kill a fake descendant while stdin is blocked.
- Successful process EOF is accepted only after an explicit `turn.completed`; truncated
  output now ends in a decode error rather than a fabricated `EndTurn`. Stderr is
  drained through EOF to prevent child blockage while only the first bounded diagnostic
  prefix is retained for classification.
- Version and login probes now use the same kill-on-drop process-group discipline as
  reasoning calls. A timeout regression proves the direct child is reaped and its
  delayed descendant cannot survive.
- `max_tokens`, `temperature`, and stop sequences are explicitly rejected because the
  stable CLI invocation cannot honor them. Per-call model identity must also equal the
  configured descriptor model. Current insight requests set `max_tokens` and
  `temperature`, so T029 must perform backend-aware request normalization before a
  Codex call can execute; silently weakening those controls is not allowed.
- Codex advertises neither `JsonObjectOutput` nor `JsonSchemaOutput`. Prompt-shaped JSON
  is not constrained output, and `--output-schema` has not been wired. This capability
  honesty is independent of the real no-tools blocker.
- Deterministic fake-executable tests prove argv, stdin, environment, ephemeral/tempdir
  use, parsing, normalized errors/usage, and process-tree cancellation. They do **not**
  prove what tools the real Codex service exposes to the model.
- The installed Codex 0.147.0 CLI has `--ignore-user-config`, `--ignore-rules`,
  `--ephemeral`, and feature toggles, but `exec` has no documented, direct "no tools"
  mode. `--sandbox read-only` still permits filesystem reads and shell execution.
  Disabling `shell_tool` and `unified_exec` (plus the other surfaces) is defense in
  depth, not protocol evidence that the model-visible tool list is empty.
- For that reason, even a successful ChatGPT login produces a backend descriptor with
  `AuthStatus::Unavailable`. The connector is intentionally not product-selectable.
  Enabling it requires evidence for the real CLI/model tool inventory plus the ignored
  adversarial canary; prompt instructions or absence of an observed tool event are not
  sufficient.
- Two ignored manual checks remain: a no-Sotto-key fixture recap that reports CLI
  version and latency, and an adversarial read/write/shell/MCP/app/browser/tool sentinel.
  They were not run and no one-second realtime claim is made.
- Verification: 18 Codex tests pass and 2 Codex live checks are ignored; strict
  all-target provider clippy and provider formatting pass. The full provider suite
  passes 47 unit tests with 3 manual checks ignored, plus the 5 historical provider
  live checks ignored. Existing localhost transport mocks required the approved
  unsandboxed run.

## Review — connector hardening addressed; product block remains

The blocked descriptor prevented the review findings from reaching product use. The
following connector hardening is now implemented with deterministic regressions:

- Unknown protocol and item types fail closed.
- Cancellation and receiver drop interrupt blocked prompt writes.
- Only `turn.completed` produces `EndTurn`.
- Timed-out probes kill and reap their process group.
- Stderr drains fully with bounded diagnostic retention.
- Unsupported request controls are rejected explicitly.
- JSON-object and JSON-Schema capabilities remain absent.

These fixes do not relax the independent blocker: the real installed CLI still lacks
protocol evidence that the model-visible tool inventory is empty. The ignored
adversarial live canary also remains unrun. T025 therefore stays `blocked`, and even a
future passing code review must not make its descriptor selectable until the no-tools
gate is resolved.

## Isolation-gate handoff

T030 exclusively owns the bounded real-CLI/App-Server protocol investigation. T025's
descriptor remains `Unavailable` throughout that spike. A T030 PASS still requires a
separate enablement review against the pinned protocol/version; FAIL or INCONCLUSIVE
removes Codex from the v1 selectable paths rather than weakening isolation or relying
on prompt instructions.

## Post-T030 consistency audit (2026-08-11)

T030 is complete with a FAIL verdict. The connector remains intentionally experimental and
disabled: a successful `codex login status` may make `CodexProbe` report login readiness, but
`CodexProvider::descriptor` converts that state to `AuthStatus::Unavailable`, so the product
registry cannot resolve it. No non-test product path currently registers the connector. Focused
Codex coverage passed 19 tests with the two manual login/adversarial checks still ignored.

T025 remains `blocked`, not `done`: its required model-visible no-tools proof and live adversarial
acceptance were not achieved. T030 is the completed decision artifact; retaining the blocked state
prevents a hardened but disabled connector from being mistaken for an accepted product backend.
