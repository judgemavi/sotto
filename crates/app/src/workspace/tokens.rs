//! Theme-resolved workspace design tokens ported from the normative HTML mock.
//!
//! Every value below is defined in *both* branches of [`WorkspaceTokens::resolve`], so no colour
//! exists in only one theme. The field names follow `docs/design/workspace-v2-mock.html`'s `:root`
//! custom properties, with two carry-overs the mock dropped but the columns still use:
//!
//! - `surface_2` is the mock's `--hover`: the raised/hovered variant of `surface`.
//! - `ink_2` has no mock equivalent and stays as the mid-weight body ink.
//! - `accent_on` and `live_on` are the inks that sit *on* those fills. The mock never needs them
//!   because CSS inherits a readable colour; GPUI does not, so a filled control must be told.
//!
//! Repointed to the mock's neutral-grey and indigo palette on 2026-08-14, replacing the earlier
//! teal scheme. Both branches moved together: a palette changed in one theme only is how a
//! product ends up rendering one theme's text on the other theme's ground.

use gpui::{App, Pixels, Rgba, px, rgb, rgba};
use gpui_component::ActiveTheme as _;

#[derive(Clone, Copy)]
#[expect(
    dead_code,
    reason = "complete normative palette remains centralized for gradual component adoption"
)]
pub(crate) struct WorkspaceTokens {
    pub(crate) ground: Rgba,
    pub(crate) surface: Rgba,
    pub(crate) surface_2: Rgba,
    pub(crate) sunken: Rgba,
    pub(crate) line: Rgba,
    pub(crate) line_soft: Rgba,
    pub(crate) ink: Rgba,
    pub(crate) ink_2: Rgba,
    pub(crate) muted: Rgba,
    pub(crate) faint: Rgba,
    pub(crate) accent: Rgba,
    pub(crate) accent_ink: Rgba,
    pub(crate) accent_on: Rgba,
    pub(crate) accent_wash: Rgba,
    pub(crate) accent_line: Rgba,
    pub(crate) live: Rgba,
    pub(crate) live_ink: Rgba,
    pub(crate) live_on: Rgba,
    pub(crate) live_wash: Rgba,
    pub(crate) live_line: Rgba,
    pub(crate) warn: Rgba,
    pub(crate) warn_wash: Rgba,
    pub(crate) scrim: Rgba,
}

impl WorkspaceTokens {
    pub(crate) fn resolve(cx: &App) -> Self {
        if cx.theme().is_dark() {
            Self {
                ground: rgb(0x101014),
                surface: rgb(0x17171c),
                surface_2: rgb(0x1d1d24),
                sunken: rgb(0x0b0b0e),
                line: rgb(0x2a2a33),
                line_soft: rgb(0x222229),
                ink: rgb(0xececf1),
                ink_2: rgb(0xc6c6d0),
                muted: rgb(0x9494a1),
                faint: rgb(0x62626e),
                accent: rgb(0x8a8fff),
                accent_ink: rgb(0xa5a8ff),
                accent_on: rgb(0x0f0f1a),
                accent_wash: rgb(0x1e1e38),
                accent_line: rgb(0x3c3d78),
                live: rgb(0xf2685c),
                live_ink: rgb(0xf8837a),
                live_on: rgb(0x2a0f0c),
                live_wash: rgb(0x351d1b),
                live_line: rgb(0x63302a),
                warn: rgb(0xd9a24a),
                warn_wash: rgb(0x2e2617),
                scrim: rgba(0x00000099),
            }
        } else {
            Self {
                ground: rgb(0xf4f4f6),
                surface: rgb(0xffffff),
                surface_2: rgb(0xf0f0f3),
                sunken: rgb(0xececef),
                line: rgb(0xe0e0e6),
                line_soft: rgb(0xebebf0),
                ink: rgb(0x1a1a1f),
                ink_2: rgb(0x3c3c45),
                muted: rgb(0x62626d),
                faint: rgb(0x9a9aa5),
                accent: rgb(0x5558d9),
                accent_ink: rgb(0x4649c9),
                accent_on: rgb(0xffffff),
                accent_wash: rgb(0xececfd),
                accent_line: rgb(0xc9caf5),
                live: rgb(0xdc4a3f),
                live_ink: rgb(0xc23d33),
                live_on: rgb(0xffffff),
                live_wash: rgb(0xfdecea),
                live_line: rgb(0xf2c4bf),
                warn: rgb(0x966410),
                warn_wash: rgb(0xf7eeda),
                scrim: rgba(0x14141a73),
            }
        }
    }
}

pub(crate) struct TypeScale;

impl TypeScale {
    /// Eyebrows, chips, timecodes and other measured metadata.
    pub(crate) const META: Pixels = px(10.5);
    /// Scope chips and the capture bar's recording kind.
    pub(crate) const CHIP: Pixels = px(11.0);
    /// Buttons and tab labels.
    pub(crate) const CONTROL: Pixels = px(12.5);
    pub(crate) const BODY: Pixels = px(13.0);
    /// The open session's title in the view bar.
    pub(crate) const TITLE: Pixels = px(14.0);
    pub(crate) const CLOCK: Pixels = px(17.0);
}

pub(crate) struct Space;

impl Space {
    pub(crate) const XS: Pixels = px(4.0);
    pub(crate) const SM: Pixels = px(8.0);
    pub(crate) const MD: Pixels = px(12.0);
    pub(crate) const LG: Pixels = px(16.0);
}
