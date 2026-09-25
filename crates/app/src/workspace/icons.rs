//! Product icon names from Longbridge kit assets (Lucide).
//!
//! ADR-0025: standard actions use the kit catalog. Extra product markers (`House`, `AppWindow`,
//! `Mic`, `FileInput`) are embedded via [`crate::assets::AppAssets`] — default `Assets` alone
//! does not include them.

use gpui_kit::component::{Icon, Sizable as _, Size};
use gpui_kit::px;

/// Re-export the kit's Lucide catalog for chrome and markers.
pub(crate) use gpui_kit::assets::IconName;

/// A captured application, window, or display.
pub(crate) const CAPTURED: IconName = IconName::AppWindow;
/// A microphone-only recording.
pub(crate) const MICROPHONE: IconName = IconName::Mic;
/// A file that became a session rather than a capture Sotto performed.
pub(crate) const IMPORT: IconName = IconName::FileInput;
/// The rail's Home entry.
pub(crate) const HOME: IconName = IconName::House;

/// Standard chrome icons used throughout the shell (aliases onto the kit catalog).
#[expect(dead_code, reason = "alias retained for chrome call sites")]
pub(crate) type ChromeIcon = IconName;

/// A content marker drawn from the kit catalog, inheriting ambient text colour.
pub(crate) fn marker(name: IconName) -> Icon {
    Icon::new(name).with_size(Size::Size(px(14.0)))
}

#[cfg(test)]
mod tests {
    use super::{CAPTURED, HOME, IMPORT, IconName, MICROPHONE};

    #[test]
    fn product_markers_resolve_to_kit_variants() {
        assert_eq!(CAPTURED, IconName::AppWindow);
        assert_eq!(MICROPHONE, IconName::Mic);
        assert_eq!(IMPORT, IconName::FileInput);
        assert_eq!(HOME, IconName::House);
    }
}
