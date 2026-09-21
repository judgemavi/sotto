# T104 — Sotto consumes no MCP

**Status:** ready

**Wave:** N8 — entry workspace

**Depends on:** T102, which had to land first because `crates/app/src/workspace/notes.rs` holds 32
of the references removed here and was mid-edit. T102 closed 2026-08-27.

**Owns:** the removal of every MCP *client* surface — `crates/mcp/`, `crates/app/src/mcp/`, the
server list and grant picker in `crates/app/src/settings/mod.rs`, external-source receipts in
`crates/app/src/workspace/{notes,layout,mod,transcript}.rs`, `external_citations` in
`crates/insight/src/notes/schema.rs` and its consumers, and differentiator #3 in `AGENTS.md`.

## Why this exists

The maintainer, 2026-08-28: *"we don't [want] sotto to consume any mcp, I want sotto to be purely
transcription, summarizing and ability to ask questions across historical transcriptions and
summaries."*

Sotto's product surface is three things: transcribe locally, summarise the result, and answer
questions across the whole library. Consuming MCP was a fourth — pulling outside documents in as
evidence — and it is being withdrawn, not deferred.

This is a withdrawal of *consumption only*. Sotto exposing **itself** as an MCP server, so an
external assistant can read the record and write notes back, is a separate and still-wanted
direction; it is T105's subject and nothing here should make it harder. That is why this task
deletes the client and its UI rather than the `rmcp` dependency wholesale.

## What "consume MCP" currently spans

| Surface | Scale |
|---|---|
| `crates/mcp/` — client, streamable-HTTP transport, types | 2,638 lines |
| `crates/app/src/mcp/` — UI state, grants, persistence | 1,517 lines |
| `crates/app/src/settings/mod.rs` — server list, add/remove, grant picker | 75 references |
| `crates/app/src/workspace/{notes,layout,mod,transcript}.rs` — source receipts, evidence chips | 68 references |
| `crates/insight/src/notes/{schema,mod,overlay}.rs`, `crates/app/src/notes/controller.rs`, `crates/providers/src/codex/` | 14 references |

## The part that is not deletion

`crates/insight/src/notes/schema.rs` puts `external_citations: Vec<EvidenceId>` on **every** note
item — overview, decisions, action items, and separately on action owners and due dates. It is a
required field of the JSON schema the reasoning backend is prompted against, and it is present in
notes already persisted.

**Decision: drop the field, deserialize tolerantly.** New notes carry no `external_citations`.
Stored notes that have it still load, and the field is ignored. No migration runs and no derived
artifact is rewritten. The alternative — migrating stored notes — buys tidiness on disk at the cost
of rewriting artifacts that are recomputable anyway; and leaving the field permanently empty would
keep a term in the model contract that can never be populated, which is the debt that outlives its
reason.

`schema.rs:123` documents that a missing field turns a well-formed set of notes into
`missing field 'external_citations'`. That error path goes away with the field, and its comment
must go with it rather than being left describing a schema that no longer exists.

## Decisions

1. **`crates/mcp/` is deleted, not emptied.** T105 will need a server, and a server is not a
   client with the calls reversed — the transport, the types and the error model all differ. Keeping
   a hollowed-out crate to "save work later" would hand T105 a shape built for the opposite
   direction. `rmcp` returns as a dependency when T105 needs it.
2. **No persisted user data is lost.** The maintainer's Application Support directory holds
   `models/`, `reasoning-settings.json`, `recordings/`, `sotto.sqlite3`,
   `transcription-settings.json` and `workspace-state.json` — and no MCP settings file. No server
   was ever configured, so removal discards nothing real. Any MCP settings file found at runtime is
   left on disk untouched rather than deleted; removing a feature is not licence to delete a file
   the user may want to inspect.
3. **AGENTS.md differentiator #3 is deleted, not rewritten.** "Bring your own context" described
   consuming MCP and nothing else. Six differentiators, and the remaining five are unaffected.
   Rewriting #3 to describe *serving* MCP would state T105's claim before T105 exists.
4. **Ask is untouched.** ADR-0020 already makes Ask read the whole local library; it never
   depended on MCP. Same for transcription and summarising.

## Order of work

Each step must leave the tree compiling and the gates green, so the removal proceeds inward from
the leaves rather than deleting the crate first and chasing errors outward.

1. `crates/insight/` — drop `external_citations` from the schema and its validation, tolerant
   deserialization for stored notes.
2. `crates/providers/src/codex/` — remove the MCP-shaped event and config surface.
3. `crates/app/src/workspace/` and `crates/app/src/notes/controller.rs` — remove evidence chips,
   source receipts and their state.
4. `crates/app/src/settings/mod.rs` — remove the server list, add/remove and grant picker.
5. `crates/app/src/mcp/` — delete the module and its wiring in `lib.rs` / `main.rs`.
6. `crates/mcp/` — delete the crate and its workspace member entry.
7. `AGENTS.md` — delete differentiator #3; renumber the remaining five.

## Acceptance

- No `mcp` identifier, module, dependency or workspace member remains in the tree.
- Notes generate, persist and render with no `external_citations` anywhere in the schema, the
  prompt, or the UI.
- A note persisted *before* this change still loads, with its stored `external_citations` ignored
  rather than rejected. Asserted by test against a fixture captured from the old format.
- Transcription, summarising, and Ask across the library are unchanged; their existing tests pass
  unedited. If one needs editing, something outside this task's scope moved.
- Settings has no MCP surface and no dead affordance where one used to be.
- `AGENTS.md` states five differentiators, and no remaining text claims Sotto consumes outside
  context.
- Full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Sotto *serving* MCP — T105. Anything about the reasoning backends themselves, which are a separate
axis from where their evidence came from. The `sotto.sqlite3` schema: if it carries MCP-shaped
tables, they are left in place and unused rather than migrated, because a schema migration to
delete an unused table risks live data to gain nothing.

## Notes

Filed 2026-08-28. The MCP client was built out under differentiator #3 ("Bring your own context")
and is being withdrawn because the product's answer to "what is Sotto" narrowed to three things it
does itself. Worth recording that the withdrawal is of a *direction*, not a *defect*: the client
worked. It is being removed because carrying a capability nobody chose is how a lightweight-by-
construction product stops being one — differentiator #5, which survives this task and is part of
why it exists.
