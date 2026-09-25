# ADR-0024: Adopt gpuikit on gpui-unofficial

- Status: Superseded by ADR-0025
- Date: 2026-09-21
- Decision owners: Sotto maintainers
- Evidence: T105 spike (`.tasks/T105-spike-notes.md`, local `crates/gpuikit-spike`)

## Context

ADR-0001 chose a pure-Rust shell on GPUI with `gpui-component`. ADR-0003 retained
crates.io `gpui = "=0.2.2"` and `gpui-component = "=0.5.1"` after the historical
board/overlay spike. The product shell has since moved to the top-down
transcript-and-notes workspace (ADR-0015 / ADR-0016); the component library is
what we edit daily, not the canvas primitives that ADR-0003 measured.

`gpui-component` and Nate Butler’s [gpuikit](https://crates.io/crates/gpuikit) are
both component toolkits *on* GPUI. They are not interchangeable drops: gpuikit
builds against the crates.io `gpui-unofficial` line (1.14+), not crates.io
`gpui 0.2.2`. Adopting it therefore changes both the component API and the GPUI
pin.

T105’s isolated spike (`crates/gpuikit-spike`) pinned:

```toml
gpui = { package = "gpui-unofficial", version = "=1.14.2" }
gpui_platform = { package = "gpui-platform-gpui-unofficial", version = "=1.14.2", features = ["font-kit"] }
gpuikit = "=0.9.0"
```

and proved: `Application::with_platform` + `gpuikit::assets()` + `gpuikit::init`,
transparent titlebar and traffic-light inset, theme toggle via `GlobalTheme`,
`button`, `input`/`InputState`, `scroll_area`, and `dialog`/`DialogState`. Product
`app` stayed on `gpui 0.2.2` + `gpui-component` for the spike.

Options considered:

1. **Stay on `gpui-component` 0.5.1 / GPUI 0.2.2.** Lowest churn; locks us to an
   older GPUI line and a library whose docking model we already use for Ask.
2. **Upgrade within Longbridge (`gpui-kit` / newer `gpui-component`).** Keeps
   `dock::Panel` familiarity but is a separate dependency decision and still a
   GPUI-line move.
3. **Adopt gpuikit + `gpui-unofficial` 1.14.2** (chosen). Matches the spike
   evidence; Dialog and Input peers exist; Ask has no `dock::Panel` peer and
   must use `sidebar` or a custom collapsible.

gpuikit is pre-1.0 and warns of breaking changes every release. That cost is
accepted under the same pin discipline ADR-0001 already required for GPUI.

## Decision

Sotto’s product UI migrates from `gpui-component` to **gpuikit**, and from
crates.io `gpui 0.2.2` to **`gpui-unofficial` 1.14.2** with matching
`gpui-platform-gpui-unofficial` (`font-kit`).

- **Pins are exact** (`=1.14.2`, `=0.9.0`). Upgrades of either line require a
  deliberate PR and an ADR note, same rule as ADR-0001.
- **Boot path** is `Application::with_platform(gpui_platform::current_platform(false))`,
  `.with_assets(...)` (gpuikit assets plus Sotto’s own), `gpuikit::init`, and
  `bind_input_keys` where inputs are used. No `gpui_component::Root`.
- **Dialogs** mount as sibling `DialogState` entities, not under a Root host.
- **Ask** does not depend on `dock::Panel`. Cutover uses gpuikit `sidebar` or a
  Sotto-owned collapsible region that preserves ADR-0017 / ADR-0020 behaviour
  (app-level Ask, collapsed by default, explicit open).
- **Theme and focus** re-home onto gpuikit’s `Theme` / `GlobalTheme` /
  `ActiveTheme`. Sotto’s focus-ring treatment (T101) is re-applied on the new
  button/input wrappers, not abandoned.
- **Icons:** keep Sotto’s vendored Lucide paths as the product asset source;
  do not require gpuikit’s Radix set for product chrome.
- **Cutover is a strangler**, ordered in T105: boot/theme → buttons/icons →
  settings → library → transcript/notes → dialogs → Ask → remove
  `gpui-component`. The product binary must not depend on both GPUI lines at
  once (duplicate types).
- **Revert path until cutover lands:** keep shipping `gpui 0.2.2` +
  `gpui-component`; the spike crate may remain as a compile check and is
  removable after `app` is green on gpuikit.
- **Headless core is unchanged.** `core`, capture, ASR, providers, insight,
  rag, and mcp stay GPUI-free.

This ADR does **not** authorize leaving GPUI for another UI framework, adopting
Electron/webview, or enabling gpuikit’s web/wasm platform in the shipped app.

## Consequences

- ADR-0001’s “GPUI + `gpui-component`” pairing is amended: the shell remains
  GPUI; the component library is gpuikit.
- ADR-0003’s retained `gpui = "=0.2.2"` pin is superseded for the product
  shell. Historical board/overlay measurements remain evidence about GPUI’s
  suitability, not a requirement to stay on 0.2.2.
- Every `gpui_component::*` call site in `crates/app` must be rewritten.
  Workspace tests that call `gpui_component::init` move to `gpuikit::init`.
- Ask’s Panel trait and any docking APIs disappear; behaviour is preserved by
  layout, not by Longbridge dock types.
- crates.io may resolve some `*-gpui-unofficial` helpers (macros, util) at a
  newer patch line than 1.14.2; `Cargo.lock` is the source of truth after
  cutover and must be reviewed for a single GPUI type universe.
- AGENTS.md and onboarding notes that name `gpui-component` or `gpui 0.2.2`
  update when the strangler completes, not before.

## Revisit if

- gpuikit or `gpui-unofficial` churn blocks a release or repeatedly breaks the
  shell after a routine pin bump.
- Ask cannot meet ADR-0017 / ADR-0020 with `sidebar` or a custom region at
  acceptable complexity.
- Profiling on the new pin violates Sotto’s CPU/RSS budgets beside a live
  meeting client.
- A maintained Longbridge or Zed-aligned component line becomes clearly better
  evidenced for our workspace (docking, editor, markdown) than gpuikit.

## Supersedes

Amends ADR-0001’s choice of `gpui-component` as the component library (GPUI as
the framework remains). Supersedes ADR-0003’s product pin of crates.io
`gpui = "=0.2.2"` / `gpui-component = "=0.5.1"`. Does not reopen ADR-0001’s
pure-Rust / headless-core boundary.
