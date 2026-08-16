# T099 — The notes document must survive being edited

**Status:** todo

**Wave:** N8 — entry workspace

**Depends on:** nothing. T087 built the overlay and T098's mounted tests found these; both are
`in-review` and both close on this.

**Owns:** `crates/app/src/workspace/notes.rs`, the composer input construction in
`crates/app/src/workspace/mod.rs`, and this task. Coordinate with T098's lane if it is still active.

## Why this exists

T098 mounted the notes document and exercised it the way a person does. Three defects fell out, and
none of them is visible from the data layer — T087's overlay model is correct and thoroughly tested.
They are all in the last step, between a correct document and what reaches the screen.

Filed rather than left in a handoff message: every one of them is a claim T087's acceptance already
makes.

## 1. Clicking Edit on an action item crashes the app

The worst of the three, and the only one a user will meet as a crash rather than as a
disappointment.

`begin_notes_block_edit` (`crates/app/src/workspace/notes.rs:732`) composes a multi-line string for
an action block:

```rust
format!("{text}\nOwner: {}\nDue: {}", owner.unwrap_or_default(), due_date.unwrap_or_default())
```

and hands it to `self.annotation_input.update(cx, |input, cx| input.set_value(editable, …))`. That
input is built at `crates/app/src/workspace/mod.rs:475` as
`InputState::new(window, cx).placeholder("Add a note to this recording")` — **single-line**, with no
`.multi_line()`. Newlines reaching it panic.

An `Edit` control is drawn on every action block in every summary. This is not an edge case.

Decide the fix rather than reaching for the first one: make the composer multi-line when a block
edit is active, give owner and due date their own inputs, or edit the action's text alone and put
owner/due behind separate controls. The `"Editing block. Its Owner and Due lines are part of this
verbatim edit."` message implies the first; the acceptance below does not require it.

## 2. A reworded block does not reach the selectable text

Save applies the edit — T098 asserts the composed document holds the user's exact wording — but
copying `summary-claim-0` after the refresh still returns the original generated sentence. The
document is right and the screen is stale.

This is T087's central promise failing at the last step: *"A generated block can be reworded; the
document **shows** the user's words with provenance."* A reader looking at a block they just
reworded sees the model's sentence.

T098's mounted test records the gap in place and deliberately does not assert around it. When this
is fixed, that comment comes out and the assertion goes in — the test is already positioned for it.

## 3. No reorder control is rendered

T087 lists reorder among the operations it implements, and the overlay model supports it, but
nothing draws it. Either render it, or state plainly in T087 that reorder exists in the model and is
not yet reachable, and remove the claim from its handoff. An operation that exists only in the data
layer is not an operation the product has.

## Acceptance

- Editing an action item — text, owner, and due date — cannot panic, and a mounted test drives that
  exact path. The crash is what filed this task; a fix without a test that would have caught it is
  not enough.
- A reworded block's selectable text is the user's wording, asserted by copying it in a mounted
  test, replacing T098's recorded gap comment.
- Reorder is either reachable and asserted, or explicitly recorded as model-only in T087 with its
  claim withdrawn.
- The overlay model, its persistence, and its composition are unchanged — T087 proved those and
  this task is about what reaches the screen.
- No acceptance already proven by T098 regresses, in particular the three provenance states and the
  user block that draws no citation chip.
- Full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

The overlay model and its persistence, adding operations, restyling, and the manual/visual gates
T087 and T098 leave to the maintainer.

## Notes

Filed 2026-08-16 from T098's handoff. Worth recording how these were found: the data layer was
proven correct and the presentation was not, and every one of these defects lived in the gap
between. T098's own value was refusing to write a test that passed against broken behaviour — the
reword gap is documented in a code comment at the assertion that would otherwise have papered over
it. T087 and T098 both close when this closes.
