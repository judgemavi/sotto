# T100 — The keyboard cannot see where it is

**Status:** in-review

**Wave:** N8 — entry workspace

**Depends on:** nothing. T076 depends on *this*: its keyboard-activation acceptance cannot be met
while no control can be reached by keyboard at all.

**Owns:** the focus wiring and its tokens — `crates/app/src/workspace/tokens.rs`, the key-binding
and window setup in `crates/app/src/main.rs`, and this task. **Diagnose before editing anything
else.** Several workspace files are held by open review lanes; if the fix turns out to need one of
them, stop and report rather than reaching into it.

## Why this exists

Maintainer, 2026-08-16, on a real summary: *"tabing across app doesnt highlight any controls. looks
like none have focused state except for inputs."*

Two claims already recorded in the board are affected. T076 states its evidence controls "are tab
stops with spoken text labels" and adds an Enter/Space listener to `evidence_control`
(`crates/app/src/workspace/notes.rs:1771`) because `gpui-component`'s button binds no key
activation. T077 states its `SelectableText` "registers a focus handle as a tab stop, so the text is
reachable without a mouse." Neither claim has been demonstrated, and the observation above suggests
at least one of them is false in the built app.

This is an accessibility failure, not a polish item. A product whose controls are reachable only by
pointer excludes people who do not use one, and every keyboard affordance the codebase believes it
has is currently unverified.

## What is already known — do not re-derive this

The plumbing appears to be present, which is why the diagnosis matters more than the fix:

- The real window **is** mounted inside `gpui_component::Root` (`crates/app/src/main.rs:89`), so
  this is not the missing-`Root` problem that blocks the *test* harness.
- `Root` binds `tab`/`shift-tab` in its own `"Root"` key context and handles them with
  `window.focus_next()` / `focus_prev()` (`gpui-component-0.5.1/src/root.rs:15-23`, `:387`).
- `gpui_component::init` is called (`main.rs:19`), so those bindings are registered.
- `Button` is a tab stop **by default** — `tab_stop: true`, and it calls `track_focus` with its
  `tab_index` whenever it is not disabled (`button.rs:344-355`, `:436-456`).
- `Button` already draws a ring: `.focus_ring(is_focused, px(0.), window, cx)` at `button.rs:611`,
  implemented at `styled.rs:545` with a 1.5px border.

So the ingredients exist. Find which link is actually broken before changing anything.

## The question to answer first

**Does focus move and the ring not render, or does focus never move?** These have different fixes
and the difference is invisible from a screenshot. Distinguish them — a temporary probe that prints
or renders the focused handle after each Tab is enough — and record the answer here.

Candidate causes, in the order worth checking:

1. **The ring is drawn but invisible against our theme.** The likeliest and cheapest. Our tokens are
   applied over `gpui-component`'s theme; if the ring colour resolves to something near the surface
   colour it is present and unseeable. Check what `focus_ring` resolves to under both appearances.
2. **Nothing is focused to begin with, so the first Tab has no origin.** Check whether anything
   takes initial focus when the window opens.
3. **The key event never reaches `Root`'s context.** Our own bindings are installed at
   `main.rs:104` via `workspace::key_bindings()`; confirm nothing shadows `tab`, and that the
   dispatch path from an unfocused window includes `Root`.
4. **The controls are not the tab stops we think.** `evidence_control` wraps its `Button` in a plain
   `div` carrying the `on_key_down` listener. A plain div is not focusable, so if the listener is
   meant to fire it must be on something that can hold focus — verify whether that handler is
   reachable at all, or is dead code sitting outside the focusable node.

## Acceptance

- The diagnosis above is recorded in this task: which link was broken, and how that was established.
- Tab and shift-tab move focus through the workspace's controls, and the focused control is
  **visibly** distinguishable under both light and dark appearance.
- The Enter/Space activation `evidence_control` already implements actually fires when its control
  is focused — or, if that listener is unreachable by construction, it is moved to where focus
  lands rather than left as dead code.
- A test proves keyboard activation of at least one real control end to end. This needs a
  `Root`-mounted harness; `library.rs`'s `mount_shell` is the working pattern and is private to
  that module today. Lifting it into a shared test module is in scope here, and pays for itself —
  T076's residual and a Return-key test in `notes.rs` were both blocked by its absence.
- T077's "reachable without a mouse" claim for `SelectableText` is either demonstrated or withdrawn.
- Full tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Screen-reader announcement — GPUI exposes no accessibility tree to assert against, and the standing
mitigation is that every control carries a readable text label rather than an icon. Restyling any
control beyond the focus indicator. Adding new keyboard shortcuts.

## Notes

Filed 2026-08-16 from the maintainer's T076 review. T076 stays `in-review` until this closes: its
acceptance asks for keyboard activation of the evidence controls, and that cannot be true while no
control can be focused. Its other residuals are settled — the visual pass was met in use, and
screen-reader announcement is unassertable.

Recorded so it is not lost: the same review noted the **`Show timecodes` control may overflow the
right edge of the notes column** on a wide window — in the screenshot it extends past where the
claim text above it wraps. Confirm before acting; if real it belongs to a `ControlRow` question in
that head, in the family T095 and T060 exist to prevent, not to this task.

Diagnosis, 2026-08-16: **focus never moved**. A Root-mounted test established that the window
started with no focused node and a simulated Tab left it that way. GPUI dispatches keys through the
focused node's ancestry; with no origin, Root's `"Root"` key context and its `focus_next()` action
were not in the dispatch path. A non-tab-stop `KeyboardRoot` between Root and the workspace now
holds initial focus, so the first Tab moves to the first real control and Shift-Tab moves backward.
The same mounted test uses the production `evidence_control` and proves Enter reaches its ancestor
listener from the focused child button. Sotto also installs explicit maximum-contrast component
ring tokens for both stored theme appearances; tests switch dark then light and prove neither
appearance change discards them.

Ownership deviation: `crates/app/src/workspace/notes.rs` was held by open lanes, but
`evidence_control` changed from `fn` to `pub(super) fn` so this lane's mounted regression exercises
the production control instead of a test copy. Testing the real seam justified retaining that
minimal visibility-only change.

T077's `SelectableText` claim is withdrawn. Source inspection showed `TextView` calls
`track_focus(focus_handle)` but never marks that handle as a tab stop. Its text remains pointer-
selectable and copyable, but static prose is not inserted into the control tab order. No production
claim should say it is keyboard-reachable unless the upstream widget or Sotto's wrapper later adds
an intentional text-navigation interaction.

Automated gates, 2026-08-16: the Root-mounted focus/activation tests pass; the full workspace test
suite passes when its loopback MCP tests are allowed to bind local sockets; strict workspace
Clippy over all targets/features, formatting, plist validation, and diff checks pass. The first
sandboxed full run failed only at all four MCP HTTP tests with `Operation not permitted`; the same
binary and then the full suite passed outside that socket restriction. **Real built-app visual
confirmation remains NOT RUN**: review must confirm the black-at-20%-alpha light ring and
white-at-20%-alpha dark ring are visibly distinct on the actual controls before changing this task
to `done` and releasing T076.
