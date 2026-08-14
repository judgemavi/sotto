# T052 — Smooth streaming transcript presentation

**Status:** done

**Wave:** N4 — workspace layout

**Depends on:** T049 for `workspace/transcript.rs`. T005 and T018 are accepted history.

**Owns:** `crates/app/src/workspace/transcript.rs`, a pacing module under
`crates/app/src/workspace/`, and this task

## Why this exists

The transcript currently renders as plain divs tracked by a `ScrollHandle`. Two problems follow.

It does not virtualize, so a ninety-minute session lays out every row every frame. `uniform_list`,
used by the diagnostic window, cannot fix this: wrapped transcript rows are not uniform height.
`gpui::list()` with `ListState` is present in the pinned 0.2.2 and is built for exactly this —
variable heights, append-only, anchored to the bottom.

And it renders ASR output at ASR cadence. That reads as jerky, which invites the wrong conclusion —
that smooth streaming is impossible on a local sliding-window transcriber. It is not. `crates/asr`
already runs a LocalAgreement stabilizer: a 2.5 second unstable tail, commit after
`agreement_passes` consecutive agreeing passes, and committed audio never re-entering an inference
window, so **finals cannot retract** — there is a green test asserting that. Committed text is
already monotonic. Only the tail churns, and only a handful of words are ever in it.

So smoothness is a presentation problem, and this task solves it in the presentation layer without
touching the commit policy.

## Plan

1. Migrate the transcript column to `gpui::list()` / `ListState`. Keep follow-live, the manual-scroll
   pause, and citation focus (`scroll_to_item` against the focused `EventId`) working across the
   migration.
2. Render the two registers separately:
   - committed finals flow in the list, append-only;
   - the current unstable partial renders in a fixed strip below the list, one per audio stream.
   The strip is deliberately outside the list. A rewriting hypothesis then cannot re-wrap committed
   text, cannot shift rows above it, and cannot force `ListState` to re-measure a resizing item —
   which is also the fiddliest failure mode of the migration in step 1.
3. Add a pacing buffer between the timeline seam and the list. Committed words arrive in bursts;
   release them at a steady 40–60 ms cadence. Bound the queue so display never trails the commit
   point by more than ~1.5 s, and drain it immediately on Stop so a stopped session is never missing
   text that was already committed.
4. Gate the unstable strip on VAD. Whisper hallucinates on silence, and a phantom phrase that later
   vanishes is the worst possible non-smoothness — it is a visible retraction in a product whose
   contract is that finals never retract.
5. Style the two registers so the distinction is legible without a legend: committed text is the
   record, the strip is provisional.
6. Keep the AGENTS.md calm requirements green: no duplicate rolling hypotheses, at most one live
   partial per stream, no automatic scroll after the user scrolls away, and a visible Follow live
   action to restore it.

## Contract

- Presentation only. The commit policy, `TranscriberConfig` defaults, and the `TranscriptUpdate`
  contract from T018 are unchanged. If this task believes a config default should move, it reports
  back rather than editing `crates/asr`.

## Acceptance

- A test feeds a recorded hypothesis sequence — including a tail that changes across passes — and
  asserts the committed text rendered is monotonic, never reordered, and never shortened.
- The pacing buffer drains fully on Stop: a stopped session's rendered transcript equals its
  persisted finals exactly.
- Scroll and layout are observed, not assumed: over a real session of at least ten minutes, record
  that committed rows never reflow when the unstable strip changes, and that scrolling stays
  responsive with the full session in the list. This observation belongs with T035's session gate.
- Silence produces no rendered text in the unstable strip.
- Citation focus still scrolls the exact `EventId` into view after the `ListState` migration.
- Focused and full app tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Changing the stabilizer, the window, the hop interval, or agreement passes; speaker diarization;
word-level timestamps; and any animation beyond the pacing of already-committed text.

## Notes

- The transcript uses GPUI's variable-height `list` with committed rows kept out of the unstable
  strip. Pinned user annotations render beneath the exact committed `EventId` without changing the
  committed-row citation index.
- The presentation pacer releases one word every 50 ms and bounds its remaining queue to 30 words
  (1.5 seconds at that cadence). A stop drain releases every accepted word immediately.
- Deterministic tests cover monotonic committed presentation, bounded lag and stop drain, silence
  gating, latest-partial selection, and exact citation indexing.
- Formatting, focused/full app Rust tests, and strict app Clippy pass after shared workspace
  integration. The ten-minute real-session observation, Metal runtime behavior, and visual
  acceptance remain `NOT RUN`; those observations still belong to T035's signed-app gate.

### Closure — 2026-08-13 (planner, narrowed)

Accepted on automated evidence: the `ListState` migration, the pacing buffer, the unstable strip
rendered outside the virtualized list, and the preserved follow-live and citation-focus behaviour.

Residual to T055, because it lands squarely on this task's own contract: annotation anchor
resolution runs a full `replay_lenient` plus two whole-event scans per annotation, per frame, from
`layout.rs:20`. With the pacer releasing words every 40-60 ms this repeats roughly twenty times a
second over the entire session. It is the most likely cause of any late-session scroll degradation
and would be easy to misattribute to list virtualization.

The ten-minute scroll, reflow and responsiveness observation remains with T035 and is not claimed.

