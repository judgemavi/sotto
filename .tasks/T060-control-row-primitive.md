# T060 — A control row that cannot clip what matters

**Status:** done

**Wave:** N6 — UI primitives

**Depends on:** nothing. T057 is `done`, which releases `crates/app/src/workspace/layout.rs`.

**Owns:** a new row primitive under `crates/app/src/workspace/`,
`crates/app/src/workspace/mod.rs` for its module registration,
`crates/app/src/workspace/layout.rs` and `crates/app/src/workspace/library.rs` for retrofit, and
this task

**Concurrency (planner, 2026-08-13):** T058 and T059 run alongside this. **Do not touch**
`crates/app/src/workspace/transcript.rs` — T059 holds it, and it is the one workspace file this
task must leave alone. `crates/app/src/session/**` belongs to T058. You hold `mod.rs`, so T059 will
route any module registration it needs through you rather than editing it.

## Why this exists

The same layout defect has now shipped three times:

1. Session-rail titles clipped at both ends with no ellipsis, because `gpui_component::Button`
   hardcodes `.flex_shrink_0().items_center().justify_center()` at `button.rs:460-462`. Deferred by
   maintainer decision after three attempts, and still open.
2. The Ask rail collapsing to a single character at narrow widths.
3. **Stop unreachable while recording** — the session bar overflowed, cutting `Pause` mid-word and
   pushing `Stop` off-screen entirely. AGENTS.md requires Stop to be one obvious action away while a
   session runs, so this one was a product failure rather than a cosmetic one.

Each was fixed, or attempted, by bounding widths at the site where it appeared. That has not worked,
because the real problem is that every control in these rows is equally unshrinkable and nothing
declares what must survive. Adding `min_w_0` and `overflow_hidden` per site treats one symptom and
leaves the next row to rediscover it.

## The idea

One primitive that makes shrink priority explicit and refuses to clip what is marked essential.
Three roles, named at the call site:

- **Essential** — never clipped, never compressed. Terminal controls and the elapsed clock.
- **Ellipsizing** — takes remaining space and truncates with a visible ellipsis. Titles, target
  names.
- **Expendable** — collapses or drops entirely when space runs out. Scope chips, secondary labels.

The primitive owns the `flex_shrink` and `min_w_0` mechanics so no call site has to know that
`gpui_component` controls default to unshrinkable, and it neutralises `Button`'s centred layout for
text that must ellipsize.

## Plan

1. Build the primitive with the three roles above. Where a `gpui_component` control's defaults fight
   the role — `flex_shrink_0`, `justify_center` — the primitive renders an interactive `div` instead
   of fighting the component.
2. Retrofit the session bar in `layout.rs`: Stop and the clock essential, target name ellipsizing,
   scope chips expendable.
3. Retrofit the session rail in `library.rs`. This is the fix for the deferred ellipsis defect
   recorded in T055; that item closes here.
4. Add a narrow-width test. Layout defects have been found only by a human at one window width all
   day, which is why the same bug shipped three times.

## Acceptance

- At any window width down to a stated minimum, Stop and the elapsed clock are fully visible and
  clickable while a session runs.
- Session-rail titles preserve their beginning and truncate with a visible ellipsis. The T055
  deferral closes.
- Scope chips collapse before anything essential is compressed.
- A test asserts the essential controls remain within bounds at the minimum width; it must fail if a
  future row marks nothing essential.
- Focused and full app tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Restyling the session bar or rail beyond what the roles require, the theme tokens, and pause
semantics.
