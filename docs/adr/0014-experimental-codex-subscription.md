# ADR-0014: Explicit experimental Codex subscription reasoning

- Status: Accepted
- Date: 2026-08-12
- Decision owners: Sotto maintainers

## Context

T025 built a hardened Codex CLI connector that uses the CLI's own ChatGPT authentication. T030
correctly returned FAIL for a narrower security question: Codex CLI 0.147.0 has no supported field
that proves an empty model-visible built-in tool inventory. The product consequently required an
explicit experimental acknowledgement even when `codex login status` reported a valid ChatGPT
login.

That fail-closed default made an API key the only usable reasoning path and contradicted the product
goal that a user with an existing Codex subscription can opt into reasoning without another
credential. Authentication readiness and a proof of zero tools are different facts; the UI should
show both instead of collapsing a valid login into "unavailable."

Official OpenAI documentation defines `codex login status` as the supported authentication check,
with exit status zero when credentials are present. It also documents `codex exec`, read-only
sandboxing, per-feature disables, ephemeral execution, ignored user configuration/rules, JSONL, and
output schemas. It does not document one switch that guarantees an empty model-visible tool list.
It does document the top-level `web_search = "disabled"` mode as removing the web-search tool. This
is a different setting from the deprecated feature toggle and from `tools.web_search=false`, which
the earlier inventory sweep measured instead.

The corrected upstream-request capture establishes two narrower facts. First,
`-c web_search=disabled` removes `web_search` from both non-code-mode and code-mode requests.
Second, the product default model needs `code_mode_host`; disabling it makes a code-mode-only model
fail before producing an answer. Keeping it enabled leaves `apply_patch` declared. A direct probe
reached the patch router, where the read-only sandbox rejected the write. That is positive evidence
of sandbox enforcement, not evidence that the tool is absent.

## Decision

Sotto adds an explicit, off-by-default **Codex subscription — experimental** mode:

- Sotto probes only `codex --version` and `codex login status`; it never reads or copies Codex auth
  or configuration files and never asks for an API key on this path.
- A successful ChatGPT login is shown as authenticated readiness. Missing CLI, login required, and
  probe failure remain distinct actionable states.
- The user must persist an explicit acknowledgement before Codex can be selected as the meeting
  notes Summarizer. Removing that acknowledgement deselects Codex without affecting the local
  meeting record. Watcher and Suggester remain unavailable because realtime proposal quality and
  latency have not been accepted for this experimental connector.
- Every invocation remains ephemeral, uses a new empty working directory and read-only sandbox,
  ignores user configuration and rules, disables known tool surfaces, scrubs the environment,
  sends the prompt over stdin, passes `-c web_search=disabled`, and rejects any observed tool event.
- `code_mode_host` remains enabled so the configured default model can complete a turn without a
  deprecated `gpt-5.4` pin. This is not treated as a general execution grant: shell/unified exec,
  apps, hooks, plugins, MCP, browser, computer, and other known surfaces remain disabled.
- The explicitly acknowledged experiment accepts `apply_patch` as declared but sandbox-denied.
  The safety boundary is the read-only sandbox plus fail-closed tool-event handling, not model
  obedience and not a claim of an empty inventory. This acceptance is limited to post-call derived
  views; it does not upgrade Codex into a general untrusted-code executor.
- The UI states plainly that these controls reduce authority but do not prove that the model sees
  zero tools. T030 remains a valid FAIL result and the strict/default descriptor remains
  unavailable; the acknowledged experimental descriptor is selectable when login is ready.
- **The Codex connector advertises no structured-output capability, and passes no
  `--output-schema`.** An earlier revision of this decision claimed JSON-object calls used the
  CLI's output-schema mechanism. That was wrong in a way worth recording, because the shape of the
  error is likely to recur: `--output-schema` is *strict* structured output. It requires every
  property enumerated and `additionalProperties: false` at each level, so it cannot express "any
  JSON object" at all — that is OpenAI's separate JSON mode. Advertising `JsonObjectOutput` and
  satisfying it with a placeholder `{"type":"object"}` schema made every live notes run fail
  upstream with `invalid_json_schema`, while every fake-process test in the suite passed, because
  the rejection happens at OpenAI and no local fake models it.

  The connector therefore declares only what it can honour. Requests asking for a JSON object are
  downgraded at the single normalization seam, the loss is recorded on the result, and the reply is
  prompt-shaped JSON parsed defensively by insight. A JSON-object request reaching the connector
  directly is refused rather than quietly served as text.

  Constrained output can be revisited by supplying a real schema for each concrete response type
  and advertising `JsonSchemaOutput`. That is a different capability with a different contract, and
  it needs a schema derived from the response types rather than hand-written beside them.
- No reasoning remains the cold default. Direct OpenAI Responses remains the supported BYOK path.

Codex is suitable initially for post-call notes and other non-realtime derived views. No realtime
latency or proposal-quality claim follows from this opt-in.

## Consequences

- A user already logged into Codex through ChatGPT can generate notes without configuring a Sotto
  API key, subject to their Codex availability and quota.
- This is a conscious risk acceptance, not a reversal or falsification of T030. A future official
  empty-tools contract can remove the experimental warning only through another reviewed decision.
- Hosted web search is no longer part of the accepted risk: the production argv removes it through
  the supported top-level mode and the live inventory gate must fail if a network-reaching tool
  reappears.
- `apply_patch` remains visible, but a write attempt is denied by `--sandbox read-only`. A future
  CLI version that permits the write, exposes a shell/network tool, or stops reporting tool events
  fails the connector's acceptance and must make it unavailable again.
- Unknown or known tool events fail the call. That observation is not proof that no tool could have
  run before the event was received, which is why consent and the warning remain mandatory.
- Codex and direct OpenAI retain different backend fingerprints and cached artifacts.

## References

- [OpenAI Codex developer commands](https://learn.chatgpt.com/docs/developer-commands)
- ADR-0008
- T025
- T030
- T047
