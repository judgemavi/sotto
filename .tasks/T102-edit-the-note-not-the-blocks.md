# T102 — Edit the note, not the blocks

**Status:** todo

**Wave:** N8 — entry workspace

**Depends on:** T087's overlay, which this builds a new surface over and does not change. T089 owns
`crates/app/src/workspace/**` until it closes; coordinate before starting.

**Owns:** the notes editing surface in `crates/app/src/workspace/notes.rs`, the text↔operation
translation wherever it lands, and this task. **The overlay model, its persistence, and its
composition in `crates/insight/**` are read-only here** — this task adds a way to author the
operations T087 already defines, and must not add, remove or alter one.

## Why this exists

The maintainer, on the shipped editor: *"current editing is block based, I was hoping for more of a
markdown style editor, like full note edit."*

The block controls have been generating friction the whole way, and the record shows it rather than
merely suggesting it:

- **T099 exists because clicking `Edit` on an action item crashed the app.** Its fix composes a
  multi-line string — the block's text, then `Owner: …`, then `Due: …` — sets it on the shared
  composer, and **parses those lines back out on save** (`parse_editable_block`). That is a text
  editor wearing a block editor's clothes, and it is fragile in an obvious way: a block whose last
  line happens to begin `Due:` is misread.
- **Reorder has no control at all.** T087 implements it in the model and T099 recorded that it is
  unreachable, because there was no good place to put one. In a text editor, moving a paragraph
  *is* the reorder.
- The composer is shared with typed notes, which is why plain Return had to be given two meanings
  and why `composer_edits_notes_document` exists.

Every one of those is the same shape: a document being edited one block at a time through a surface
that was built for something else.

## What must not change, and why

This is not a request to make the summary a text file. Three properties are load-bearing and all
three come from block identity:

1. **Regeneration merges instead of destroying.** Re-summarise a recording and matched edits carry
   over, by citation-anchor overlap. Without stable block ids there is nothing to match: regeneration
   either discards the person's work or can never be offered again.
2. **Provenance is structural.** Generated, edited, and user-authored are distinguishable because
   they are *different layers*, not because a flag is maintained. Free text cannot answer the
   question.
3. **Citations anchor to claims.** A chip belongs to a block. Loose prose has nowhere to hang
   evidence, and evidence is what separates this product's summary from any other.

Those are the things Sotto is actually selling. The editing surface is not worth one of them.

## The approach

The good news is that no trade is required. T087's overlay is an append-only log of `Add`,
`Reword`, `Hide` and `Reorder` operations against block ids — **which is exactly what a text diff
produces.**

Render the composed document as one continuous markdown surface. Let the person edit all of it —
type between paragraphs, delete one, move one, reword mid-sentence. On save, **diff the edited text
against the text that was rendered, and emit the operations that explain the difference**:

| What the diff sees | Operation |
|---|---|
| paragraph unchanged | none |
| paragraph's text changed | `Reword` on that block id |
| paragraph gone | `Hide` on that block id |
| paragraph moved | `Reorder` |
| paragraph that matches no rendered block | `Add` as a user block |

The document reads and edits as one note. Underneath, block identity is untouched and every
existing guarantee holds. This is how block-backed editors generally work — continuous to the
writer, a tree to the system.

## The risk that must be designed for, not discovered

**Matching an edited paragraph back to its block id is where this fails badly rather than
loudly.** A wrong match silently reattributes one person's words to another author, or moves a
citation onto a sentence it does not support — precisely the failure the two-layer design exists to
prevent.

It is tractable: the diff runs against text the person just looked at, so ordering is mostly stable,
and position plus text similarity resolves the ordinary cases. But the ambiguous cases must **fail
visibly** — refuse the save and say what is ambiguous, or fall back to `Add` + `Hide` (which loses
the link but never lies about authorship) rather than guess at a match. Decide which, and record
why. A confidence threshold picked by feel and left unstated is not acceptable here.

## Two decisions this task must make

1. **Owner and due date.** They are structured fields on an action item. In a text surface they
   either become syntax the person types — `- [ ] Ship the checklist @priya ^friday`, or similar —
   or they leave the text and live beside it. The current `Owner:` / `Due:` line convention is a
   workaround from T099, not a design; do not carry it forward by default.
2. **Citations in edit mode.** Do chips render inline in editable text, or does the editor show
   plain prose while read mode shows chips? Editing text with embedded chips is where this class of
   editor usually becomes unpleasant. Pick one, and say what the person sees when they edit a
   sentence that carries evidence.

## Acceptance

- The whole notes document is editable as one continuous markdown surface: reword mid-paragraph,
  add a paragraph, delete one, and reorder by moving text, all without touching a per-block control.
- Each of those produces the corresponding overlay operation, asserted by test against the stored
  overlay — not merely against what is rendered.
- A generated block reworded through the text surface keeps its citations and its
  `edited-from-generated` provenance. A newly typed paragraph is user-authored and carries no
  citation. Asserted by test.
- Regeneration after a full-document edit still merges: matched edits carry over verbatim, orphaned
  edits are kept and flagged. T087's existing tests cover this and must pass **unchanged** — if one
  needs editing, the storage model moved and that is out of scope.
- An ambiguous match fails visibly by the rule this task records. A test drives one.
- The per-block Edit control and the `Owner:`/`Due:` composer convention are removed, not left
  beside the new surface. Two editors for one document is worse than either.
- A mounted test drives the real surface, as T098 established. The presentation is the deliverable
  here, so data-layer tests alone do not close this.
- Full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

The overlay model, its persistence, and its composition — T087 owns those and they are proven.
Changing what a summary contains or the section taxonomy. Rich-text formatting beyond what the
document already renders. Editing the transcript, which is captured fact.

## Notes

Filed 2026-08-16 from the maintainer's direct feedback while reviewing the notes column. Worth
recording the shape of this correction, because it is the second one this week: T088 was withdrawn
because a *direction* ("Obsidian-shaped") was built out as a decision, and this task exists because
a *storage model* (block identity, which is right) was allowed to dictate an *interface* (per-block
controls, which is not). The block controls were never chosen — they fell out of the data model
because nobody asked what editing should feel like.
