# ADR-0025: Adopt Longbridge gpui-kit

- Status: Accepted
- Date: 2026-09-24
- Decision owners: Sotto maintainers
- Evidence: gpui-kit / gpui-component 0.6.6 crates.io APIs; prior ADR-0024 gpuikit spike

## Context

ADR-0024 moved the product shell from crates.io `gpui` 0.2.2 + `gpui-component` 0.5.1
onto Nate Butler’s `gpuikit` 0.9.0 and `gpui-unofficial` 1.14.2. That cutover proved a
newer GPUI line and stock Dialog/Input peers, but left Sotto painting a private palette
onto the toolkit theme, maintaining a custom `focus::Button` appearance wrapper, and
vendoring Lucide paths that the kit already ships.

Longbridge’s [`gpui-kit`](https://crates.io/crates/gpui-kit) 0.6.6 is a single facade
over matching `gpui-pre` / `gpui-component` / `gpui-kit-assets` crates. Applications are
expected to depend on `gpui-kit` alone (`use gpui_kit::*`), boot through
`gpui_kit::application()` + `gpui_kit::init`, and host windows under `Root` so dialogs,
sheets, tooltips, and selectable text copy work. Stock `Button`, `Checkbox`, `Input` /
`Textarea` / `Editor`, `TabBar`, and Lucide `IconName` cover the chrome Sotto was
re-implementing.

Options considered:

1. **Stay on gpuikit + gpui-unofficial (ADR-0024).** Lowest immediate churn; keeps a
   parallel palette and custom chrome wrappers; Ask/dialog Root model remains awkward.
2. **Adopt Longbridge gpui-kit 0.6.6** (chosen). One dependency pin, stock styling, Root
   dialogs, Textarea/Editor for notes Markdown source, TextView for reading mode.
3. **Revert to gpui-component 0.5.1 / GPUI 0.2.2.** Abandons the newer GPUI line without
   gaining kit assets or the 0.6 editor/textarea split.

## Decision

Sotto’s product UI depends on **`gpui-kit = "=0.6.6"`** (default features) and
**`test-support` under `[dev-dependencies]`**. The app crate does **not** list `gpui`,
`gpui_platform`, `gpuikit`, or `gpui-unofficial` directly.

- **Boot:** `gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(|cx| { gpui_kit::init(cx); … })`.
  Window content is `cx.new(|cx| Root::new(view, window, cx))`.
- **Styling:** fully adopt Longbridge theme tokens via `ActiveTheme` / `cx.theme()`.
  Remove Sotto hex palettes that painted toolkit `Theme`, custom button appearance
  wrappers that only reproduced the old look, and vendored Lucide icons for standard
  actions (`Delete`, `Close`, `PanelLeft`, `House`, `Mic`, `AppWindow`, `FileInput`, …).
  `WorkspaceTokens` / `TypeScale` are thin aliases onto `cx.theme()` colours and the kit
  typography ladder — not a product-owned palette or display face (no New York / custom
  scale). Highlights and capture surfaces use the kit's unchanged `accent` /
  `accent_foreground` pair. Warning labels use `foreground` on `background`, with
  warning colour reserved for indicators and borders; no custom white blends or
  lightness clamps are applied. Row hover uses `list_hover`, not
  `secondary_hover` (which can equal `secondary` in the default light theme). Stock
  `Progress`, `TabBar::segmented`, `Alert`, `AlertDialog`, and kit `Button` cover
  progress chrome, Notes/Transcript and recording tabs, settings claims, destructive
  confirms, Home actions, and rail navigation.
- **Icons:** register a composed `AppAssets` (`Assets` + `icon_assets!` extras) so
  product markers embed real SVG bytes; do not rely on `IconName::path()` alone.
- **Custom rendering** remains only for meeting-specific surfaces (transcript rows,
  notes sections, capture status), coloured through theme tokens — not a parallel palette.
- **Notes:** reading mode uses kit markdown / `TextViewState`; full-document Markdown
  source editing uses `Editor`/`EditorState` with `language("markdown")`, soft wrap, and
  the `tree-sitter-markdown` (+ inline) kit features.
- **Dialogs:** `window.open_dialog` / `open_alert_dialog` via `Root`; no sibling
  `DialogState` mounting.
- **Appearance:** Follow System / Light / Dark via `Theme::sync_system_appearance` and
  `Theme::change(ThemeMode::…)`. Always-visible scrollbars via `Theme::set_scrollbar_mode`
  when the product still needs them.
- **Dev profile:** workspace `[profile.dev.package]` opt-level=3 for `gpui-pre`,
  `gpui-component`, `gpui-kit`, `gpui-kit-assets`, `gpui-pre-macros`, `gpui-pre-platform`,
  `rustybuzz`, `taffy`, and `ttf-parser` per Longbridge install docs.
- **Spike cleanup:** remove `crates/gpuikit-spike` and obsolete gpuikit docs/imports.
- **Pins are exact.** Upgrades require a deliberate PR and an ADR note.

This ADR does **not** authorize Electron/webview, ambient capture, or reasoning in `core`.

## Consequences

- ADR-0024’s gpuikit + gpui-unofficial product pin is superseded.
- AGENTS.md names `gpui-kit = "=0.6.6"` as the UI stack.
- Workspace tests call `gpui_kit::init`; prefer `#[gpui_kit::test]` where it fits.
- Product IA (transcript + notes, session rail, Ask dock, citations, capture controls,
  save/cancel notes edits, citation-preserving diffs, `debug_selector` where tests need
  them) is preserved; chrome styling is the kit’s.

## Revisit if

- gpui-kit / gpui-pre churn blocks a release or repeatedly breaks the shell after a
  routine pin bump.
- Notes Markdown editing or selectable copy cannot meet product quality on the kit
  Textarea/Editor/TextView path.
- Profiling on the new pin violates Sotto’s CPU/RSS budgets beside a live meeting client.

## Supersedes

Supersedes ADR-0024 (gpuikit on gpui-unofficial). Amends ADR-0001’s component-library
choice again (GPUI remains; library is Longbridge gpui-kit). Does not reopen ADR-0001’s
pure-Rust / headless-core boundary.
