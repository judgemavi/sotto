# T101 — A focus indicator you can actually see

**Status:** todo

**Wave:** N8 — entry workspace

**Depends on:** T100, which made focus move at all. This is the second half of that finding and
nothing else blocks it.

**Owns:** the focus indicator and its tokens — `crates/app/src/workspace/tokens.rs`, and whatever
shared wrapper the controls need. **Name the wrapper's home before writing it** and report if it
has to live in a file an open lane holds.

## Why this exists

T100 established that focus never moved, and fixed it. With focus now moving, the maintainer looked
at where it lands and found the indicator unreliable: legible on a solid button, close to invisible
on a ghost button, where only the corner arcs of the ring pick up enough contrast to read against a
dark surface.

**This cannot be fixed by choosing a better colour, and that is the whole point of filing it.**
`gpui-component` draws its ring at a hardcoded alpha:

```rust
// gpui-component-0.5.1/src/styled.rs:614
.border_color(cx.theme().ring.alpha(0.2))
```

T100 already set the token to pure black on light and pure white on dark — the maximum contrast
obtainable through that seam. Every colour we can supply is drawn at 20% opacity over a 1.5px
border. The ceiling has been reached, so the remaining options are to draw the indicator ourselves
or to accept an indicator that disappears on half the controls.

An indicator that works on some controls and not others is worse than a uniformly weak one: it
teaches a keyboard user that focus is sometimes nowhere, and there is no way to tell that state
apart from focus having been lost.

## What to decide

Do not reach for the first approach. Three are plausible and they differ in cost:

1. **A Sotto-drawn ring in a shared focus-aware wrapper.** Full opacity, our own width and offset,
   applied where controls are built. Most control, but every focusable control has to route through
   it, and `gpui-component`'s own ring will still draw underneath unless it is suppressed.
2. **A background or border change on the focused control** rather than an outside ring. Cheaper
   and it composes with ghost buttons, which are the failing case, but it competes with the
   selected state some controls already carry — check `Ask` while its panel is open before
   choosing this.
3. **Upstream.** `focus_ring` takes a `margins` argument and hardcodes the alpha; a patch making the
   alpha configurable is small. It is also not in our control on any useful timescale, so treat it
   as a follow-up rather than the plan.

Whichever is taken, record why the other two were not.

## Acceptance

- A focused control is unmistakable on **both** a solid and a ghost button, under **both**
  appearances. The ghost-on-dark case is the one that filed this task; if it is not demonstrably
  fixed, nothing here is.
- The indicator does not collide with a control's existing selected state — a focused unselected
  control and a selected unfocused one must not look the same.
- Focus movement, Enter/Space activation, and the tab order T100 established are unchanged, and its
  tests still pass untouched.
- A test asserts the indicator is applied to a focused control. `debug_bounds` cannot report colour,
  so assert the decision — the way `row_mark` is asserted in `transcript.rs` — and say plainly in
  the task that the token binding either side of that decision is not pixel-proven.
- Full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Focus *traversal* and activation — T100 owns those and they work. Screen-reader announcement, which
GPUI exposes no tree to assert against. Adding keyboard shortcuts. Restyling any control beyond its
focused appearance.

## Notes

Filed 2026-08-16 from the maintainer's screenshots of `Ask`, `Re-transcribe` and `Re-summarize`
under focus. T100 closes with this filed rather than staying open: what it set out to fix — focus
that could not move at all, and an Enter listener that could never fire — is fixed and pinned by a
test that fails when reverted. The visibility of the indicator is a separate problem with a
separate cause, now understood, and holding T100 open for it would also hold T076 and T092.
