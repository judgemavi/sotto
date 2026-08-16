# T082 — Real icons, and chrome where macOS puts it

**Status:** done

**Wave:** N7 — v2 workspace

**Depends on:** T081 (`in-review`), which added the glyph buttons and moved Settings in-window.

**Owns:** `crates/app/assets/**` (new), `crates/app/Cargo.toml`, `crates/app/src/main.rs`,
`crates/app/src/workspace/**`, `crates/app/src/settings/mod.rs`, and this task

## 1. The menu item does not open Settings

`Sotto ▸ Settings…` does nothing. The wiring reads correctly — `OpenSettings` is registered with
`cx.on_action`, `App::is_action_available` consults global listeners so the item should be enabled,
the handler resolves the workspace window, downcasts `Root::view()` to `MeetingWorkspace`, and
`toggle_settings` ends in `cx.notify()`. Every step looks right on paper and the behaviour is still
wrong, so **this must be debugged against the running app, not by reading it again.**

Find where it actually stops: whether the action dispatches at all, whether
`workspace_window.update` returns `Err`, whether the downcast fails, or whether the overlay renders
and is not visible. Say which, and fix that.

The title-bar gear works, so the difference is the dispatch path, not the sheet.

## 2. Real icons instead of Unicode glyphs

The trash button shipped as U+1F5D1 and CoreText painted a full-colour emoji bin among monochrome
symbols. A variation selector was applied as a workaround; the honest fix is icons the app owns.

`gpui_component::IconName` already names `Delete`, `Settings`, `Sun`, `Moon` and maps them to
`icons/*.svg`, but the crate ships no icons and this app registers **no `AssetSource` at all**, so
nothing can resolve. Add one:

1. Vendor SVGs under `crates/app/assets/icons/`, named to match `IconName`'s paths so the built-in
   names resolve rather than inventing a parallel set. Record where they came from and their
   licence — an unattributed icon set is a legal problem, not a styling one.
2. Implement `AssetSource` over that directory and register it with `Application::new().with_assets`.
3. Replace the Unicode glyphs — trash, settings, theme, close, collapse — with icons that inherit
   `text_color` so both themes work from one asset.
4. Keep the rail's semantic glyphs (`▣` captured, `●` microphone, `⇥` imported, `⌂` Home) as
   glyphs, or convert them too — but decide deliberately and say why. They are content markers, not
   controls.

This also settles a question the maintainer raised: with real icons, icon-only controls become
viable in more places. They still are not free — GPUI 0.2.2 publishes no accessibility tree, so a
hidden label is unreadable by any screen reader, and a tooltip is all a person gets. Keep text on
domain verbs.

## 3. Move app chrome to the menu bar

The app name, Settings, and the theme switch belong to macOS, not to a row inside the window.

- `Sotto` is already the app menu. Settings is already in it, and must work (part 1).
- Add the theme switch as a menu item — `View ▸ Appearance`, or equivalent — reflecting the current
  choice, including "follow system".
- Then **remove the in-app title-bar row** T081 added, unless something in it cannot move. Say what
  remains and why. A window that duplicates the OS's own chrome wastes the vertical space this
  workspace needs.

Note that Settings must stay reachable while a recording runs, and that Stop's reachability rule is
unaffected: it lives on the capture bar, which is not this row.

## Acceptance

- `Sotto ▸ Settings…` opens the Settings overlay, with the cause of the previous failure recorded.
- Icons render from vendored SVGs, inherit theme colour, and no emoji-presentation codepoint remains
  in a control.
- Icon provenance and licence are recorded.
- Theme is switchable from the menu bar and reflects the current choice.
- The in-app title-bar row is gone, or what remains is justified.
- Settings is reachable during a recording; Stop is unaffected.
- Focused and full app tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

Iconifying domain verbs, new settings, and the workspace columns.

## Outcome

### 1. Why `Sotto ▸ Settings…` did nothing

**The action dispatched and the listener ran. `workspace_window.update` returned
`Err("window not found")`, and `let _ = …` threw that away.**

It is a re-entrancy failure, not a wiring failure, which is why the code read correctly:

1. macOS calls `handle_menu_item`, which calls `App::dispatch_action`.
2. There is an active window, so that routes into `Window::dispatch_action` →
   `Window::dispatch_action_on_node`.
3. `dispatch_action_on_node` runs inside `App::update_window_id`, which does
   `cx.windows.get_mut(id)?.take()?` — the window is **out of its slot** for the duration, which is
   how GPUI hands out `&mut Window` safely.
4. Global `cx.on_action` listeners run in that function's capture phase. The listener was reached.
5. It then called `WindowHandle::update` on *that same window*. `take()` on an already-taken slot
   yields `None`, so `update_window_id` returns `Err("window not found")`. The downcast, the entity
   update and `toggle_settings` were never executed.

The title-bar gear worked because a click dispatches from an element listener that is already
holding the window, so it never re-enters.

**Fix:** `workspace::in_workspace` defers the window update with `App::defer`. The effect queue is
flushed by the outermost `App::update`, after the window has been put back, so the handle resolves.
The error is no longer swallowed — a failure now prints its cause on stderr.

The same defect silently affected the appearance items added in part 3 and ⌘, which was added with
them; all four actions go through one helper.

### 2. Icons

Lucide, ISC. `crates/app/assets/icons/` carries the six SVGs actually drawn, the ISC `LICENSE`, and
`PROVENANCE.md` recording origin, fetch date, per-file purpose, and what was deliberately *not*
vendored. `workspace::icons::Assets` embeds them with `include_bytes!` and is registered with
`Application::new().with_assets`; `IconName::Delete` and `::Close` resolve through aliases so the
vendored filenames stay identical to upstream and remain checkable.

The rail's semantic markers were **converted**, not kept. They are content rather than controls, but
that is an argument about weight, not about ownership: `▣` and `⇥` have no settled meaning for
"captured application" and "imported file", and CoreText — not Sotto — chose their faces. Drawn at
the ambient text size and colour they still read as typography.

### 3. Chrome

The in-window title-bar row is **gone entirely**; nothing in it survived. The app name is now the
window's own title and the app menu; Settings is `Sotto ▸ Settings…` plus ⌘,; the theme switch is
`View ▸ Appearance` with Follow System / Light / Dark, marked with U+2713 because GPUI 0.2.2 cannot
set an `NSMenuItem`'s checked state. Follow System is live: a window appearance observer re-syncs
while no palette is recorded. Stop is untouched on the capture bar.

### Verification

Automated: `workspace::layout::tests::the_settings_menu_action_opens_the_overlay_over_this_window`
and its two siblings mount the shell exactly as `main.rs` does — under `gpui_component::Root`, with
`register_menu_actions`, with the window activated — and drive `App::dispatch_action`, the same
entry point GPUI's menu callback uses. Reverting the `defer` makes all three fail with the real
`window not found`. `workspace::icons::tests::every_vendored_icon_rasterizes_to_visible_ink` renders
each asset through resvg (GPUI's own rasterizer, a dev-dependency only) and asserts real ink.

Not automated, and needing a human: that the macOS menu bar itself invokes these actions; that the
icons look right in both palettes; that ⌘, works.
