# ADR-0011: Meeting notes first; MCP-grounded proposals second

- Status: Accepted
- Date: 2026-08-12
- Decision owners: Sotto maintainers

## Context

Sotto's capture, transcription, timeline, board, and provider foundations are useful beyond
sales calls. The prior product contract made sales-specific realtime advice the destination and
treated post-call summarization and MCP as later supporting features. That ordering leaves the
most dependable AI value headless while prioritizing the highest-latency, highest-noise feature.

The revised product goal is an AI-enabled meeting copilot. At minimum it produces trustworthy
meeting notes. It may additionally propose useful things based on the meeting and resources the
user made available through MCP.

MCP introduces a distinct trust boundary. External content can be sensitive, malicious, stale,
or capable of inducing actions. A server's claim that a tool is read-only is metadata, not an
enforceable permission boundary. The product needs evidence provenance and disclosure controls
before it needs model-controlled tools.

## Decision

1. The deterministic, no-key meeting record remains the foundation: explicitly scoped capture,
   local transcription, prosody, retained screen-change references, and an append-only timeline.
2. Structured meeting notes are the minimum AI capability. Notes contain an overview, topics,
   decisions, action items, open questions, risks, and follow-ups. Owners and dates appear only
   when stated. Every factual item cites known timeline event ids.
3. Captured meeting events and model output are distinct. Notes and topical views are recomputable
   derived artifacts stored alongside the timeline. A proposal may be appended as a typed system-
   output event so the audit timeline records what Sotto displayed, but it is never a factual
   meeting event and never mutates or supersedes captured evidence. A missing reasoning backend
   leaves a complete transcript and board, with a truthful explanation that AI notes are unavailable.
4. Realtime proposals are optional, off by default, and quiet by default. Initial kinds are
   clarifying question, decision check, next step, follow-up, and relevant context. A proposal
   is anchored to meeting events, distinguishes meeting citations from external evidence, and
   never implies that the user accepted it.
5. MCP v1 is a Sotto-controlled, resources-first context plane. A session grant selects servers
   and resources. Sotto retrieves bounded text evidence, assigns opaque evidence ids, records
   source receipts and digests, and passes the result to reasoning as untrusted context.
6. The model does not receive MCP credentials, arbitrary MCP tools, or authority to execute an
   external action. Side-effecting MCP calls are out of scope. Sending meeting-derived search
   text to an MCP server requires a separate explicit disclosure grant.
7. The main review workspace has Notes and Board lenses over the same selected session. Notes
   citations navigate to the underlying board moment; external citations expose their source
   identity and retrieval provenance.
8. Direct OpenAI Responses API remains the supported BYOK reasoning backend and continues to send
   no tools. ADR-0014 additionally permits an explicit, off-by-default experimental Codex
   subscription path using the user's ChatGPT login. T030's lack of a supported empty model-visible
   tool inventory still stands and must remain visible wherever Codex can be selected. Backend ids
   remain open for future adapters.
9. Transcript-first and on-demand screen inspection remain unchanged. No audio is sent to a
   model, and image transport still requires an explicit user opt-in and a capable backend.

This ADR supersedes sales-only product framing in `AGENTS.md`, T013's original trigger taxonomy,
and the prior decision to defer MCP until after realtime advice. It does not rewrite the accepted
implementation evidence in completed tasks.

## Consequences

- The shortest product path is headless cited notes, then a notes review UI, then optional MCP
  enrichment, and only then realtime proposals.
- Sales-shaped recap, clustering, RAG labels, speaker labels, and suggestion event names require
  deliberate one-way migrations or replacement product surfaces. T045 owns the RAG taxonomy.
- MCP context must be included in derived-artifact cache identity. An unchanged transcript with
  changed source evidence is a different reasoning input.
- Source unavailability degrades to transcript-only notes; unknown meeting or external citations
  fail closed.
- Product acceptance must measure note faithfulness and citation precision independently from
  proposal usefulness, quietness, and latency.
- OAuth, local-process launch, remote disclosure, prompt injection, retention, and deletion add
  security and UX work. V1 minimizes that surface by prohibiting actions and durable source
  ingestion.

## Revisit if

- an on-device reasoning backend meets the note-quality and footprint gates;
- Codex exposes a supported, independently verifiable empty model-visible tool inventory;
- users demonstrate a need for explicitly approved external actions strong enough to justify a
  separate threat model and ADR; or
- participant-verified evaluation shows the Notes-first ordering does not provide meaningful
  value.
