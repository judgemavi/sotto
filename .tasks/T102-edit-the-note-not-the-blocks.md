# T102 — Edit the note, not the blocks

**Status:** done

**Wave:** N8 — entry workspace

**Depends on:** T087's overlay, which this builds a new surface over and does not change. T089
closed on 2026-08-18 and released `crates/app/src/workspace/**`.

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

## Decisions

Recorded 2026-08-18 before the surface was written.

**Matching.** Exact `(text, action)` longest common subsequence within each section first. Leftovers
are then paired **by position in that leftover list**. A positional pair whose texts share no
alphanumeric token of length ≥ 2 is not a reword: it becomes `Hide` + `Add`, which drops the
citation link but never reattributes. A positional pairing is **refused** (save fails, message
names the two paragraphs) when some leftover paragraph shares at least as many such tokens with a
**non**-positional leftover as it does with its positional pair, and that cross overlap is greater
than zero. That is the swap-and-reword case, and the two-similar-claims-collapsed-into-one case:
both have more than one honest explanation, so the save does not pick. There is no similarity
cutoff. Moving a paragraph to a different `##` heading is `Hide` + `Add` in those two sections
(section is part of identity; `Reorder` cannot change it).

**Owner and due.** Trailing fields on the action line: `- [ ] Ship the checklist | owner: Priya | due: Friday`.
Either field may be omitted. They are not separate lines and they are not `@` / `^` tokens, so
body text can mention a person or a day without being parsed as a field.

**Citations in edit mode.** The editor is plain markdown: headings, paragraphs, list items, and
action fields. No chips. Evidence stays on the block id; read mode still shows chips behind
`Show timecodes`. Editing a cited sentence is editing its words. The chips return when the person
leaves the editor.

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

## Implementation — 2026-08-18

The notes column now has one **Edit** control that opens the composed document as markdown. Save
diffs that text against the presented blocks and appends T087 operations; Cancel discards. Per-block
Edit/Hide and the `Owner:` / `Due:` composer convention are gone. Check remains on actions in read
mode. The bottom composer is typed notes only.

Translation lives in `crates/app/src/notes/document_edit.rs`. Overlay types in `insight` were not
changed. T087 overlay tests pass unchanged.

Decisions are recorded above: positional leftover pairing, refuse when a cross match is as strong,
`| owner:` / `| due:` trailing fields, plain prose in the editor.

### Verification

The four gates below were recorded as passing on 2026-08-18. They did not all pass: a later run on
2026-08-27 found two Clippy errors in this task's own code —

- `clippy::manual_ok_err` at `workspace/notes.rs:929` (`match result { Ok(()) => None, Err(e) => Some(e) }`),
- `clippy::redundant_clone` at `workspace/notes.rs:1792` (`target.clone()` in the check-control arm),

both of which `#![deny(warnings)]` rejects. Each was replaced with its exact equivalent
(`result.err()`, and dropping the clone since `target` is not used afterwards). Re-verified on
2026-08-27 across the whole workspace:

- `cargo test --workspace` — 590 passed, 0 failed, 41 targets.
- `cargo clippy --workspace --all-targets` — clean.
- `cargo fmt --check` — clean.
- `git diff --check` — clean.

Acceptance re-checked against the code rather than the claim: the mounted harness drives the real
document surface, the ambiguity refusal and the orphaned-reword warning each have a mounted test,
per-block `Edit`/`Hide` and the `Owner:`/`Due:` composer convention are absent from the tree, and
T087's overlay tests pass unedited. The documented "key path is NOT covered" note at
`notes.rs:1897` concerns evidence-chip keyboard activation and is a stated limitation of the mount
harness, not a gap in this task.

Closed 2026-09-02. Gates re-verified across the workspace after two Clippy errors in this task's own code were fixed; acceptance re-checked against the tree rather than the claim. The evidence-control keyboard residual moved to T035.
