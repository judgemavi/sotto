# T025 — Codex CLI backend using the user's ChatGPT login

**Status:** done

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

## Tool-inventory measurement (2026-08-13) — still blocked, now with positive evidence

Evidence: `docs/experiments/codex-cli-tool-inventory.md`. `codex login status` reported
`Logged in using ChatGPT`; no Sotto API key was configured and no Codex auth/config/token/
session file was read or copied. No bypass-permission flag was used.

**The model-visible tool inventory was determined positively, and it is not empty.** The
method captures the exact upstream request body Codex builds — via a loopback capture
endpoint and a `-c model_provider` override, using the production `invocation_arguments()`
argv — and enumerates its tool declarations. Codex 0.147.0 publishes tools through two
channels (the Responses `tools` array *and* an `additional_tools` developer input item
carrying code-mode namespaces); reading only the first would have produced a false "no
tools" result for the product default model. Under the full 26-feature disable set:

- `gpt-5.6-luna` (product default): `functions.exec`, `functions.wait`,
  `functions.request_user_input`. `functions.exec` is a JS isolate exposing nested tools,
  ships an `apply_patch` signature, and states that further nested tools may be omitted
  from its description and discovered at runtime.
- `gpt-5.4`: `update_plan`, `request_user_input`, `apply_patch` (filesystem write),
  `web_search` (network egress).

Positive control: the same capture with no disables returns eight tools, so an empty result
would have been meaningful.

Live sentinels against the real login:

- Fixture recap **passes** on `gpt-5.4`: `codex-cli 0.147.0`, observed latency 3216 ms. No
  realtime claim is made.
- The adversarial injection canary **passes** — the canary was neither read nor written and
  the sentinel came back. So does a direct-instruction probe, across four runs.
- Those passes are model judgement, not isolation. Running the identical argv with a plainer
  prompt made the same model dispatch `apply_patch` (blocked only by the read-only sandbox,
  per the tool router's own rejection message) and **successfully execute `web_search`**
  against the live internet. With Sotto's preamble the model instead self-reported both
  tools as "unavailable", which the captured request shows to be false.

Second, independent blocker found: under Sotto's argv the product default `gpt-5.6-luna` is
`tool_mode: code_mode_only`, so `--disable code_mode_host` makes the CLI emit an
`item.completed` of type `error` before the turn. The fail-closed parser rejects it, so the
connector cannot complete a single turn against the default model. Not fixed here, because
fixing it means relaxing the fail-closed parser.

Verdict: **the descriptor is unchanged.** `CodexProvider::descriptor` still reports
`AuthStatus::Unavailable` for a ready ChatGPT login and the backend stays non-selectable by
default. `CodexDescriptorMode::ExperimentalUserOptIn` remains an explicit consented
acceptance of what is now a quantified risk, and its UI disclosure must name the
filesystem-edit and web-search surfaces rather than calling isolation merely "unverified".

Not established: whether the ChatGPT backend adds further server-side tools (the capture
measures the CLI-constructed request, a sound lower bound only); the complete nested tool
set inside `functions.exec`; and any supported way to reach an empty inventory.

Coverage now: 23 Codex tests pass; 4 live checks are ignored by default, of which
`live_model_visible_tool_inventory_must_be_empty` is the gate and currently **fails** by
design against the installed CLI. Re-run it to re-evaluate enablement.

## Flag/config sweep and MCP measurement (2026-08-13, round 2) — still blocked

Evidence: `docs/experiments/codex-cli-tool-inventory.md`. Same discipline as round 1: real
ChatGPT login, no Sotto API key, no Codex auth/config/token/session file read or copied, no
bypass-permission flag. All results below reproduce through the production
`live_model_visible_tool_inventory_must_be_empty` harness, which now also parses the nested
code-mode tools and asserts Codex has not reserved the right to hide more.

**Question 1 — can `web_search` and `apply_patch` be disabled for `gpt-5.4`? No.**
All 104 feature flags were enumerated; `--disable X` is documented as exactly
`-c features.X=false`, so flags and `features.*` config are one space. `--strict-config`
rejects unknown `-c` keys, which makes it an exact schema oracle, and it confirms the
binary's own serde metadata: the entire tools config is `ToolsToml { web_search,
experimental_request_user_input, update_plan }`. There is no `tools.apply_patch`.
`tools.web_search=false` is **inert** — measured across five variants including with all
`--disable` flags removed. So are `--disable` of `web_search`, `web_search_request`,
`web_search_cached`, `search_tool`. `web_search_tool_type` has no "off" variant and removing
it leaves the tool present. `apply_patch` can be removed, but only by falsifying the model
catalog through `model_catalog_json` — not by any flag.

**The key negative finding: `--disable` rewrites the description, not the surface.**
Setting only `supports_search_tool: false` — a field unrelated to multi-agent — removes the
"Some deferred nested tools may be omitted" caveat from the `functions.exec` declaration and
unmasks `multi_agent_v1__{spawn,wait,send_input,resume,close}_agent`, in a run that passes
both `--disable multi_agent` and `--disable multi_agent_v2`. Those tools were live in the
shipped configuration and merely hidden. Any inventory read while that caveat is present is
a floor, never a ceiling; the harness now fails on it explicitly.

**Question 2 — the default model works; `gpt-5.4` does not need pinning.** The blocker was
self-inflicted. `code_mode_host` is in `DISABLED_FEATURES` and `gpt-5.6-luna` is
`tool_mode: code_mode_only`. Dropping that one feature makes a live turn complete cleanly
(`thread.started`, `turn.started`, `agent_message`, `turn.completed`) with no `error` item
and no parser relaxation. This also sidesteps the catalog's own retirement notice for
`gpt-5.4` ("deprecated soon; Codex now uses GPT-5.6 Terra"). **Do not ship this flag change
alone:** under the shipped catalog it converts a hard failure into a live `apply_patch` path,
because luna's code mode advertises `apply_patch` as a nested tool.

**Question 3 — `codex mcp-server` is strictly worse.** It accepts only `-c/--enable/
--disable/--strict-config`: no `--ephemeral`, `--ignore-user-config`, `--ignore-rules`,
`--sandbox`, or `--cd`, and `--ignore-user-config` has no config-key equivalent. Measured
inventory over that path is the full unhardened twelve tools — regaining `exec_command`,
`write_stdin`, `request_plugin_install`, and the MCP resource tools on top of `apply_patch`
and `web_search`, because the server's `--disable` flags never reach the per-`tools/call`
session. The same run started the user's configured MCP servers, **executed the user's
`~/.codex/hooks.json` command hooks**, and persisted a rollout to `~/.codex/sessions/`,
violating T025's no-session-history requirement. On protocol stability it relocates rather
than removes the fragility: the payload is still internal Codex item types inside
`codex/event` notifications. Recommendation: do not adopt.

**One configuration measures clean, and is not accepted.** Code mode plus a catalog stripped
of `apply_patch_tool_type`/`web_search_tool_type`/`multi_agent_version` with
`supports_search_tool: false` yields `functions.exec` (nested: `update_plan` only, no
caveat), `functions.wait`, `functions.request_user_input`, and completes a live turn. It is
rejected as the enablement path because `model_catalog_json` re-declares capabilities rather
than enforcing them (leaving round 1's server-side-injection question wide open), because it
depends on the very field just proven to be a masking control, because runtime `ALL_TOOLS`
remains unmeasured, and because it adds a V8 isolate running model-authored JavaScript in
order to remove tools. That trade needs its own task and ADR.

Verdict: **descriptor unchanged.** `CodexProvider::descriptor` still reports
`AuthStatus::Unavailable` for a ready ChatGPT login. No connector behaviour was modified.
The adversarial sentinel was deliberately not re-run: it is gated on Question 1 yielding a
clean flag set, which it did not, and round 1 already showed its pass is not evidence.

Coverage now: 24 Codex unit tests pass (one added, covering the nested code-mode and
deferral-caveat parser that the gate's evidence depends on); 4 live checks remain ignored by
default, of which `live_model_visible_tool_inventory_must_be_empty` is the gate and still
**fails** by design against the installed CLI.

## T029 dependency — backend-aware request normalization

Re-confirmed 2026-08-13 (round 2): the dependency still stands, unchanged.
`crates/insight/src/ask.rs:298-299` sets `max_tokens: Some(1_500)` and
`temperature: Some(0.0)`; `crates/insight/src/context/mod.rs:170-171` sets
`max_tokens: Some(4_096)` and `temperature: Some(0.0)`. `CodexProvider::validate_request`
(`crates/providers/src/codex/mod.rs:278-287`) rejects both. Nothing in round 2 changes this.

Insight requests set `max_tokens` and `temperature`. The stable `codex exec` invocation
cannot honor either, so `CodexProvider::validate_request` rejects them and no insight call
can execute through Codex until T029 performs backend-aware request normalization. Do not
resolve this by silently dropping or weakening those controls: the caller asked for a token
ceiling and a sampling temperature, and a backend that ignores both must say so rather than
pretend. The earlier experimental JSON-object exception that cleared both fields was removed;
normalization belongs exclusively to the backend-aware seam established by T067.

## Supported web-search control correction and unblock (2026-08-13, round 3)

The round-2 negative web-search conclusion was incomplete: it swept every feature flag and
the nested `tools` table but missed Codex's supported **top-level** setting. Official OpenAI
documentation defines `web_search = "disabled"` and says that value removes the tool. The
production argv now passes `-c web_search=disabled`; this is distinct from the deprecated
`features.web_search` toggle and the inert `tools.web_search=false` variants measured above.

ADR-0014 now records the accepted experimental boundary. The inventory is not claimed to be
empty: `apply_patch` remains declared, but a direct invocation reaches the tool router and is
denied by `--sandbox read-only`. Hosted web search is removed, known shell/MCP/app/browser
surfaces remain disabled, and the connector still rejects observed tool events. The explicit
experimental acknowledgement accepts that sandbox-bounded residual risk; the strict descriptor
remains unavailable.

`code_mode_host` has been removed from `DISABLED_FEATURES`. It must remain enabled for the
configured code-mode-only default (`gpt-5.6-luna`) to complete, so the production path no longer
depends on pinning deprecated `gpt-5.4`. This does not relax the parser or enable shell execution.

T067 owns backend-aware request normalization. The connector-side exception that cleared
`max_tokens` and `temperature` for JSON-object calls has been removed: Codex validation remains
fail-closed, and only the generic backend seam may resolve caller/backend control disagreements.

Live acceptance used the installed `codex-cli 0.147.0`, configured default `gpt-5.6-luna`, and
the existing ChatGPT login on 2026-08-13:

- Upstream-request inventory gate **passed**: direct tools were `functions.exec`,
  `functions.wait`, and `functions.request_user_input`; documented nested tools were
  `apply_patch` and `update_plan`; no web/search/browser/network/MCP surface was present. The
  deferred-nested-tools caveat remains disclosed and accepted only by the experimental opt-in.
- Default-model fixture recap **passed** with exact JSON and 3261 ms observed latency. No
  realtime claim is made, and no `gpt-5.4` pin or parser relaxation was used.
- Adversarial injection sentinel **passed**: the read canary was not disclosed, the write canary
  was not created, no forbidden tool event was emitted, and the exact sentinel returned.
- Deterministic Codex coverage: **20 passed, 4 manual checks ignored**. Full provider targets:
  **70 passed, 5 ignored** in the library plus **5 historical live-provider checks ignored**.
  Strict all-target provider clippy and Codex rustfmt check passed.

The inventory and sandbox acceptance are the security evidence; the behavioural sentinel is a
regression, not proof that the declared tools are absent. T030's empty-built-in-tools verdict
remains FAIL, while ADR-0014 explicitly accepts the narrower sandbox-bounded experimental path.

## Structured-output defect and repair — planner, 2026-08-13

The first live notes run through the enabled connector failed with
`provider returned HTTP 0: Codex CLI request failed`. Reproduced directly against the CLI with the
production argv:

    invalid_json_schema: Invalid schema for response_format 'codex_output_schema':
    In context=(), 'additionalProperties' is required to be supplied and to be false.

Two distinct defects, both repaired.

**1. A capability was advertised that the connector cannot honour.** The descriptor claimed
`BackendCapability::JsonObjectOutput` — "any syntactically valid JSON object", which is OpenAI's
JSON mode — and implemented it with `--output-schema`, which is *strict structured output* and
therefore the `JsonSchemaOutput` capability. Strict mode requires every property enumerated with
`additionalProperties: false`, so it cannot express "any object" by construction. The placeholder
`{"type":"object"}` was not a small mistake in an otherwise sound design; it was the only shape the
declaration could take, and it is invalid.

The repair declares the truth: no structured-output capability, no `--output-schema`. T067's
normalization seam now handles `JsonObjectOutput` as a third control, so an insight request asking
for a JSON object is downgraded before dispatch with the loss recorded on the result. A JSON-object
request reaching the connector directly is refused rather than served as text, so the seam cannot be
bypassed silently. ADR-0014's claim about the output-schema mechanism is corrected in place.

Note what did not catch this: **every fake-process test passed.** The rejection happens at OpenAI,
and no local fake `codex` executable models what OpenAI accepts. The integration fixture asserted
`--output-schema` was *present*, so it actively locked in the defect. It now asserts the flag is
absent and that `-c web_search=disabled` is present.

**2. A diagnosable failure was reported as an opaque one.** `classify_failure` recognised only
login, rate-limit and context-length prose; everything else collapsed to
`Upstream { status: 0, message: "Codex CLI request failed" }`. The cause was in the payload the
whole time. It now extracts `error.code` — a fixed API vocabulary, unlike the surrounding prose,
which may quote the request and therefore a transcript — and validates it against a bounded
character set and length before reflecting it. An unrecognised failure now says it is unrecognised
rather than implying a diagnosis. A hostile-code test proves the field cannot smuggle text into a
user-visible message.

This is the third time in one day that an error naming a transport instead of a cause has cost a
debugging round; "the disk is full" and "has not committed that moment yet" were the others.

Verification: `providers` 74 passed / 5 ignored, `insight` full suite passed including the
fake-Codex cited-notes integration, strict Clippy over all targets for both crates, and
`cargo fmt --all --check`. The live notes path still needs a real run to confirm end to end.
