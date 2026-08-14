# T081 — Settings belongs in the window, and delete is a trash can

**Status:** in-review

**Wave:** N7 — v2 workspace

**Depends on:** T080 (`in-review`), which built the settings sheet.

**Owns:** `crates/app/src/main.rs` for the settings action, `crates/app/src/workspace/mod.rs`,
`crates/app/src/workspace/layout.rs`, `crates/app/src/workspace/library.rs`,
`crates/app/src/settings/mod.rs`, and this task

## 1. Settings opens a second OS window

`main.rs:41` handles `OpenSettings` by calling `cx.open_window`, so Settings is a real second
window — and T080 drew a modal sheet with a scrim inside it. The result is a scrim-backed dialog
floating in its own window over the workspace: two competing containers, one of them redundant.

The mock is unambiguous. Settings is `#setScrim` — an overlay **inside the app window**, over the
workspace it configures, dismissed by its close control or Escape.

Move it. `MeetingWorkspace` holds whether the sheet is open and renders it over the stage; the
`Sotto ▸ Settings…` menu item toggles that instead of opening a window. Keep the menu item — it is
the macOS convention and people reach for it — and add the gear the mock puts in the title bar
(`#settingsBtn`), so the setting is reachable without the menu bar.

While there: the mock's title bar also carries a theme toggle (`#themeBtn`, `◐`) and the Ask toggle
(`?`). Add them if they are honest — a theme toggle that cannot actually switch themes is a dead
control, so check what `gpui_component`'s theme support allows before drawing one.

## 2. Delete should be a trash can

The maintainer's call, and it is right for this one action: delete has a universal glyph where
"Re-transcribe" and "Summarize" have none. Use a trash icon for delete in the view bar and on each
retained recording row in Settings.

Non-negotiable, because this is a destructive action reduced to a picture:

- **Keep the two-click arm.** The first click arms and says what will be deleted; the second
  performs it. An icon makes the target less explicit, so the confirmation carries more weight, not
  less.
- **Every icon button keeps a tooltip and an accessible label.** An icon-only control still needs
  its words for a screen reader; you are hiding the label from sighted users, not removing it.
- **Name the target when armed.** "Delete this recording and its 157 MB?" beats a red glyph.

Do not iconify anything else in this pass. Icons suit chrome — open, close, toggle, collapse — and
destructive actions with a settled convention. They do not suit domain verbs; the mock agrees,
using text for `Summarize`, `Import…`, `New recording`, `Reveal recording` and reserving glyphs for
Ask, Settings, theme, collapse and close.

## 3. Shorter labels where the mock is shorter

The view bar carries `Re-transcribe`, `Reveal recording`, `Delete…` as three full-width text
buttons and reads as crowded. The mock says `Reveal`, not `Reveal recording`. Adopt the mock's
labels rather than inventing shorter ones.

## Acceptance

- Settings opens as an overlay in the app window, from both the menu item and a title-bar control,
  and closes by its control and by Escape. No second window is created.
- Every setting still reachable and still working; T080's six disclosure statements still pass their
  inventory regression.
- Delete is an icon in the view bar and on each recording row, retains its two-click arm, names its
  target when armed, and carries a tooltip and accessible label.
- No other control loses its text label.
- Nothing clips at the stated minimum width.
- Focused and full app tests, strict Clippy over all targets, formatting, and diff checks pass.

## Out of scope

New settings, the reasoning backend list (Codex only), and iconifying domain actions.

## Notes

### How Settings is mounted now

`SettingsView` no longer knows how it is mounted. It emits `SettingsEvent::Dismissed` instead of
calling `window.remove_window()`, and `MeetingWorkspace` owns whether it renders:

- The workspace holds `settings: Option<Entity<SettingsView>>` and `settings_open: bool`, and
  `layout.rs` renders the sheet as an absolutely positioned, `occlude()`d overlay covering the whole
  shell — the mock's `#setScrim`, over the workspace it configures.
- The sheet is built the **first** time it is asked for and kept afterwards. Building it probes the
  Codex CLI and starts the MCP poll, so cold launch must not build it (AGENTS' idle-launch rule) and
  each re-open must not start another poller. `SettingsView::reopen` restores what a fresh sheet
  would have had: Storage & privacy leading, a re-measured recording library, no armed delete, focus.
- `main.rs` opens exactly one window. Its `OpenSettings` handler now resolves that window's `Root`,
  downcasts to `MeetingWorkspace`, and calls `toggle_settings` — the same entry point the title bar's
  gear uses. The menu item stays; no `cx.open_window` for settings remains.
- Closing re-measures the library footprint and the open recording, because Settings can delete
  retained media, and releases the focus the sheet took.

### Title-bar controls: two added, one rejected

The shell had no always-present chrome row, and Settings must be reachable from Home as well as from
a running or stopped recording, so `render_title_bar` adds the mock's `.titlebar` above the state
bars: the wordmark, the mock's `quiet by default` subtitle, then the icon controls.

- **Settings (`⚙`) — added.** Toggles the same overlay the menu item does.
- **Theme (`◐`) — added, and it is honest.** `gpui_component::Theme` is a global that both
  `WorkspaceTokens::resolve` and every `gpui_component` control resolve against, so
  `Theme::change` really repaints the window in both directions. It would have been a dead control if
  it also forgot: `gpui_component::init` syncs to the system appearance at launch, so the choice is
  recorded in `workspace-state.json` alongside `ask_open` and re-applied before the first frame.
  `None` still means "follow the system".
- **Ask (`?`) — rejected as a duplicate, not as dishonest.** The shell already carries a working,
  *text-labelled* Ask toggle in the Ask rail. A second control for one state is the clutter this pass
  exists to reduce, and moving it would have meant leaving the collapsed rail an empty 42px strip
  whose reserved width is pinned by an existing regression. Reconsider when the Ask rail itself is
  redesigned.

### How the armed delete reads

Delete is `🗑` in the view bar and on every retained recording row in Settings. Both keep a two-click
arm — Settings **gained** one, since it deleted on a single click before — and armed, the control
stops being a picture: it says `Confirm delete` in words while the message names the target.

- View bar: `Delete “Meeting app — Sprint 41 planning” and its 412 MB? This removes the recording,
  its transcript and its notes from this Mac. Click Confirm delete to go ahead.` With no retained
  media it says so rather than naming a size.
- Settings row: `Delete the recording for “Sprint 41 planning” and its 412.0 MB? Its transcript and
  notes stay on this Mac. Click Confirm delete to go ahead.` A still-growing recording says its final
  size is not known yet instead of inventing one. Changing pane disarms.

Every icon button is built through `workspace::icon_button` / `workspace::delete_icon_button`, which
take the accessible label as a required argument and wire it to the tooltip. **Be precise about the
limit:** GPUI 0.2.2 publishes no platform accessibility tree — no ARIA, no `AXTitle`, no AccessKit
bridge — so no screen reader can read that name, and an invisible label would have been a dead
control written as text. The tooltip is the whole of what the framework offers, which is why icons
stay confined to chrome and to the one destructive action that restores its words when armed. If
accessibility becomes a gate, it is a framework-level task, not a per-button one.

Nothing else was iconified. `Re-transcribe`, `Summarize`, `Import…`, `New recording` and `Reveal`
keep their text. `Reveal recording` shortened to the mock's `Reveal`, with `Show this recording in
Finder` as its tooltip; no abbreviation was invented.

### Verification

- `cargo test -p app` — 197 pass (was 191). `cargo test --workspace` green.
- T080's two regressions still pass unchanged: `privacy_disclosure_inventory_is_complete` (exact
  six-string inventory) and `every_pinned_disclosure_still_renders_on_a_pane` (mounted). A new
  workspace test additionally proves two of them lay out inside the workspace window after the move.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean;
  `cargo fmt --all --check` clean; scoped `git diff --check` clean.
- Launch-and-render: `cargo run -p app` launched and stayed alive with a rendered window, twice.

### NOT RUN

- **Visual acceptance of the built app.** No screenshot was taken — `screencapture` is denied to this
  shell — so the gear, the `◐` toggle, the `🗑` glyph and the in-window scrim have not been *seen*
  rendered; they are proven mounted and in-bounds by `debug_bounds` only. The maintainer's eye is
  still the gate that opened this task.
- **The trash glyph's rendering.** `🗑` (U+1F5D1) relies on CoreText emoji fallback. It is not
  pinned by any test and has not been observed on screen; SVG icons were not an option because the
  app registers no asset source and `crates/app/Cargo.toml` is not owned here.
- **Menu-item dispatch through the real macOS menu bar.** The test drives `toggle_settings` and the
  gear; the `Sotto ▸ Settings…` path through `cx.set_menus` → `App::dispatch_action` →
  `WindowHandle::update` was not exercised by an automated test.
- **Theme choice surviving a real relaunch.** The round trip through `workspace-state.json` is
  tested; quitting and relaunching the signed app to confirm the palette is restored was not.
