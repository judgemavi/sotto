//! Theme-resolved workspace design tokens for meeting-specific surfaces.
//!
//! Chrome uses stock Longbridge components and their theme. Custom transcript / notes / capture
//! rendering reads the same tokens through [`WorkspaceTokens::resolve`] so Sotto does not keep a
//! parallel hex palette (ADR-0025). Field names match older call sites; values come from
//! `cx.theme()`.

use gpui_kit::component::{ActiveTheme as _, Root, Theme, ThemeMode, scroll::ScrollbarMode};
use gpui_kit::{
    AnyView, App, Context, FocusHandle, Hsla, IntoElement, Pixels, Render, SharedString, Window,
    div, prelude::*, px,
};

/// Apply Light or Dark. Call after init and whenever the appearance menu picks a fixed mode.
pub(crate) fn apply_theme(mode: ThemeMode, window: Option<&mut Window>, cx: &mut App) {
    Theme::change(mode, window, cx);
    install_visible_scrollbars(cx);
}

/// Follow the OS appearance. `None` recorded choice at launch means this path.
pub(crate) fn sync_system_appearance(window: Option<&mut Window>, cx: &mut App) {
    Theme::sync_system_appearance(window, cx);
    install_visible_scrollbars(cx);
}

/// Overlay scrollbars stay visible so a long transcript or notes column can be judged at a glance.
///
/// Kit init may copy macOS "Show scroll bars: When scrolling", which fades the thumb after idle.
/// Appearance changes can re-sync that preference; call this after init and after theme changes.
pub(crate) fn install_visible_scrollbars(cx: &mut App) {
    Theme::set_scrollbar_mode(ScrollbarMode::Always, cx);
}

/// No-op retained for call sites that previously painted a Sotto focus ring into the theme.
///
/// Stock Longbridge focus rings are the product look now (ADR-0025).
pub(crate) fn install_component_focus_ring(_cx: &mut App) {}

/// Non-tab-stop focus origin so the first Tab reaches a real control under `Root`.
///
/// GPUI dispatches keys through the focused node's ancestry. With nothing focused, Tab never
/// reaches a listener even though the frame contains tab stops. This wrapper is focused when the
/// window is built and sits between `Root` and the workspace.
pub struct KeyboardRoot {
    focus_handle: FocusHandle,
    view: AnyView,
}

impl KeyboardRoot {
    #[must_use]
    pub fn new(view: impl Into<AnyView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::workspace::selectable::bind_copy_keys(cx);
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        Self {
            focus_handle,
            view: view.into(),
        }
    }

    #[must_use]
    pub fn view(&self) -> &AnyView {
        &self.view
    }
}

impl Render for KeyboardRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Kit Root stores dialog/sheet/notification state but does not paint those layers itself;
        // the window's content view must (gpui-kit overlays contract, ADR-0025).
        let dialogs = Root::render_dialog_layer(window, cx);
        let sheets = Root::render_sheet_layer(window, cx);
        let notifications = Root::render_notification_layer(window, cx);
        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .child(self.view.clone())
            .children(sheets)
            .children(dialogs)
            .children(notifications)
    }
}

/// Colours for meeting-specific surfaces, mapped from the active Longbridge theme.
///
/// Not a private palette: every field is a theme token (or a close semantic neighbour). Prefer
/// reading `cx.theme()` directly in new code; this struct exists so existing layout call sites
/// keep compiling while chrome moves onto stock components.
#[derive(Clone, Copy)]
#[expect(
    dead_code,
    reason = "complete token surface remains centralized for gradual call-site adoption"
)]
pub(crate) struct WorkspaceTokens {
    pub(crate) ground: Hsla,
    pub(crate) surface: Hsla,
    pub(crate) surface_2: Hsla,
    pub(crate) sunken: Hsla,
    pub(crate) line: Hsla,
    pub(crate) line_soft: Hsla,
    pub(crate) ink: Hsla,
    pub(crate) ink_2: Hsla,
    pub(crate) muted: Hsla,
    pub(crate) faint: Hsla,
    /// Kit foreground paired with the accent surface used by highlights and the capture bar.
    pub(crate) ink_on_wash: Hsla,
    /// Keep small highlight metadata on the same accessible foreground as body text.
    pub(crate) muted_on_wash: Hsla,
    pub(crate) focus: Hsla,
    pub(crate) accent: Hsla,
    pub(crate) accent_ink: Hsla,
    pub(crate) accent_on: Hsla,
    pub(crate) accent_wash: Hsla,
    pub(crate) accent_line: Hsla,
    pub(crate) live: Hsla,
    pub(crate) live_ink: Hsla,
    pub(crate) live_on: Hsla,
    pub(crate) live_wash: Hsla,
    pub(crate) live_line: Hsla,
    pub(crate) warn: Hsla,
    pub(crate) warn_line: Hsla,
    pub(crate) warn_wash: Hsla,
    pub(crate) scrim: Hsla,
    /// Kit typography ladder (ADR-0025) — meeting surfaces read these instead of a private scale.
    pub(crate) text_meta: Pixels,
    pub(crate) text_chip: Pixels,
    pub(crate) text_control: Pixels,
    pub(crate) text_body: Pixels,
    pub(crate) text_lede: Pixels,
    pub(crate) text_title: Pixels,
    pub(crate) text_clock: Pixels,
    pub(crate) text_display: Pixels,
}

impl WorkspaceTokens {
    pub(crate) fn resolve(cx: &App) -> Self {
        let theme = cx.theme();
        let typo = theme.typography_tokens();
        // Use kit surface/foreground pairs unchanged in both appearances. Status colour belongs
        // on indicators and borders, not on small labels or a privately generated light wash.
        Self {
            ground: theme.background,
            surface: theme.secondary,
            surface_2: theme.list_hover,
            sunken: theme.input,
            line: theme.border,
            line_soft: theme.border,
            ink: theme.foreground,
            ink_2: theme.muted_foreground,
            muted: theme.muted_foreground,
            faint: theme.muted_foreground,
            ink_on_wash: theme.accent_foreground,
            muted_on_wash: theme.accent_foreground,
            focus: theme.ring,
            accent: theme.accent,
            accent_ink: theme.foreground,
            // Text on a solid accent fill.
            accent_on: theme.accent_foreground,
            accent_wash: theme.accent,
            accent_line: theme.border,
            live: theme.danger,
            live_ink: theme.accent_foreground,
            // Text on a solid live/danger fill.
            live_on: theme.danger_foreground,
            live_wash: theme.accent,
            live_line: theme.danger,
            warn: theme.foreground,
            warn_line: theme.warning,
            warn_wash: theme.background,
            scrim: theme.overlay,
            text_meta: typo.xs.size,
            text_chip: typo.sm.size,
            text_control: theme.font_size,
            text_body: typo.md.size,
            text_lede: typo.lg.size,
            text_title: typo.lg.size,
            text_clock: typo.xl.size,
            text_display: typo.xl.size,
        }
    }

    /// Body and metadata use the kit foreground paired with the highlight surface.
    pub(crate) fn for_highlight(self) -> Self {
        Self {
            ink: self.ink_on_wash,
            ink_2: self.muted_on_wash,
            muted: self.muted_on_wash,
            faint: self.muted_on_wash,
            ..self
        }
    }
}

pub(crate) struct TypeScale;

impl TypeScale {
    /// Thin aliases onto [`WorkspaceTokens`] typography fields (kit ladder, ADR-0025).
    pub(crate) fn meta(tokens: &WorkspaceTokens) -> Pixels {
        tokens.text_meta
    }

    pub(crate) fn chip(tokens: &WorkspaceTokens) -> Pixels {
        tokens.text_chip
    }

    pub(crate) fn control(tokens: &WorkspaceTokens) -> Pixels {
        tokens.text_control
    }

    pub(crate) fn body(tokens: &WorkspaceTokens) -> Pixels {
        tokens.text_body
    }

    pub(crate) fn lede(tokens: &WorkspaceTokens) -> Pixels {
        tokens.text_lede
    }

    pub(crate) fn title(tokens: &WorkspaceTokens) -> Pixels {
        tokens.text_title
    }

    pub(crate) fn clock(tokens: &WorkspaceTokens) -> Pixels {
        tokens.text_clock
    }

    pub(crate) fn display(tokens: &WorkspaceTokens) -> Pixels {
        tokens.text_display
    }

    /// Kit monospace family from the active theme (resolved at init; never hardcode Menlo).
    pub(crate) fn mono(cx: &App) -> SharedString {
        cx.theme().mono_font_family.clone()
    }
}

pub(crate) struct Space;

impl Space {
    pub(crate) const XS: Pixels = px(4.0);
    pub(crate) const SM: Pixels = px(8.0);
    pub(crate) const MD: Pixels = px(12.0);
    pub(crate) const LG: Pixels = px(16.0);
}

/// WCAG 2.x relative luminance from linearized sRGB (not HSL lightness).
#[cfg(test)]
fn relative_luminance(color: Hsla) -> f32 {
    let rgb = color.to_rgb();
    fn linearize(channel: f32) -> f32 {
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * linearize(rgb.r) + 0.7152 * linearize(rgb.g) + 0.0722 * linearize(rgb.b)
}

/// WCAG contrast ratio `(L1 + 0.05) / (L2 + 0.05)` using relative luminance.
#[cfg(test)]
fn contrast_ratio(fg: Hsla, bg: Hsla) -> f32 {
    let l1 = relative_luminance(fg);
    let l2 = relative_luminance(bg);
    let (hi, lo) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use gpui_kit::Hsla;
    use gpui_kit::TestAppContext;
    use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode, scroll::ScrollbarMode};

    use super::{
        contrast_ratio, install_visible_scrollbars, relative_luminance, sync_system_appearance,
    };

    #[gpui_kit::test]
    fn relative_luminance_matches_wcag_reference_points() {
        let black = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.0,
            a: 1.0,
        };
        let white = Hsla {
            h: 0.0,
            s: 0.0,
            l: 1.0,
            a: 1.0,
        };
        assert!(
            (relative_luminance(black) - 0.0).abs() < 1e-5,
            "black luminance"
        );
        assert!(
            (relative_luminance(white) - 1.0).abs() < 1e-5,
            "white luminance"
        );
        // Pure mid-gray in HSL is not 0.5 relative luminance — HSL L must not be used as a proxy.
        let mid_gray = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.5,
            a: 1.0,
        };
        let mid_l = relative_luminance(mid_gray);
        assert!(
            (mid_l - 0.5).abs() > 0.05,
            "HSL L=0.5 must not equal relative luminance (got {mid_l})"
        );
        assert!(
            (contrast_ratio(black, white) - 21.0).abs() < 0.05,
            "black on white must be ~21:1"
        );
    }

    #[gpui_kit::test]
    fn visible_scrollbars_survive_theme_changes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_visible_scrollbars(cx);
            assert_eq!(Theme::global(cx).scrollbar_mode, ScrollbarMode::Always);
            Theme::change(ThemeMode::Dark, None, cx);
            install_visible_scrollbars(cx);
            assert_eq!(Theme::global(cx).scrollbar_mode, ScrollbarMode::Always);
            Theme::change(ThemeMode::Light, None, cx);
            install_visible_scrollbars(cx);
            assert_eq!(Theme::global(cx).scrollbar_mode, ScrollbarMode::Always);
            sync_system_appearance(None, cx);
            assert_eq!(Theme::global(cx).scrollbar_mode, ScrollbarMode::Always);
        });
    }

    #[gpui_kit::test]
    fn resolve_reads_active_theme_tokens(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            for mode in [ThemeMode::Light, ThemeMode::Dark] {
                Theme::change(mode, None, cx);
                let tokens = super::WorkspaceTokens::resolve(cx);
                assert_eq!(tokens.ground, cx.theme().background);
                assert_eq!(tokens.ink, cx.theme().foreground);
                assert_eq!(tokens.live, cx.theme().danger);
                assert_eq!(tokens.accent_wash, cx.theme().accent);
                assert_eq!(tokens.live_wash, cx.theme().accent);
                assert_eq!(tokens.ink_on_wash, cx.theme().accent_foreground);
                assert_eq!(tokens.muted_on_wash, cx.theme().accent_foreground);
                assert_eq!(tokens.warn_wash, cx.theme().background);
                assert_eq!(tokens.warn_line, cx.theme().warning);
                assert_eq!(tokens.line_soft, cx.theme().border);
                // Fills must not be reused as body text on ground.
                assert_ne!(
                    tokens.accent_ink,
                    cx.theme().accent,
                    "{mode:?}: accent_ink must not be the accent fill"
                );
                // WCAG normal-text floor (4.5:1), including small highlight metadata.
                assert!(
                    contrast_ratio(tokens.accent_ink, tokens.ground) >= 4.5,
                    "{mode:?}: accent_ink on ground contrast too low ({:.2})",
                    contrast_ratio(tokens.accent_ink, tokens.ground)
                );
                assert!(
                    contrast_ratio(tokens.live_ink, tokens.live_wash) >= 4.5,
                    "{mode:?}: live_ink on live_wash contrast too low ({:.2})",
                    contrast_ratio(tokens.live_ink, tokens.live_wash)
                );
                assert!(
                    contrast_ratio(tokens.warn, tokens.warn_wash) >= 4.5,
                    "{mode:?}: warn on warn_wash contrast too low ({:.2})",
                    contrast_ratio(tokens.warn, tokens.warn_wash)
                );
                assert!(
                    contrast_ratio(tokens.ink_on_wash, tokens.live_wash) >= 4.5,
                    "{mode:?}: ink_on_wash on live_wash contrast too low ({:.2})",
                    contrast_ratio(tokens.ink_on_wash, tokens.live_wash)
                );
                assert!(
                    contrast_ratio(tokens.ink_on_wash, tokens.accent_wash) >= 4.5,
                    "{mode:?}: ink_on_wash on accent_wash contrast too low ({:.2})",
                    contrast_ratio(tokens.ink_on_wash, tokens.accent_wash)
                );
                assert!(
                    contrast_ratio(tokens.muted_on_wash, tokens.accent_wash) >= 4.5,
                    "{mode:?}: muted_on_wash on accent_wash contrast too low ({:.2})",
                    contrast_ratio(tokens.muted_on_wash, tokens.accent_wash)
                );
                assert_ne!(
                    tokens.surface, tokens.surface_2,
                    "{mode:?}: row hover must differ from the surrounding surface"
                );
            }
        });
    }
}
