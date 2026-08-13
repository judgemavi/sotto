# T041 — MCP-grounded meeting notes

**Status:** done

**Wave:** C2 — source-grounded synthesis

**Depends on:** T037; T038; T039; T040

**Owns:** `crates/insight/src/notes/**`, `crates/insight/tests/mcp_grounded_notes.rs`,
`crates/insight/src/lib.rs`, `crates/insight/Cargo.toml`, `prompts/notes/**`,
`crates/rag/src/store.rs`, `crates/rag/src/schema.rs`, focused RAG tests, the narrow durable-bundle
integrity seam in `crates/mcp/src/types.rs` after T040 releases it,
`crates/app/src/mcp/mod.rs`, `crates/app/src/notes/controller.rs`,
`crates/app/src/notes/view.rs` after T040's recorded handoff, `Cargo.lock`,
`.tasks/T041-mcp-grounded-meeting-notes.md`

## Goal

Enrich meeting notes with approved MCP resources while keeping meeting facts and external
evidence distinct, cited, bounded, and optional.

## Plan

1. Accept a session grant snapshot and `ContextSource` at the notes boundary.
2. Wrap MCP excerpts as untrusted evidence with opaque ids; never expose MCP functions.
3. Validate meeting `EventId` citations and external evidence ids separately. Every note claim and
   optional owner/due claim declares an evidence basis of meeting, external, or mixed; the declared
   basis requires the corresponding nonempty citation vectors and forbids vacuous provenance.
4. Include the context-bundle digest in cache identity.
5. Persist the exact normalized bundle and source receipts beside each grounded artifact so a
   reopened note/proposal citation resolves without contacting the server again. Define explicit
   retention and deletion with the meeting; this is evidence replay, not corpus ingestion.
6. Degrade to transcript-only notes with explicit source status when MCP is absent or unavailable.

## Contract for downstream tasks

A notes item may cite meeting evidence, external evidence, or both. External claims require known
evidence ids whose receipts remain inspectable by the UI.

## Acceptance

- Unknown evidence ids, uncited external claims, and excerpt mismatches fail closed.
- Persisted bundles are integrity-checked on replay: digest, evidence/server/URI/content hashes,
  included bytes, truncation, token counts, and uniqueness must agree before any citation renders.
- Changed source content misses cache even when the timeline and backend are unchanged.
- Reopening a grounded artifact resolves every external citation from its durable receipt without
  re-contacting the MCP server; deleting the meeting deletes its stored bundle and receipts.
- Artifact and bundle/receipt rows commit atomically; no cited artifact can survive with an orphaned
  or mismatched evidence bundle.
- Source failure still returns valid transcript-only notes plus an unavailable-source status.
- Prompt-injection fixtures cannot change the output schema, disclose other context, or cause a
  tool/action call.
- Provider request shape continues to contain no tools.

## Out of scope

Realtime proposals, durable MCP corpus ingestion, and external actions.

## Implementation handoff — 2026-08-12

- Added portable `ContextBundle` replay validation with separate stable content/cache and exact
  serialized-receipt integrity digests. Validation fails closed on digest, evidence/server/URI,
  content hash, byte/truncation/token, uniqueness, ordering, and identifier mismatches; retrieval
  wall clock does not perturb stable content identity.
- Migrated RAG to schema v4 with one atomic grounded-artifact row containing v2 notes, usage,
  truthful provider model, frozen optional grant fingerprint, source status, and the exact bundle.
  V3 migration, conflict immutability, replay, and transactional meeting/event/artifact deletion
  are covered; v1 derived artifacts and prompts remain intact.
- Added v2 grounded note schemas with explicit meeting/external/mixed basis and separate evidence
  vectors for every factual, owner, and due-date claim. Unknown, vacuous, and mismatched evidence
  fails closed, including nested owner/due provenance.
- Grounded generation resolves only the frozen application-selected grant through `ContextSource`,
  includes bundle digest/status/fingerprint in cache identity, preserves cancellation before cache
  return and persistence, degrades operational source failure to transcript-only notes, and keeps
  MCP cancellation distinct. No model-visible MCP tool/action surface exists.
- The app freezes SessionId, grant, fingerprint, credentials, and HTTP-only broker at click time.
  Notes reopens validated durable v2 artifacts without an MCP/provider call, rejects stale timeline
  artifacts, preserves cached Ready state when reasoning is disabled, and renders meeting evidence
  separately from inspectable external receipts (source, URI, title, retrieval time, digest,
  bytes/truncation, and last-modified metadata).
- Added a captured-request injection regression: a malicious selected excerpt remains inside the
  explicitly labelled untrusted-evidence section, an unselected resource canary never enters the
  request, the request schema contains no tool/tool-choice field, and an attempted extra output
  field fails closed under the v2 deny-unknown schema.
- Added a direct cold-reopen controller regression: a durable grounded artifact rehydrates to Ready,
  disabling reasoning preserves that reviewable artifact, and the provider call flag remains clear;
  replay has no `ContextSource` and therefore cannot contact MCP.
- PASS — MCP library tests: 14 passed.
- PASS — RAG tests: 10 passed, 1 latency/model-cache test ignored; focused persistence: 7 passed.
- PASS — full insight tests: 28 passed, including 6 focused grounded-note regressions.
- PASS — full app library tests with runtime shaders: 73 passed, including the direct zero-provider
  cold-reopen/disable regression.
- PASS — MCP/RAG/insight all-target/all-feature and app all-target runtime-shader Clippy with
  warnings denied. App Clippy used `SOTTO_SKIP_SWIFT_BUILD=1`; the app test binary was linked and
  executed from the same target.
- PASS — `cargo fmt --all -- --check` and `git diff --check`.
- NOT RUN — real MCP server, real OpenAI, manual UI, stdio, or any MCP tool/action. All new evidence
  and provider tests use fakes; T046's stdio FAIL remains enforced.

## Independent review — 2026-08-12

- ACCEPTED — the final malicious-evidence regression captures the exact reasoning request, proves
  only the selected excerpt is rendered as labelled untrusted data, rejects an attempted output
  `tools` field through the closed notes schema, and confirms the provider-neutral request has no
  tool or tool-choice surface.
- ACCEPTED — the final cold-reopen controller regression rehydrates the durable grounded artifact
  to `Ready`, preserves it when reasoning is disabled, and observes no additional provider work.
- PASS — focused injection regression and focused app cold-reopen regression rerun independently.
  The broader implementation gates recorded above, Rustfmt, Clippy, and diff checks were also
  reviewed; no T041 acceptance blocker remains. Real MCP/OpenAI/manual UI evidence remains
  explicitly not run and is not required for this deterministic slice.
