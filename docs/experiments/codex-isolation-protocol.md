# Codex model-visible no-tools isolation verdict

- Date: 2026-08-11
- Verdict: **FAIL**
- Tested runtime: `codex-cli 0.147.0`
- Authentication: `codex login status` reported `Logged in using ChatGPT`; no Sotto API key
  was configured or read.

## Question

Can Sotto start an authenticated Codex turn through a supported CLI or App Server field that
guarantees the model is offered no filesystem, shell, web, MCP, app, hook, skill, image, or
other execution tools?

The required property is absence from the model-visible request. Read-only sandboxing,
approval denial, an empty working directory, and a prompt asking the model not to use tools do
not satisfy it.

## Official surface inspected

- [Non-interactive mode](https://learn.chatgpt.com/docs/non-interactive-mode) documents
  `codex exec`, JSONL output, sandbox selection, and approval behavior. It does not document a
  zero-tools mode.
- [Configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
  documents individual feature toggles such as `features.shell_tool` and
  `features.unified_exec`. These disable named implementations; they do not define or attest to
  an empty model-visible tool inventory.
- [App Server](https://learn.chatgpt.com/docs/app-server) documents `dynamicTools` as custom
  tools supplied by the client. Supplying an empty list prevents additive dynamic tools; it does
  not remove Codex built-ins.

The installed `codex exec --help` likewise exposes `--disable <FEATURE>`, sandbox/approval
controls, ignored configuration/rules, ephemeral sessions, and output schema, but no
`--no-tools` or equivalent contract.

## Installed protocol schema

The installed runtime's experimental schema was generated into a disposable directory with:

```sh
codex app-server generate-json-schema --experimental --out "$TEMP_SCHEMA_DIR"
```

`ThreadStartParams` contains additive `dynamicTools`, sandbox, approval, configuration,
environment, and developer-instruction fields. `TurnStartParams` contains sandbox, approval,
environment, output-schema, and input fields. Neither contains a built-in tool inventory or a
field whose empty value removes all built-ins. App configuration has per-app
`default_tools_enabled` and per-app tool settings; those control connected apps, not Codex's
built-in shell/filesystem tool set.

The schema also exposes shell/command operations and explicitly distinguishes sandbox policy
from tool availability. This reinforces that permissions constrain execution after a tool is
available; they do not prove the tool was absent from the model request.

## Live attempt

A combined disposable sentinel canary was started with the existing ChatGPT login and no API
key. The sandboxed attempt failed before a model turn with an in-process App Server
`Operation not permitted` error. An escalated attempt returned no usable JSONL trace and was
aborted after approximately 855 seconds. It was not restarted, and no absence claim is derived
from a hung call.

The live canary therefore neither strengthens nor weakens the verdict. T030's contract says to
return FAIL when the supported surface offers only per-feature or permission controls. The
official and generated protocol surfaces establish exactly that condition.

## Consequence

- `openai.codex-cli` remains `AuthStatus::Unavailable` even when `codex login status` is ready.
- T027 must not present Codex as selectable. It may explain that the installed CLI lacks the
  required no-tools isolation contract.
- Direct OpenAI Responses is the only v1 cloud reasoning path. No reasoning remains the default
  and the local map tier remains fully functional.
- T029 skips live Codex parity/latency runs and asserts that selecting Codex is rejected.
- Reconsideration requires a documented official CLI/App Server field that controls the actual
  model-visible built-in tool inventory, followed by a new adversarial live canary and review.

T025's deterministic fake tests remain useful for process cleanup, JSONL parsing, secret
handling, and cancellation. They do not convert feature disables into a protocol isolation
guarantee.
