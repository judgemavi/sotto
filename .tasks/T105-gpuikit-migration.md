# T105 — Migrate UI components to gpuikit

**Status:** ready

**Wave:** UI stack

**Depends on:** none (does not block product work; land behind ADR)

**Owns:** `crates/app/Cargo.toml`, `crates/app/src/main.rs`, `crates/app/src/workspace/**`,
`crates/app/src/settings/**`, new ADR under `docs/adr/`

## Why

Replace `gpui-component` with [gpuikit](https://crates.io/crates/gpuikit). GPUI stays — gpuikit
is a component layer on GPUI, not a framework swap. Forces `gpui = 0.2.2` →
`gpui-unofficial` 1.14 + `gpui_platform`.

## Plan

### 0. Spike (gate before ADR)
- Pin `gpui-unofficial` 1.14, `gpui-platform-gpui-unofficial` 1.14 (`font-kit`), `gpuikit` 0.9
- Boot: `Application::with_platform` + `gpuikit::assets()` + `gpuikit::init`
- Prove: window, transparent titlebar/traffic lights, theme, Button, Input, scroll
- Document gaps vs today's `Root` / Dialog / `dock::Panel`

### 1. ADR
- New ADR: adopt gpuikit; supersede ADR-0001/`gpui-component` pin and ADR-0003 version claim
- Exact pins; revert path = stay on `gpui-component` until cutover compiles

### 2. Strangler (order)
1. Boot + assets + theme/tokens/focus ring
2. Buttons + icons
3. Settings sheet
4. Library rail
5. Transcript + notes (`TextView` → gpuikit markdown/editor)
6. Dialogs (replace `Root` + `Dialog`)
7. Ask dock — **highest risk**; `dock::Panel` may become a custom collapsible region
8. Drop `gpui-component`; fix test mounts (`gpui_component::init` → `gpuikit::init`)

### 3. Done when
- `cargo test -p app` + clippy `-D warnings` green
- Manual: Home, open entry, edit note, Ask, settings, appearance, quit-while-recording
- No `gpui-component` in the tree

## Non-goals
- No changes to `core` / capture / providers / insight
- Do not dual-depend `gpui 0.2.2` and `gpui-unofficial` (duplicate types)
- No floating versions — pin exact

## Sticky risks
- Ask `dock::Panel` has no clear gpuikit peer → custom layout
- Pre-1.0 churn; pin hard
- Input/event and theme APIs will not map 1:1
