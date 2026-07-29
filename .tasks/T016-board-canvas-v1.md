# T016 — Board canvas v1: the timeline as a readable whiteboard

**Status:** blocked (on T020 verdict, T012)

**Wave:** 3 — Phase 2, the note-taker dogfood gate

**Depends on:** T003 (**do not start until the spike passes**; on a fail this task is
rewritten against the Electron fallback) · T011 (live timeline) · T012 (the tokio↔GPUI
seam and the dev window it establishes) · T015 (screen snapshots to pin)

**Owns:** `crates/app/src/board/**`

## Goal

The first half of the Phase 2 dogfood gate. `AGENTS.md`: *"If the timeline isn't good
enough to read, it isn't good enough to reason over."* This task makes the fused
audio+screen timeline readable as a spatial artifact — and the honest test of whether the
whole premise works. No advising is built until this is good.

Note the explicit non-goal: the note-taker is an **internal milestone, never shipped as a
product**. Build it to be used by us, on real calls, not to be demoed.

## Plan

1. **Utterance blocks along the time axis**, coloured per speaker, laid out with the
   append-only discipline the spike proved: position computed once on arrival, never
   recomputed. A correction (`supersedes`) updates a block's text **in place** without
   moving it or anything after it — this is exactly why T014 made the timeline
   append-only, and it is the property that makes the board glanceable.

2. **Prosody as space, not decoration.** A long pause renders as a literal gap;
   interruptions overlap; speech rate can modulate block density. This is the cheapest
   high-value thing on the board — the rep sees hesitation without reading a label.
   Use T006's annotations; do not invent a second prosody source.

3. **Screen thumbnails pinned to their interval.** Each `ScreenSnapshot` pins along the
   stretch of conversation it was visible for, using `visible_from..visible_to`. Load
   thumbnails lazily by `FrameRef` and evict off-screen ones — the board must not hold a
   two-hour call's frames in memory.

4. **Panning and zoom** per the spike's approach, with a "follow the frontier" mode that
   auto-scrolls to newest content and yields the moment the user pans away. Returning to
   the frontier should be one obvious gesture.

5. **Visual calm is a hard requirement**, not polish. New objects arrive gently at the
   frontier; nothing already read jumps or reflows. If a layout decision trades calm for
   density, choose calm — `AGENTS.md` makes this a differentiator, and a board that
   twitches during a live call gets closed exactly like a chatty copilot does.

6. **Post-call review** is the same view with no live tail: pan the map of the
   conversation rather than scrub a transcript. Loading a persisted timeline from SQLite
   (T008) must produce a board identical to the one built live — replay equivalence is
   testable, so test it.

7. **Do not build** suggestion cards, topic clustering, or open-objection tracking here.
   Those are Phase 3/4 and depend on the advisor. Leave the anchoring seam — a block must
   be able to host a card budding off it — but no advising UI.

## Acceptance

- A real recorded call renders as a readable board: correct speakers, visible prosody,
  screen thumbnails on the right intervals.
- Corrections update in place with zero movement of surrounding content.
- Replaying a persisted timeline reproduces the live board exactly.
- Frame time and memory stable over a two-hour session with thumbnails.
- **The gate:** we use this on real calls and agree the timeline is accurate and readable.
  That judgement is the deliverable — record it in the task notes with what was wrong.

## Out of scope

Suggestions and any advising UI, topic clustering, user annotations (Phase 4), the
overlay lens (T012 owns the seam; the production lens is Phase 4), shipping this.


## Amendment — the board is now a shipping tier, not a dogfood gate (2026-07-29)

`AGENTS.md` has changed underneath this task. The map is no longer an internal milestone we
use and discard: it is **the first usable product tier, and it ships**. It works with no API
key configured, and onboarding runs through it — a rep tries Sotto on one call before anyone
asks them for a credential.

That raises the bar here. Previously "readable enough to prove the timeline works" was
enough. Now:

- **It must be good standalone.** Someone with no model configured should find reviewing a
  call on this board genuinely better than scrubbing a recording. That is the bar.
- **No dead ends where the reasoning tier would be.** With no key configured there is no
  empty "summary" panel, no greyed-out "clusters" button, no error state. Absence of the
  reasoning tier is a *normal* configuration, not a degraded one — the UI should not imply
  the user is missing something broken.
- **Leave the second axis room.** T019 produces topic regions, cross-links and open threads
  as a derived view keyed by `EventId`. The board renders that as an **optional overlay** the
  user can switch off, falling back to pure chronology. Design the layout so clusters can be
  layered on later without reflowing what chronology already placed — the append-only
  discipline you already need for live capture is the same mechanism.

The gate still applies — use it on real calls and agree the timeline is accurate and
readable before advising work starts — but it is now a product bar as well as a checkpoint.


## Amendment — the gate does not need sales calls (2026-07-29)

Clarified in `AGENTS.md`: capture is scoped to any window or application, so validating this
tier does not require access to a sales pipeline. Any two-party conversation exercises
everything the map tier does — two audio streams with speaker attribution, prosody, screen
frames, OCR, board layout, post-call review. A 1:1 with a colleague is sufficient: mic is
you, the captured app is them.

Only the advisor's validation needs real sales calls, because trigger types and battlecard
grounding are the sales-specific parts. Do not block this gate on sales-call access.
