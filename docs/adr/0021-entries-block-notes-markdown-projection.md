# ADR-0021: The entry above the recording, block-identity notes, and the markdown projection

- Status: Accepted
- Date: 2026-08-14
- Decision owners: Sotto maintainers
- Amends: ADR-0016 (workspace shape), ADR-0019 (recording-centric vocabulary), ADR-0007 (derived
  view persistence)
- Upholds: ADR-0004 (append-only timeline), ADR-0015 (no canvas/graph surface), ADR-0018 (recording
  as source of truth), ADR-0020 (app-level Ask)

## Context

Three pressures arrived together and turn out to be one decision.

**The recording is the wrong top-level unit.** Today an entry in the library *is* a recording
session: it exists only once capture starts, and everything hangs off it. But meetings exist before
they are recorded — the prep note, the agenda, the brief — and one occasion sometimes spans several
captures: the call that dropped and restarted, the huddle before the meeting proper. The product has
no place for either. T078 shipped a Home you can return to; there is still nothing to *prepare in*.

**Notes want to be edited, and edits must survive regeneration.** The generated summary is read-once
today. The artifact people actually keep is a document: AI draft plus their own corrections,
additions, and checked-off action items. A naive "make the summary editable" breaks the standing
rule that derived views are recomputable and that model output never masquerades as anything else —
and a naive regeneration throws the person's edits away.

**The notes should live where the person's other notes live.** Meeting knowledge that is trapped in
an app database dies there. The maintainer's direction is Obsidian-shaped: meetings as linked
markdown notes, graphable, greppable, owned as plain files. Building a PKM inside Sotto would
re-litigate ADR-0015; projecting into the user's existing one does not.

## Decision

### 1. The entry is the unit of the library

An **entry** is the top-level object: it has an identity, a person-chosen title (subsuming T085's
recording title), a notes document, and **zero or more recording sessions** attached to it.

- An entry may exist before any capture: created deliberately, holding prep notes. Its empty state
  reads as *prepared*, never as a failed recording.
- Each attached session keeps everything it has today, unchanged: its own append-only timeline, its
  capture scope, its media-time clock, its recording. Nothing about ADR-0004 or ADR-0018 moves.
- The rule for what hangs where: **facts hang off sessions; documents hang off entries.** Timeline,
  recording, capture target: session. Notes document, prep notes, title, brief: entry.
- Citations already carry a session id (T054). A claim in an entry's notes cites
  `(session, event)` — an existing shape, not a new one.
- **A recurring meeting is a series of linked entries, not one entry accumulating recordings.**
  Multiple sessions on one entry are for one *occasion* (dropped call, huddle + meeting). A weekly
  standup is one entry per occurrence, linked to a series page. Aggregation across occurrences is a
  derived view over links, never a mutation of any entry.

### 2. The notes document is two layers with block identity

The entry's notes document is rendered as `generated artifact ⊕ user edit overlay`.

- **Layer 1 — the generated artifact** stays exactly what ADR-0007 says it is: immutable, versioned,
  recomputable, every claim cited. Regeneration produces a new version; no version is ever edited.
- **Layer 2 — the user edit overlay** is an append-only log of operations against block ids: add a
  block, reword a block, check/uncheck an action item, hide a block, reorder. The overlay is
  user-authored content and is never sent through a model.
- **Every generated claim and action item is a block with a stable id**, and block identity derives
  from the claim's citation anchors — the transcript events it cites. That is what makes
  regeneration mergeable.
- **The merge is deterministic, never model-mediated.** On regeneration, new blocks are matched to
  old ones by citation-anchor overlap. Edits whose target matches carry over; user text survives
  **verbatim**; a checked item stays checked when its regenerated counterpart cites the same
  moments. An edit whose target vanished is kept as a user-authored block and flagged, never
  silently dropped. No path feeds user edits to a backend to "integrate" them: that would mangle
  user words, expand disclosure, and destroy provenance in one move.
- Provenance is structural: a reader (and the UI) can always distinguish AI-generated,
  AI-generated-then-edited, and user-authored, because the distinction is which layer the content
  lives in — not metadata anyone maintains.
- The live-call annotation gesture (T051/T056) is unchanged: a mark anchored to a transcript moment
  is a timeline event and remains one. Post-call, anchored annotations surface inside the notes
  document as user-layer blocks; the notes document is the one post-call writing surface. The
  separate "your notes" block in the summary column retires when this ships.

### 3. ~~Notes project to a local markdown vault; the record projects read-only~~ — withdrawn 2026-08-16

**Withdrawn by the maintainer.** Everything Sotto stores stays in SQLite and notes are edited in the
app. There is no markdown mirror, no user-chosen vault folder, no `sotto://` scheme, and no series
page.

What was actually wanted from this section is decision 2 above: **editable summaries** — the
block-identity overlay, delivered by T087, which lets a generated claim be reworded, hidden, checked
or answered with a block of your own, with provenance, surviving regeneration. That shipped. This
section extrapolated a file format, a URL-scheme registration and a bidirectional sync engine from
it, on the strength of one line in this ADR's own context: *"the maintainer's direction is
Obsidian-shaped."* A direction was carrying the weight of a decision.

Recorded struck through rather than deleted, because a withdrawn decision is more useful to the next
reader than a missing one. The cost was roughly 1,500 lines and a wave of the task board, and it was
reversible only because the projection turned out to be a leaf that nothing depended on.

**Kept from this section:** the entry-series relation on `Entry` and the schema migration that
persists it. Recurring meetings as linked entries is decision 1's business, not the vault's, and both
T090's brief and T093's carried-forward commitments read the series relation.

## Consequences

- **Vocabulary amends ADR-0019 upward, not backward.** Below the entry, everything ADR-0019 says
  about recordings stays true and recording-centric. The library, however, lists entries; the rail,
  Ask's scope labels, and the delete prompt speak of entries once the model lands. In-flight N7
  tasks finish against ADR-0019 as written; the entry vocabulary arrives with the N8 wave.
- **Schema:** a new entry entity above sessions, migration on top of the current head. Existing
  libraries migrate mechanically — one entry per existing session, title carried over from
  `session_titles`. An entry with zero sessions is representable and honest everywhere (search,
  footprint, Ask scope).
- **T085's title moves up:** the title becomes the entry's; a session keeps only its captured
  facts. Search still matches both chosen and captured names.
- **The notes schema (T070's successor artifact kind) must carry block ids** derived from citation
  anchors, or the overlay and the vault have nothing to address. This is a requirement on T070,
  recorded there.
- **Staleness logic extends, not changes:** the timeline hash still governs generated-artifact
  staleness (T056); the overlay is never stale because it is not derived.
- **Cross-session Ask and retrieval gain an entry dimension** eventually (ask about a series), but
  nothing in ADR-0020 changes now: the library scope is still the default, and indexing still
  happens at stop.
- **Failure honesty:** a vault mirror that cannot write (folder gone, permissions) degrades to
  in-app notes with a visible state, never a silent stop. An external edit that cannot be parsed
  into overlay ops is shown to the person, never discarded and never "fixed" by a model.

## Non-goals, restated so they do not drift

- No in-app graph view, canvas, or spatial surface (ADR-0015).
- No freeform-first editing: the document *feels* freely editable but is block-structured;
  block identity is what citations, provenance, and merge stand on.
- No model-mediated merge of user edits, ever.
- No cloud sync of the vault by Sotto; the folder is the user's, synced by whatever syncs their
  files.
- The vault never becomes the source of truth for the record.
