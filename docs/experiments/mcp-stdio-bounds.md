# MCP stdio bounded transport verdict

- Date: 2026-08-12
- Verdict: **FAIL**
- Tested SDK: `rmcp 3.0.0`
- Product consequence: local MCP stdio remains unavailable

## Question

Can Sotto support a user-approved local MCP server with all of these properties at once?

1. A newline-delimited JSON frame is capped before a complete frame is allocated or parsed.
2. The server launches as an exact executable and argument vector without a shell string, with an
   isolated working directory and minimal environment.
3. Cancellation, timeout, receiver drop, malformed input, and oversized input terminate and reap
   the direct child and every descendant.
4. Diagnostics do not expose executable paths, arguments, working directories, environment,
   protocol content, or stderr.

T046 requires every property. A partial implementation is not enough to enable the transport.

## Official SDK finding

`rmcp 3.0.0` cannot directly satisfy the inbound-frame condition. Its `TokioChildProcess` uses
`AsyncRwTransport`, whose receive path reads a newline-delimited frame with unbounded
`BufReader::read_until`. No child-process constructor accepts `JsonRpcMessageCodec`'s available
maximum-length setting.

The spike therefore implemented a Sotto-owned `Transport<RoleClient>` adapter under
`crates/mcp/src/stdio/` without exposing it from `mcp::lib`. It retains rmcp's lifecycle and typed
protocol handling while replacing only the subprocess and byte-framing boundary.

## What passed

The isolated adapter demonstrated:

- A configurable frame cap between 1 byte and 16 MiB. The reader preallocates exactly the selected
  cap, reads incrementally, and rejects the first non-newline byte beyond the cap before appending
  it or invoking JSON parsing.
- Outbound messages are checked against the same cap and the newline is written separately, so
  adding framing cannot grow the bounded message buffer.
- An absolute executable, exact argument vector, and absolute working directory are passed to
  `tokio::process::Command` directly. There is no shell string.
- `env_clear` followed by only `LANG=C`, `LC_ALL=C`, and an isolated `TMPDIR`. The macOS Python
  fixture additionally observes `__CF_USER_TEXT_ENCODING`, which the runtime injects rather than
  Sotto inheriting from its parent.
- Stderr is sent to the null device, giving it a zero-byte application buffer and preventing
  server-controlled stderr from reaching diagnostics.
- The direct child starts as leader of a new process group. On every ordinary terminal path a
  supervisor sends `SIGKILL` to the group, requests direct-child termination, and awaits the direct
  child before publishing reaped state.
- Normal initialize, `resources/list`, and `resources/read` work through the official rmcp client.
  The fixture records one list, one read, and no tool method.
- An MRTR `InputRequired` response is surfaced once with no automatic follow-up and no tool call.
  Product code would still need to map this through T039's fail-closed resource policy before any
  later enablement.
- Token-canary diagnostics omit command paths, arguments, working directories, server content,
  and stderr.

Focused fixture tests passed for normal list/read, input-required, a newline-free oversized frame,
malformed JSON, explicit cancellation, timeout, receiver drop, and ordinary descendant cleanup.

## Why the verdict is FAIL

A Unix process group is not a process-tree containment boundary. The fixture starts a descendant
with a new session (`setsid`). Killing the original child's process group reaps the direct child
and ordinary descendants, but the new-session descendant remains alive. The regression records
that escape, then explicitly kills and observes the fixture pid so the test itself leaks no process.

This violates T046's requirement to terminate and reap the entire process tree on every terminal
path. A cooperative server usually inherits the group, but “usually” is not the safety contract,
and an approved MCP package may launch helpers that create their own sessions without malicious
intent.

The acceptance contract also asks for a real user-installed resource-only MCP server. None was
launched: no exact local-server command was separately approved for this spike, and the Python
fixture is deterministic test evidence, not a user-installed-server acceptance run. That missing
manual proof independently prevents PASS.

## Consequence

- `ServerConnection` remains Streamable HTTP only.
- T040 must continue to render local stdio as unavailable and must not accept executable or argv
  configuration.
- The spike module and fixtures are evidence only; they are not exported or reachable from the
  production MCP API.
- A future reconsideration needs an OS-backed containment design that can prove cleanup even when
  descendants change process groups/sessions, followed by a separately consented real-server run
  and independent review. Reusing this process-group adapter alone cannot change the verdict.
