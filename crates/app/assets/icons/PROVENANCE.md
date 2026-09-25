# Icon provenance

Product chrome icons come from **Longbridge `gpui-kit-assets`** (Lucide, ISC), registered at
boot via `gpui_kit::assets::Assets` (ADR-0025). Sotto no longer vendors a parallel Lucide set
under this directory.

Historical note: before ADR-0025 this folder held Lucide SVGs (`trash-2`, `x`, `panel-left`,
`circle-x`, `house`, `app-window`, `mic`, `file-input`) with ISC `LICENSE` and path aliases for
`IconName::Delete` / `Close`. Those files were removed when the kit catalog became the sole
source for standard actions and markers.
