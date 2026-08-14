# ADR-0015: Replace the Board lens with one top-down meeting workspace

- Status: Accepted
- Date: 2026-08-12
- Decision owners: Sotto maintainers

## Context

The GPUI Board proved that Sotto could project the append-only timeline onto a zoomable canvas, but
real smoke testing showed that the spatial representation made the primary record harder to read.
Utterances appeared as small or overlapping bubbles, zoom state obscured content, and users had to
switch lenses to move between AI notes and the evidence supporting them.

The canonical timeline, persistence, local transcription, citations, and derived-note architecture
do not require spatial rendering. The product's minimum value is a readable live transcript and
cited meeting notes.

## Decision

The shipped meeting workspace is one top-down view:

- A chronological transcript occupies the upper section. It shows wrapped text rows with timestamp
  and neutral speaker label, collapses superseded ASR hypotheses, and follows live output until the
  user scrolls away.
- Structured AI notes and explicitly selected MCP source context occupy the lower section.
- A meeting citation focuses and highlights the corresponding transcript row.
- The product shell does not construct or expose `BoardCanvas`, a Board tab, zoom/pan controls, or a
  canvas overlay.
- Existing board code and tests may remain temporarily as historical implementation evidence, but
  they are not a product surface or a ship gate. Removing that dead surface can be a later cleanup.

The append-only session timeline remains the canonical data model. This decision changes only its
primary UI projection.

## Consequences

- Live transcript readability and note quality become the relevant human acceptance criteria.
- Screen frames, prosody, proposals, and external evidence remain typed timeline or derived data;
  they surface through inline evidence details instead of spatial placement.
- Canvas performance, zoom behavior, no-reflow geometry, GPU residency, and an always-on-top canvas
  overlay are no longer required for the meeting-copilot product.
- Proposal UI work must use inline, clearly typed system-output rows with citations rather than
  reintroducing a separate spatial surface.

## References

- ADR-0003
- ADR-0011
- T016
- T038
- T048
