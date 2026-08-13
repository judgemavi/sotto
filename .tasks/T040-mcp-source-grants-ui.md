# T040 — MCP source connections and per-session grants

**Status:** done

**Wave:** C1 — user-controlled source access

**Depends on:** T038; T039

**Owns:** `crates/app/src/mcp/**`, sequential edits to `crates/app/src/settings/**`,
`crates/app/src/notes/**`, `crates/app/src/lib.rs`, `crates/app/src/main.rs`,
`crates/app/Cargo.toml`, the narrow authenticated-HTTP and per-server grant/fingerprint seams in
`crates/mcp/src/connection.rs`, `crates/mcp/src/types.rs`, `crates/mcp/src/broker.rs`, their focused
tests, `Cargo.lock`, `.tasks/T040-mcp-source-grants-ui.md`

## Goal

Let users configure trusted MCP connections and explicitly choose which sources may contribute
evidence to one meeting.

## Plan

1. Add remote HTTPS connection flows with visible trust boundaries. Local stdio appears only as
   unavailable with an explanation until T046 passes and a follow-up task explicitly enables it.
2. Store secrets in Keychain and persist only identifiers, endpoints, and grants. Pass an opaque
   bearer secret directly to the MCP HTTP adapter; never serialize it or include it in diagnostics.
3. Show remote host, connection health, resource discovery, and
   selected resources.
4. Separate local-process launch, remote network access, and meeting-query disclosure grants.
   Query disclosure is granted to one named `ServerId`, never as an unbound global flag.
5. Show the immutable grant fingerprint and a truthful `not retrieved` source-evidence state.
   Per-receipt metadata and unavailable/truncated retrieval results appear only after T041 performs
   a grounded notes run; T040 must not contact resources merely to populate UI.

## Contract for downstream tasks

The selected meeting owns an immutable `SessionContextGrant` snapshot for each reasoning run.
Its fingerprint commits to the server id, endpoint identity, resource selection, and named-server
query-disclosure choice independently of returned resource bytes. Changing any of those inputs
creates a new run identity and cannot alter an in-flight result's inputs.

## Acceptance

- Sources default off and require an explicit per-session selection.
- Credentials never enter app persistence; no local MCP process can be launched in this task.
- Remote product connections require HTTPS; loopback HTTP remains test-only and is never presented
  as local-process/stdin support.
- Removing or changing a source invalidates/replaces only future runs.
- Remote, unavailable-local-transport, and query-disclosure states are distinct in text, not
  color alone.
- No UI action can invoke an MCP tool or mutate an external service.

## Out of scope

Notes prompt integration, proposals, and autonomous or user-approved external actions.

## Implementation handoff — 2026-08-12

- Added remote HTTPS-only source configuration, explicit user-triggered health/resource discovery,
  endpoint-bound bearer credentials in Keychain, and persisted identifier/endpoint/grant metadata
  with no secret field. The bounded HTTP client rejects redirects, including a regression proving a
  redirect target receives neither a request nor a bearer token.
- Added per-meeting resource selection and named-server redacted-query disclosure, both default
  off. Mutations require a configured server and advertised resource. Endpoint replacement deletes
  the old endpoint credential and clears that server's prior resource/disclosure approvals rather
  than retargeting them; removal deletes Keychain state before changing memory/persistence.
- Added a credential-free immutable fingerprint covering exact server id, endpoint, selected
  resources, and named disclosure. Fingerprint refresh does not read any Keychain entry. Receipt
  state remains truthfully `NotRetrieved`; source content retrieval and grounded notes stay out of
  scope.
- Mounted the shared controller in Settings and the Notes workspace. Settings distinguishes remote
  HTTPS, explicit network check, write-only token storage, and unavailable stdio in text. Notes
  shows per-meeting selections, query disclosure, future-run semantics, and no tools/actions.
- PASS — MCP library tests: 13 passed.
- PASS — MCP official-SDK HTTP integration: 4 passed, including redirect isolation, bounded
  protocol input, resources-only list/read, no tool calls, and input-required rejection.
- PASS — app library tests: 67 passed, including MCP default-off/endpoint-replacement and redacted
  public-state regressions.
- PASS — app runtime-shader `cargo check`.
- PASS — MCP all-target/all-feature and app all-target runtime-shader clippy with warnings denied.
- PASS — `cargo fmt --all -- --check` and `git diff --check`.

### Independent acceptance — 2026-08-12

- Accepted after read-only review of the final run-identity UI, bearer-free settings round-trip,
  exact endpoint credential cleanup, per-session grant invalidation, and persistence rollback paths.
- T041 may now consume the immutable grant snapshot and fingerprint; authenticated real-remote MCP,
  grounded retrieval, receipt replay, and manual UI acceptance remain explicitly unclaimed.
- NOT RUN — authenticated real remote MCP server, manual UI, or any grounded notes run. Stdio stays
  unavailable after T046 FAIL; T041 prompt integration was not started.

### Final diagnostics hardening — 2026-08-12

- Replaced raw derived diagnostics on credential readiness, connection health, and MCP UI errors
  with stable redacted categories. User-facing error text names only the corrective category and
  never renders a server-, protocol-, filesystem-, or Keychain-controlled cause.
- PASS — direct token-canary formatting regressions for `CredentialReadiness`,
  `ConnectionHealth`, `ConfiguredServer`, and every raw-cause `McpUiError` Debug + Display path.
- PASS — focused app MCP tests: 2 passed.
- PASS — app all-target runtime-shader clippy with warnings denied.
- PASS — final rustfmt and diff checks.

### Final grant identity and transactional persistence hardening — 2026-08-12

- Notes now renders only a stable `prepared` / `unavailable` source-grant run-identity state; it
  does not reveal the endpoint, selected resource URI, or fingerprint digest. The separate receipt
  remains truthfully `NotRetrieved` until grounded retrieval exists.
- Persisted-settings coverage round-trips server ids, exact HTTPS endpoints, session resource
  selections, and named query disclosure while a bearer canary remains absent from JSON.
- Successful source removal deletes the exact endpoint-scoped Keychain entry and clears that
  source's resources, disclosure, and future fingerprint. Endpoint replacement deletes the old
  endpoint token and clears the old approval/fingerprint rather than retargeting it.
- Added an injectable settings persistence seam. Endpoint replacement and source removal stage
  controller mutations, then restore both memory and the exact old credential if persistence
  fails; focused failure-injection regressions cover both operations.
- PASS — focused app MCP controller tests: 6 passed; focused disclosure-free Notes identity test:
  1 passed.
- PASS — full app library tests with GPUI runtime shaders: 72 passed.
- PASS — app all-target runtime-shader Clippy with warnings denied, independently rerun outside the
  nested Swift sandbox so the capture bridge build also completed.
- PASS — MCP library tests: 13 passed. PASS — MCP localhost HTTP integration tests: 4 passed
  outside the filesystem sandbox, including redirect bearer isolation.
- PASS — `cargo fmt --all -- --check` and `git diff --check`.
