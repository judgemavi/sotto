# T087 — The notes document: a summary you can edit without losing either author

**Status:** blocked

**Wave:** D1 — living notes

**Depends on:** T070 (`todo`) — the block-identity artifact schema this task merges against, and
the owner of `crates/insight/src/notes/**` and `crates/app/src/notes/**` until it closes. T086 —
the entry the document hangs off. Blocked until both close.

**Owns:** the edit-overlay model and merge in `crates/insight/src/notes/**` (sequential handoff
from T070), overlay persistence as its own module in `crates/rag/**`, `crates/app/src/notes/**`
and `crates/app/src/workspace/notes.rs` (sequential handoff from T070/T076), and this task

## Why this exists

The generated summary is read-once. The artifact people keep is a document: the AI draft plus
their corrections, their additions, and their checked-off action items. Today a person who
disagrees with a generated claim can do nothing about it, and regenerating throws away nothing —
because there is nothing of theirs to throw away. That inverts as soon as editing exists, and
ADR-0021 fixes the shape before the first edit is ever made: **two layers, merged
deterministically.**

## The design, fixed by ADR-0021 — not this task's to relitigate

- Layer 1: the generated artifact. Immutable, versioned, cited, recomputable. T070's schema gives
  every claim and action item a stable block id derived from its citation anchors.
- Layer 2: an append-only log of user operations against block ids: add, reword, check/uncheck,
  hide, reorder. User text is never sent through a model and never altered.
- Render = layer 1 ⊕ layer 2. Provenance (generated / edited-from-draft / user-authored) is
  which layer the content lives in, displayed quietly on every block.
- Regeneration merge is deterministic, by citation-anchor overlap. Matched edits carry over
  verbatim; a checked item stays checked when its counterpart cites the same moments; an orphaned
  edit is kept as a user block and flagged, never dropped.

## Plan

1. Define the overlay operation log and persist it per entry in `rag`, append-only, with the
   artifact version each op was made against. The overlay survives regeneration by construction —
   it is never rewritten, only appended.
2. Implement render composition in `insight`: artifact version ⊕ overlay → the presented document.
   Hidden blocks are absent, reworded blocks show user text with the edited-from-draft mark,
   user-added blocks carry no citations and never claim any.
3. Implement the merge: on a new artifact version, re-bind every overlay op by block identity
   (citation-anchor overlap per T070's schema). Record per op whether it bound, and surface
   orphaned edits as kept user blocks with a visible note that the regenerated summary no longer
   contains what they edited.
4. Make action items checkable, with owner and date editable as block rewords. Checked state is an
   overlay op, so it survives regeneration like any other edit.
5. Rebuild the notes column interaction: edit-in-place per block, add-a-block, hide, check. Calm —
   affordances appear on the block under the pointer, not forty controls at rest. The separate
   "your notes" composer block retires here per ADR-0021; post-close anchored annotations
   (T056's path) surface as user-layer blocks instead. The live-call composer is untouched.
6. Staleness stays T056's: the artifact can be stale against the timeline hash; the overlay is
   never stale. A stale artifact with fresh edits renders, marked, and regenerating is the
   person's choice.
7. Prove the merge with fixtures: reworded-and-matched, checked-and-matched, orphaned-and-kept,
   hidden-stays-hidden, and a full regeneration where every user word survives byte-identical,
   asserted over the composed document.

## Contract for downstream tasks

The composed document — blocks, provenance, block ids, checked state — is the single notes surface
T088 projects to markdown and T090/T093 read open action items from. Nobody else reads layer 1
directly.

## Acceptance

- A generated block can be reworded; the document shows the user's words with provenance, and the
  underlying artifact version is byte-unchanged, asserted by test.
- Regeneration preserves every overlay edit whose block identity matches, verbatim; a
  non-matching edit is kept and flagged, never dropped — both asserted over composed output.
- Checked action items survive regeneration when their citations match.
- A user-added block never carries or implies a citation; provenance for all three block states is
  visible and correct.
- No path serializes overlay content into a reasoning request, asserted over the serialized
  request the way T066 asserts image absence.
- Overlay persistence is append-only and replays identically after restart.
- Focused and full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

The artifact schema itself (T070), the markdown projection (T088), entry UI (T089), commitment
carry-forward (T093), rich text, and any change to live-call annotation.
