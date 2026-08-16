# T099 — The notes document must survive being edited

**Status:** done

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

## Implementation notes — 2026-08-16

The shared composer is now a bounded auto-growing multi-line input. Ordinary Return retains its
existing submit behaviour during a live recording. In the stopped-recording document editor it
inserts a newline so verbatim block text and the action's Owner/Due payload remain editable; the
visible Save block control or secondary Return commits the edit. A mounted regression clicks the
generated action's Edit control, observes all three lines, replaces text, owner, and due date,
saves, and asserts the composed action fields.

Selectable notes text now keys GPUI's markdown state by both the logical text element and its
content. Rewording therefore creates a fresh parsed selectable view instead of reusing the old
generated sentence. T098's recorded gap comment is replaced by a mounted clipboard assertion over
`summary-claim-0`.

Reorder remains a proven overlay-model and persistence operation but has no product control. T087's
design and handoff now state that limit explicitly and withdraw any implied rendered-reorder claim.
The overlay model, persistence, and composition were not changed.

### Verification

- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test -p app notes:: --locked` — 43 passed.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo test --workspace --locked` — passed outside the sandbox;
  the sandboxed attempt could not bind the loopback sockets used by `mcp --test rmcp_http` and all
  four of those cases passed when the full command was rerun with loopback access.
- `WHISPER_DONT_GENERATE_BINDINGS=1 cargo clippy --workspace --all-targets --locked -- -D warnings`
  — passed.
- `cargo fmt --all -- --check` and `git diff --check` — passed.

## Review — 2026-08-16

Accepted. Both fixes were confirmed load-bearing by reverting them rather than by reading the
handoff. Removing `.auto_grow(1, 4)` makes the new action-edit test fail with
`gpui/src/text_system.rs:372: text argument should not contain newlines`, so the crash was real and
the test would have caught it. Reverting the selectable-text keying makes the reword assertion fail
holding the original generated sentence, so T098's finding was correct and the new clipboard
assertion is not vacuous. Root cause of that one is worth recording: `TextView` caches parse state
by element id, and the id was `("summary-claim", ordinal)` — ordinal-keyed, so it survived a reword
and would also have survived a hide or reorder shifting ordinals underneath it.

One regression was found in review and fixed here. Gating plain Return on `transcript_live` also
caught a path that is not block editing: on a **stopped** recording with no summary,
`submit_annotation` appends a typed note, and Return had always done that. The gate is now
`composer_edits_notes_document`, a predicate shared with `submit_annotation` so the key and the
button cannot drift apart, and `return_still_appends_a_typed_note_when_no_summary_owns_the_composer`
pins it — it fails against the shipped gate with the note absent from the reopened store.

Closing T087 and T098 with this. T098's file still read `todo` although its work was committed in
`2b6e10f`.
