# T046 — Bounded MCP stdio transport spike

**Status:** done

**Wave:** C0b — optional local MCP transport

**Depends on:** T039 acceptance; an implementation approach that can bound frames before
allocation rather than wrapping an already-unbounded SDK reader

**Owns:** `crates/mcp/src/stdio/**`, focused stdio tests, conditional manifest/`Cargo.lock`
handoff, `docs/experiments/mcp-stdio-bounds.md`, `.tasks/T046-bounded-mcp-stdio-spike.md`

## Goal

Determine whether Sotto can safely support a user-approved local MCP stdio server without an
unbounded protocol allocation or orphan process risk.

## Plan

1. Implement or select a transport that enforces a byte cap before allocating a complete frame.
2. Launch an exact executable+argv without a shell, with a minimal allowlisted environment and
   isolated working directory.
3. Make cancellation, timeout, receiver drop, and malformed/oversized frames terminate and reap
   the entire process tree.
4. Keep stderr bounded/redacted and ensure argv, environment, paths, and server content do not
   enter Debug/errors/logs.
5. Record PASS/FAIL. FAIL leaves stdio unavailable; it does not weaken T039's HTTP contract.

## Contract for downstream tasks

Only a PASS followed by independent review may create a separate product enablement task. T040
does not expose local-process configuration while this task is blocked or failed.

## Acceptance

- A newline-free oversized frame is rejected after bounded allocation.
- Fake children prove exact argv/minimal env and process-tree cleanup on every terminal path.
- Interactive requests and tool calls remain impossible.
- A real user-installed resource-only stdio server lists and reads one known resource within the
  same bounds, or the task records FAIL without product enablement.

## Out of scope

OAuth, remote HTTP, app settings, reasoning integration, and MCP actions.

## Notes — FAIL verdict (2026-08-12)

- Implemented an isolated Sotto-owned `rmcp::Transport<RoleClient>` spike under
  `crates/mcp/src/stdio/`; it is not exported from `mcp::lib` and does not enable stdio.
- Bounded newline framing, exact executable+argv, absolute working directory, cleared/minimal
  environment, zero-buffer stderr, redacted diagnostics, official-rmcp list/read, no tool calls,
  fail-closed input-required observation, and ordinary cancellation/timeout/drop/error cleanup all
  passed focused fixture tests.
- The direct child and ordinary descendants are killed through a dedicated process group and the
  direct child is awaited by a supervisor.
- Verdict is **FAIL** because a descendant that creates a new session with `setsid` escapes the
  original process group. The focused regression proves it remains alive, then explicitly cleans
  the fixture pid. Process groups cannot establish the task's entire-tree guarantee.
- No separately consented real user-installed MCP server command was available or launched. The
  deterministic Python fixture is not represented as that manual acceptance gate.
- Full evidence and the product consequence are recorded in
  `docs/experiments/mcp-stdio-bounds.md`. T040 must keep local stdio unavailable.

Verification on 2026-08-12:

- `cargo test -p mcp --test stdio_spike` — PASS, 7 tests. This includes the adversarial escaping-
  descendant regression that establishes the FAIL verdict and cleans up its fixture explicitly.
- `cargo test -p mcp --lib` — PASS, 11 tests.
- `cargo test -p mcp --test rmcp_http` — PASS, 3 tests.
- `cargo clippy -p mcp --all-targets --all-features -- -D warnings` — PASS.
- `cargo fmt -p mcp -- --check` — PASS.
- `git diff --check -- crates/mcp/src/stdio crates/mcp/tests/stdio_spike.rs
  crates/mcp/tests/fixtures/stdio_child.py crates/mcp/Cargo.toml Cargo.lock
  docs/experiments/mcp-stdio-bounds.md .tasks/T046-bounded-mcp-stdio-spike.md` — PASS.

Not run: a real user-installed MCP stdio server. No product enablement is requested or implied.

Independent review on 2026-08-12 accepted the spike's **FAIL** verdict and closed T046 without
enabling stdio. The Sotto-owned test adapter caps inbound bytes before frame append/JSON parsing,
caps outbound JSON before newline framing, launches an absolute executable with an exact argv and
absolute isolated working directory, clears the inherited environment to the documented minimum,
discards stderr, and redacts command/protocol diagnostics. Focused fixtures prove official-rmcp
resource list/read without tools, one observable `InputRequired` without follow-up, and direct-child
plus ordinary-descendant cleanup for close, cancellation, timeout, receiver drop, malformed input,
and oversized input. The adversarial `setsid` regression truthfully proves a descendant escapes the
process group, then removes the exact fixture process; this defeats the required entire-tree
guarantee and is sufficient for FAIL. No real user-installed server run was claimed. Production
remains unchanged: `lib.rs`, `ServerConnection`, `TransportKind`, and normal manifest features are
HTTP-only; the stdio module and Tokio process support are test/dev-only, and T039's 11 unit plus 3
HTTP integration tests still pass. Reviewer reruns also passed all 7 stdio-spike tests, MCP clippy
with warnings denied, scoped rustfmt, dependency-feature inspection, and scoped diff check.
