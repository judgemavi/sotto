//! The icons Sotto owns, and the [`AssetSource`] that serves them.
//!
//! Before T082 the shell drew its controls with Unicode symbols and registered **no** asset source
//! at all, so `gpui_component::IconName` — whose variants already name `icons/*.svg` — could never
//! resolve anything. That combination is what put a full-colour emoji bin (U+1F5D1) next to
//! monochrome typographic symbols: the app was asking CoreText to choose a picture, and CoreText
//! chose one from a different set.
//!
//! The fix is to own the pictures. Every file under `crates/app/assets/icons/` is Lucide (ISC),
//! vendored with its licence and `PROVENANCE.md`; each is a 24×24 `fill="none"`,
//! `stroke="currentColor"` outline. GPUI rasterizes an SVG into an **alpha mask** and paints it
//! with the element's text colour, so one asset serves the light and the dark palette and no call
//! site has to pick a fill.
//!
//! The bytes are embedded with `include_bytes!` rather than read from disk. Sotto ships as one
//! native binary; an asset source that stat'd a directory would work in the repo and fail in every
//! place the product is actually installed.
//!
//! This module lives under `workspace` because that is where its call sites are. Nothing about it
//! is workspace-specific: `main.rs` hands [`Assets`] to `Application::new().with_assets`, which is
//! what makes both these paths and `gpui_component`'s own icon names resolvable process-wide.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};
use gpui_component::Icon;

/// A captured application, window, or display. Replaces `▣`.
pub(crate) const CAPTURED: &str = "icons/app-window.svg";
/// A microphone-only recording. Replaces `●`.
pub(crate) const MICROPHONE: &str = "icons/mic.svg";
/// A file that became a session rather than a capture Sotto performed. Replaces `⇥`.
pub(crate) const IMPORT: &str = "icons/file-input.svg";
/// The rail's Home entry. Replaces `⌂`.
pub(crate) const HOME: &str = "icons/house.svg";

/// Every path [`Assets`] can answer, and the vendored file that answers it.
///
/// Two entries are aliases. `gpui_component::IconName::Delete` and `::Close` ask for
/// `icons/delete.svg` and `icons/close.svg`, while the upstream Lucide files that draw them are
/// named `trash-2.svg` and `x.svg`. Keeping the vendored filenames exactly as Lucide publishes them
/// is what makes the provenance record checkable against the source, so the *request* path is
/// aliased here instead of the file being renamed.
const ASSETS: &[(&str, &[u8])] = &[
    (
        CAPTURED,
        include_bytes!("../../assets/icons/app-window.svg"),
    ),
    (IMPORT, include_bytes!("../../assets/icons/file-input.svg")),
    (HOME, include_bytes!("../../assets/icons/house.svg")),
    (MICROPHONE, include_bytes!("../../assets/icons/mic.svg")),
    (
        "icons/trash-2.svg",
        include_bytes!("../../assets/icons/trash-2.svg"),
    ),
    ("icons/x.svg", include_bytes!("../../assets/icons/x.svg")),
    (
        "icons/panel-left.svg",
        include_bytes!("../../assets/icons/panel-left.svg"),
    ),
    (
        "icons/circle-x.svg",
        include_bytes!("../../assets/icons/circle-x.svg"),
    ),
    // `IconName`'s own names, served by the Lucide file that draws them.
    (
        "icons/delete.svg",
        include_bytes!("../../assets/icons/trash-2.svg"),
    ),
    (
        "icons/close.svg",
        include_bytes!("../../assets/icons/x.svg"),
    ),
];

/// Sotto's vendored icons, registered once at startup.
#[derive(Clone, Copy, Debug)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        // A miss is `Ok(None)`, never an error: `gpui_component` widgets ask for icons from the
        // full upstream set, and Sotto deliberately vendors only the ones it draws.
        Ok(ASSETS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ASSETS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::new_static(name))
            .collect())
    }
}

/// A content marker drawn from a vendored asset, inheriting the ambient text colour and size.
pub(crate) fn marker(path: &'static str) -> Icon {
    Icon::empty().path(path)
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource as _;

    use super::{ASSETS, Assets};

    /// The defect this module exists to end was a picture the app did not own. Prove every path it
    /// advertises really resolves to bytes that are compiled in.
    #[test]
    fn every_advertised_icon_resolves_to_a_vendored_svg() {
        for (path, _) in ASSETS {
            let loaded = Assets.load(path).ok().flatten().unwrap_or_default();
            assert!(
                !loaded.is_empty(),
                "{path} must resolve to a vendored asset"
            );
            let text = String::from_utf8_lossy(&loaded);
            assert!(
                text.contains("<svg"),
                "{path} must be an SVG, not whatever else was on the disk"
            );
            assert!(
                text.contains("stroke=\"currentColor\"") && text.contains("fill=\"none\""),
                "{path} must be a stroked outline so one asset serves both themes"
            );
            assert!(
                !text.contains("stroke=\"#") && !text.contains("fill=\"#"),
                "{path} must not carry a baked colour"
            );
        }
    }

    /// Rasterizes every vendored icon through the same library GPUI uses, and asserts it draws.
    ///
    /// The structural test above would pass over a truncated or empty-`<g>` SVG, and the app would
    /// then paint an invisible control that nobody notices until a user reports a blank button.
    /// GPUI turns an SVG into an **alpha mask** and tints it with the element's text colour, so
    /// what actually matters is that a real render produces non-zero coverage — and that it comes
    /// from the stroke rather than a fill, which is what makes one asset serve both palettes.
    #[test]
    fn every_vendored_icon_rasterizes_to_visible_ink() -> Result<(), Box<dyn std::error::Error>> {
        // 32px is twice GPUI's 16px default icon box; it renders at 2× for quality.
        const EDGE: u32 = 32;

        for (path, _) in ASSETS {
            let bytes = Assets.load(path).ok().flatten().unwrap_or_default();
            let tree = resvg::usvg::Tree::from_data(&bytes, &resvg::usvg::Options::default())?;
            let scale = EDGE as f32 / tree.size().width();
            let mut pixmap = resvg::tiny_skia::Pixmap::new(EDGE, EDGE)
                .ok_or_else(|| std::io::Error::other("a 32x32 pixmap must allocate"))?;
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::from_scale(scale, scale),
                &mut pixmap.as_mut(),
            );

            let inked = pixmap
                .pixels()
                .iter()
                .filter(|pixel| pixel.alpha() > 0)
                .count();
            let total = (EDGE * EDGE) as usize;
            assert!(
                inked > total / 50,
                "{path} rendered {inked}/{total} inked pixels — an icon that draws (almost) \
                 nothing is worse than a glyph, because nothing looks like nothing"
            );
            assert!(
                inked < total * 4 / 5,
                "{path} rendered {inked}/{total} inked pixels — a near-solid block means the \
                 outline became a fill, which would ignore the theme colour it is tinted with"
            );
        }
        Ok(())
    }

    /// `IconName::Delete` and `::Close` are the built-in names T082 chose to resolve rather than
    /// invent a parallel set. If these aliases are dropped the trash and the sheet's close control
    /// silently render nothing, which is exactly the failure that is easy to miss by eye.
    #[test]
    fn gpui_components_built_in_names_resolve() {
        for path in ["icons/delete.svg", "icons/close.svg"] {
            assert!(
                Assets.load(path).is_ok_and(|bytes| bytes.is_some()),
                "{path} is a gpui_component IconName path and must resolve"
            );
        }
    }

    #[test]
    fn unvendored_icons_are_a_miss_rather_than_an_error() {
        assert!(
            Assets
                .load("icons/definitely-not-vendored.svg")
                .is_ok_and(|bytes| bytes.is_none()),
            "a widget asking for an icon Sotto does not ship must not fail the frame"
        );
    }

    #[test]
    fn listing_is_scoped_to_the_requested_prefix() {
        assert_eq!(
            Assets.list("icons/").map(|paths| paths.len()).unwrap_or(0),
            ASSETS.len(),
            "every vendored asset lives under icons/"
        );
        assert!(
            Assets.list("fonts/").is_ok_and(|paths| paths.is_empty()),
            "listing a directory Sotto does not ship must be empty, not everything"
        );
    }
}
