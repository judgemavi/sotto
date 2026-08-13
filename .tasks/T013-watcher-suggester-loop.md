# T013 — Realtime meeting proposal loop

**Status:** blocked

**Wave:** P1 — optional realtime copilot

**Depends on:** T035/T016 map acceptance; T029 live OpenAI evidence; T039 read-only MCP
context; T041 durable grounded evidence; T042 generic proposal events. T045 is optional: T013 may
ship with meeting plus MCP grounding but must not use historical sales-only RAG kinds. T045 local
retrieval is not authorized for proposal output until a later core schema task adds a citation type
distinct from T041/MCP `ExternalEvidenceRef`; local chunk ids must never be placed in that field.

**Owns:** `crates/advisor/**`, `prompts/advisor/**`,
`.tasks/T013-watcher-suggester-loop.md`

## Goal

Build a quiet, optional watcher/proposer loop for general meetings. Proposals are off by default,
and `no proposal` is the common result. Every displayed proposal is anchored to the meeting moment
that justified it and cites every external assertion.

This contract supersedes T013's 2026-08-11 sales-only watcher/suggester plan. ADR-0011 and T042
define the current product and event vocabulary.

## Plan

1. Watch timestamped partial/final transcript from either stream. Do not assume rep/customer roles
   and do not hard-gate one speaker. Schedule display for a natural pause when possible.
2. Use a cheap, tightly bounded first pass to decide whether an assist is warranted and select one
   generic proposal kind: clarifying question, decision check, next step, follow-up, or relevant
   context. Missing required backend capabilities fails before transport.
3. Start bounded MCP retrieval only after the watcher finds potential value. Use only the immutable
   T039/T041 session grant and context bundle; never pass MCP tools or credentials to the model.
   Local T045 retrieval waits for a distinct core local-evidence citation schema and is not part of
   this task.
4. Assemble a transcript-first proposer request. Optional screen evidence follows T028/T031's
   one-inspection, capability, provenance, and image-consent contract.
5. Stream T042 proposal partial/final events with same-session anchors, meeting citations, external
   evidence ids, backend fingerprint, usage, and cancellation state. Supersession is append-only.
6. Cancel and re-fire when a final transcript materially changes a speculative proposal. Track
   cancelled usage and never let a stale completion attach to a newer event.
7. Reject unknown/uncited evidence, unavailable sources, malicious resource instructions, invalid
   schema, and rate limits honestly. No path executes an MCP action or implies user acceptance.
8. Replay accepted meetings and annotated fixtures. Measure false-positive rate, quiet rate,
   missed-useful rate, cancellation, and pause-to-first-token latency; weight annoying false
   positives heavily.

## Contract for downstream tasks

The advisor consumes the append-only meeting timeline plus an immutable context-bundle snapshot
and emits typed proposal output events. It never mutates captured meeting facts, app state, MCP
servers, or local knowledge.

## Acceptance

- A low-value meeting fixture produces no proposal and no MCP request.
- Useful proposal fixtures produce the expected generic kind, a valid same-session anchor, and
  known meeting/external citations.
- Unavailable or injected external sources fail closed without disclosure or action.
- Speculative cancellation stops the pinned provider and prevents stale output.
- Replay evidence records precision, quietness, missed-useful rate, and stage latency; live
  pause-to-first-token targets approximately one second without weakening correctness.
- No-provider, proposals-off, and sources-off are normal zero-network states.

## Out of scope

Sales-only triggers, battlecards, model-visible MCP tools, external action execution, board
rendering (T043), overlay productionization, and AI product acceptance (T044).
