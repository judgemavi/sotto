# T036 — Meeting-copilot product contract and roadmap

**Status:** done

**Wave:** N0 — product reframe

**Depends on:** T030's Codex isolation verdict; accepted T024–T034 reasoning seams

**Owns:** `AGENTS.md`, `docs/adr/0011-meeting-notes-and-mcp-proposals.md`,
`.tasks/README.md`, `.tasks/T036-meeting-copilot-product-contract.md`, amendments to open
T013 and T019 only

## Goal

Replace the sales-only destination with a local-first AI meeting copilot whose minimum AI value
is cited meeting notes and whose optional proposals can use explicitly authorized MCP evidence.

## Plan

1. Preserve the explicit, scoped, no-key meeting record and append-only timeline.
2. Define structured notes as the minimum reasoning capability.
3. Define quiet, anchored proposals as an optional later capability.
4. Promote MCP ahead of proposals with a read-only, application-controlled v1 boundary.
5. Correct stale Codex-first wording and align the task waves without rewriting completed tasks.

## Contract for downstream tasks

ADR-0011 is authoritative for notes, MCP disclosure/provenance, proposal behavior, UI priority,
and the direct-OpenAI-only v1 backend state.

## Acceptance

- `AGENTS.md` contains no sales-only product or Codex-starter contract.
- Notes, MCP context, proposals, UI, and evaluation have exact task owners and dependencies.
- T035 and T029 evidence gates remain intact.
- Completed task history is preserved.

## Out of scope

Production implementation and live product acceptance.

## Notes

Accepted on 2026-08-12 after the maintainer explicitly shifted Sotto to an AI-enabled meeting
copilot. Product and architecture audits confirmed that local capture/map and headless recap
foundations exist, while notes UI, MCP, and realtime proposals do not.

## Independent review — accepted (2026-08-12)

The notes-first meeting-copilot contract is internally consistent and has exact downstream
ownership. It preserves the no-key factual record, makes direct OpenAI the only selectable v1
reasoning path, promotes bounded read-only MCP evidence ahead of optional proposals, and keeps
proposal audit events distinct from captured meeting facts.

The final review also confirmed durable MCP receipt replay has an owner, active app/CLI/core
handoffs are explicit, the frozen-core exception is recorded, the sales-shaped clustering and
local-knowledge migrations are assigned, task statuses match the board vocabulary, and T035/T029
remain separate manual/live gates. No dependency cycle remains across T037–T045 and T013/T019.
