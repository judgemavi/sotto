# Vendored icon provenance

- **Set:** Lucide (https://lucide.dev), from `lucide-icons/lucide` on GitHub, `main`.
- **Licence:** ISC. Full text in `LICENSE` beside these files; it must be vendored with them.
- **Fetched:** 2026-08-14, from `raw.githubusercontent.com/lucide-icons/lucide/main/icons/<name>.svg`.
- **Why Lucide:** `gpui_component::IconName` maps its variants to `icons/<name>.svg` following
  Lucide's names, so matching filenames lets the built-in names resolve with no glue.

Every file is a 24x24 `viewBox`, `fill="none"`, `stroke="currentColor"` outline icon. GPUI
rasterizes an SVG into an alpha mask and paints it with the element's text colour, so one asset
serves both themes and no call site sets a fill. Do not recolour these in the SVG.

Filenames are exactly as Lucide publishes them, so this record can be checked against the source.
Where `gpui_component` asks for a different path for the same picture, the *request* path is
aliased in `crates/app/src/workspace/icons.rs` rather than the file being renamed.

## What each is for

| File | Request path(s) | Used for |
|---|---|---|
| `trash-2.svg` | `icons/trash-2.svg`, `icons/delete.svg` | Delete, replacing the emoji-presentation U+1F5D1 bin. `IconName::Delete` |
| `x.svg` | `icons/x.svg`, `icons/close.svg` | Close the settings sheet, replacing `✕`. `IconName::Close` |
| `panel-left.svg` | `icons/panel-left.svg` | Collapse and restore the Library rail, from the toolbar |
| `circle-x.svg` | `icons/circle-x.svg` | The search field's clear button. `IconName::CircleX` |
| `house.svg` | `icons/house.svg` | The rail's Home entry, replacing `⌂` |
| `app-window.svg` | `icons/app-window.svg` | A captured application recording, replacing `▣` |
| `mic.svg` | `icons/mic.svg` | A microphone-only recording, replacing `●` |
| `file-input.svg` | `icons/file-input.svg` | An imported recording, replacing `⇥` |

## Deliberately not vendored

Nothing is kept here "in case". An icon with no call site is dead weight in a binary whose small
footprint is part of the product, and `Assets::load` answers an unvendored path with `Ok(None)`
rather than failing, so adding one later is a two-line change.

- `settings.svg`, `sun.svg`, `moon.svg`, `monitor.svg` were fetched for T082 and then not used:
  the controls they would have drawn are the ones T082 moved into the macOS menu bar, and an
  `NSMenuItem` cannot render an SVG.
- `square.svg` (Stop), `rotate-cw.svg` (Re-transcribe), `folder-open.svg` (Reveal),
  `arrow-down-to-line.svg` (Follow live) and `message-circle-question-mark.svg` (Ask) would all
  iconify domain verbs. GPUI 0.2.2 publishes no accessibility tree, so an icon-only control's name
  reaches a person only through a tooltip; those controls keep their words.
- The live recording dot is a painted circle, not a glyph, and needs no asset.
- `♪` / `♪♪` in `transcript.rs` mark non-speech audio inside prose, where a UI icon would read
  wrong. They are typography in a sentence, not a control.
