# T039 — MCP read-only context plane

**Status:** done

**Wave:** C0 — external evidence foundation

**Depends on:** T036

**Owns:** `crates/mcp/**`, `.tasks/T039-mcp-read-only-context-plane.md`, and `Cargo.lock` only
during the sequential manifest handoff

## Goal

Implement a small Sotto-owned MCP broker that retrieves explicitly selected text resources as
bounded, provenance-bearing evidence without exposing MCP tools or SDK types to reasoning code.

## Plan

1. Pin the official Rust MCP SDK with client-only features behind Sotto-owned traits and values.
2. Support capability discovery plus list/read for explicitly selected text resources over
   bounded Streamable HTTP connections. Keep stdio disabled until T046 supplies an allocation-
   bounded transport and subprocess-lifecycle proof.
3. Normalize evidence ids, server/resource descriptors, source receipts, excerpts, digests,
   retrieval time, and truncation state.
4. Enforce cancellation, per-call timeout, resource/page/byte/token budgets, deterministic
   truncation, HTTPS outside loopback, and bounded transport responses before protocol parsing.
5. Treat resource content as untrusted data. Reject blobs, prompts, sampling, arbitrary URI
   fetches, subscriptions, roots, model-authored calls, and all tool execution.
6. Provide fake transports for credential-free tests; hide all `rmcp` types from consumers.

## Contract for downstream tasks

`ContextSource` resolves a `SessionContextGrant` into a deterministic, serializable
`ContextBundle` containing known evidence ids and receipts. T039 owns the portable value and
digest contract; T041 owns durable storage/replay. The default grant selects no server.
Meeting-derived query disclosure is separately represented and defaults to none.

## Acceptance

- No server is contacted without an explicit grant.
- No transcript text is sent to a server under a resource-only grant.
- Unknown, binary, oversized, timed-out, cancelled, and changing resources fail or truncate
  explicitly without leaking content into logs.
- Streamable HTTP bounds response bytes before SSE parsing and rejects unbounded JSON responses.
- Remote endpoints require HTTPS except loopback tests; credentials are never in descriptors,
  Debug, errors, receipts, or logs.
- `core`, `providers`, and `insight` import no MCP SDK types.

## Out of scope

OAuth/settings UI, durable source ingestion, provider-native remote MCP tools, and any action.

## Notes

Implemented the application-controlled context plane in `crates/mcp`:

- pinned official `rmcp = =3.0.0` with client-only Streamable HTTP production features; SDK
  values remain private to `connection.rs` and `http_client.rs`;
- added lazy `RmcpResourceTransport` connections and a Sotto-owned `ResourceTransport` seam
  with only capability discovery, `resources/list`, and `resources/read` operations;
- added exact `ServerId` / `ResourceUri` grants, default-no-server and default-no-query policy,
  descriptors, evidence ids, receipts, SHA-256 content/bundle identities, retrieval time, and
  explicit truncation provenance;
- enforced selected-server/resource, catalog page/resource, per-resource, bundle, estimated-token,
  decoded transport-message, timeout, cancellation, MIME, binary, and identity-change policy;
- added a Sotto-owned bounded HTTP backend under rmcp's lifecycle/client layer: it caps complete
  SSE responses before parsing and rejects `application/json` response bodies without reading
  them because rmcp's built-in reqwest JSON path has no allocation limit;
- validated HTTPS except loopback and rejected endpoint credentials/query/fragment. Resource URIs,
  including custom schemes, also reject user information, query, and fragment components before
  they can enter receipts or serialization;
- added custom redacted `Debug` implementations for public server, resource, catalog, receipt,
  excerpt, selection, grant, raw-content, connection, and endpoint types. Error-facing server-id
  display is fingerprinted; token-canary coverage guards the public surfaces;
- rejected MRTR input-required resource reads and advertised no roots, sampling, elicitation, or
  extensions. The public trait has no prompt, tool, action, arbitrary URI, or subscription method;
- added credential-free fake-transport unit tests plus real official-SDK loopback HTTP tests that
  assert list/read with zero tool calls, reject MRTR `InputRequired` without a follow-up, and fail
  an oversized initialize response before catalog decoding.

Stdio is deliberately not a supported T039 runtime. rmcp 3.0's `TokioChildProcess` delegates to
`AsyncRwTransport`, whose receive path uses unbounded newline `read_until` and exposes no maximum-
frame constructor. Claiming the transport budget for that path would be false. T046 owns a bounded
stdio adapter plus argv/environment/cancellation/descendant-process evidence before it can be
enabled.

Verification on 2026-08-12:

- `cargo test -p mcp --lib` — PASS, 11 tests.
- `cargo test -p mcp --test rmcp_http` — PASS, 3 tests with loopback permission. A repeated
  sandboxed run was denied at listener bind with `PermissionDenied`; the unchanged tests passed
  immediately with loopback bind permitted.
- `cargo clippy -p mcp --all-targets --all-features -- -D warnings` — PASS.
- `cargo fmt -p mcp -- --check` — PASS.
- `git diff --check -- crates/mcp .tasks/T039-mcp-read-only-context-plane.md Cargo.lock` — PASS.

Not run: a remote authenticated server, OAuth, settings UI, full-workspace tests, or stdio. Stdio
is disabled and owned by T046; OAuth and user approval remain owned by T040.

Independent final review on 2026-08-12 accepted T039's deterministic scope. Production exposes
only the Streamable HTTP connection variant and enables no rmcp child-process/stdio feature;
T046 remains the explicit gate for any future bounded stdio runtime. The custom HTTP client checks
the cumulative response-byte budget before each chunk reaches the SSE parser and rejects non-SSE,
including JSON, before reading the body. Endpoint and resource identifiers reject user information,
query, and fragment containers; public diagnostic surfaces redact server-controlled identifiers,
URIs, metadata, and content. The official-SDK loopback coverage proves resources list/read with no
tool call, fail-closed `InputRequired`, and an oversized initialize response rejected before catalog
decoding. SDK types remain isolated to `mcp`, and the lockfile resolves rmcp only through that crate.
Reviewer reruns passed 11 unit tests, 3 HTTP integration tests, all-target/all-feature clippy with
warnings denied, scoped rustfmt check, dependency/feature inspection, and scoped diff check. No
remote authenticated server, OAuth/settings flow, full-workspace gate, or stdio run was claimed.
