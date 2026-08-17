# T101 — A focus indicator you can actually see

**Status:** in-review

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

## Implementation — 2026-08-16

The shared modifier lives at `crates/app/src/workspace/focus.rs`. That home was named before it was
written: focus presentation belongs to the shipped workspace shell rather than to one control or
to the colour-token file. Settings is an overlay inside that shell, so its buttons route through
the same constructor too. No open lane held the new file or the import-only call sites when work
started. Historical Board and dev-window controls are not part of the shipped product shell and
remain untouched.

The first review rejected a two-tone box shadow. GPUI paints a shadow as a filled rounded rectangle
behind the control. A solid fill hid the interior and made `Record again` look correct, but a ghost
fill exposed the rectangle and swallowed the labels on Ask and a library row. An
`overflow_hidden` ancestor clipped the protruding part around `Re-transcribe`, leaving only corner
brackets. The automated assertion described the two opaque shadows, so it proved the broken
mechanism rather than a visible indicator. That implementation and its one-outcome
`focus_indicator(true)` seam have been removed.

The replacement is one opaque 2 px border painted on the button itself. GPUI paints borders after
the control's content and within its own bounds: there is no filled shape behind a transparent
ghost label, and no outside halo for a clipping parent to eat. The colour is appearance-resolved at
construction — pure black in light mode and pure white in dark mode, the maximum contrast in each
case. Once the ring is opaque, the second contrasting edge is unnecessary and would spend more of
these compact controls' interior. The shared constructor clips overflow at the button's own bounds,
which suppresses `gpui-component`'s superseded outside halo without relying on whatever clipping
an ancestor happens to apply.

Focus does not replace or alter the background. Ask's selected/open state therefore retains its
selected fill whether focused or not, while focus is expressed only by the inset border. The
existing `gpui-component` button remains the element that owns focus, the tab stop, activation, and
selected fill; the shared constructor only attaches the focused style. T100's tab order and key
behavior do not change, and its tests were not edited.

Why not the other approaches:

- A focused background replacement remains rejected because Ask already uses its fill to say the
  panel is selected/open. The inset border is a separate channel and leaves that fill untouched.
- An upstream configurable-alpha patch remains a reasonable follow-up, but it would not put a fix
  in the shipped app on this task's timescale and would tie acceptance to a dependency release.
- The reviewed two-tone shadow was rejected because a shadow is a filled shape behind the control,
  not a ring. A single appearance-resolved border is enough once the alpha ceiling is removed.

The regression asserts the production focused style has all four 2 px border edges, the supplied
opaque colour, no box shadow, and no background override. It would fail if the reviewed
paint-behind mechanism returned or if focus started replacing Ask's selected fill. GPUI exposes no
pixel assertion for this treatment, so this is **not pixel proof** and does not certify appearance.
Real built-app confirmation of solid and ghost controls in light and dark appearances remains
maintainer-owned and **NOT RUN** by the implementation lane. This task stays `in-review`.

Corrected automated gates, 2026-08-16: focused inset-border regression passed; T100's unchanged
Root-mounted focus and Enter activation tests passed; the locked full workspace suite passed
(including 267 app tests, four ignored real-media tests, and all four MCP HTTP tests); strict
workspace Clippy over all targets/features, formatting, and diff checks passed.
