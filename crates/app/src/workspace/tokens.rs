//! Theme-resolved workspace design tokens for the quiet-page visual system.
//!
//! `docs/design/workspace-v4-mock.html` is the direction: a readable notebook for everyday use,
//! not an engineering console. Field names still follow the older mocks so existing callers keep
//! compiling; the hex values moved together in both appearances so one theme cannot paint the
//! other's ink.
//!
//! - `surface_2` is the raised/hovered variant of `surface`.
//! - `ink_2` is the mid-weight body ink.
//! - `accent_on` and `live_on` are the inks that sit *on* those fills. CSS would inherit; GPUI
//!   must be told.

use std::rc::Rc;

use gpui::{
    AnyView, App, Context, FocusHandle, IntoElement, Pixels, Render, Rgba, Window, div, prelude::*,
    px, rgb, rgba,
};
use gpui_component::{ActiveTheme as _, Theme};

// The component library applies 20% alpha to this token. Black-on-light and white-on-dark retain
// the greatest possible edge contrast after that fixed alpha is applied.
const LIGHT_FOCUS_RING: &str = "#000000";
const DARK_FOCUS_RING: &str = "#FFFFFF";

/// Installs Sotto's focus colour into both component-theme appearances.
///
/// `gpui-component` owns the geometry of its button focus ring, but its default ring colour is
/// unrelated to Sotto's surfaces and is drawn at 20% alpha. Keeping the colour in both stored
/// theme configurations matters: [`Theme::change`] reapplies one of those configurations whenever
/// the person switches appearance, so changing only the active colour would repair one frame and
/// lose the indicator at the next switch.
pub(crate) fn install_component_focus_ring(cx: &mut App) {
    let theme = Theme::global_mut(cx);
    Rc::make_mut(&mut theme.light_theme).colors.ring = Some(LIGHT_FOCUS_RING.into());
    Rc::make_mut(&mut theme.dark_theme).colors.ring = Some(DARK_FOCUS_RING.into());
    theme.colors.ring = if theme.is_dark() {
        rgb(0xffffff).into()
    } else {
        rgb(0x000000).into()
    };
}

/// Overlay scrollbars stay visible so a long transcript or notes column can be judged at a glance.
///
/// `gpui_component::init` copies macOS "Show scroll bars: When scrolling", which fades the thumb
/// after idle. That hides how much of the recording is off-screen. Appearance changes do not
/// reset this; call it after init and after any `Theme::change` that might rebuild the global.
pub(crate) fn install_visible_scrollbars(cx: &mut App) {
    Theme::global_mut(cx).scrollbar_show = gpui_component::scroll::ScrollbarShow::Always;
}

/// Non-tab-stop focus origin which lets the first Tab enter `gpui-component::Root`'s key context.
///
/// GPUI dispatches a key through the focused node's ancestry. With no focused node, Root's Tab
/// action is never reached, even though the frame contains tab stops. This wrapper is focused when
/// the window is built and sits between Root and the workspace, so the first Tab reaches Root and
/// moves to the first real control without presenting the origin itself as a stop.
pub struct KeyboardRoot {
    focus_handle: FocusHandle,
    view: AnyView,
}

impl KeyboardRoot {
    #[must_use]
    pub fn new(view: impl Into<AnyView>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
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
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .child(self.view.clone())
    }
}

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
    /// Opaque inset focus border: maximum contrast for the active appearance.
    pub(crate) focus: Rgba,
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
                ground: rgb(0x16141c),
                surface: rgb(0x1e1b26),
                surface_2: rgb(0x252230),
                sunken: rgb(0x121018),
                line: rgb(0x2f2b3a),
                line_soft: rgb(0x272430),
                ink: rgb(0xf4f1f8),
                ink_2: rgb(0xcfc8dc),
                muted: rgb(0xb7b1c4),
                faint: rgb(0x8a8498),
                focus: rgb(0xffffff),
                accent: rgb(0xc8b6ee),
                accent_ink: rgb(0xd4c6f4),
                accent_on: rgb(0x1c1924),
                accent_wash: rgb(0x2a2438),
                accent_line: rgb(0x4a3f68),
                live: rgb(0xf07a70),
                live_ink: rgb(0xf49a93),
                live_on: rgb(0x2a0f0c),
                live_wash: rgb(0x3a2220),
                live_line: rgb(0x63302a),
                warn: rgb(0xe0b07a),
                warn_wash: rgb(0x32261a),
                scrim: rgba(0x00000099),
            }
        } else {
            Self {
                ground: rgb(0xf3f2f7),
                surface: rgb(0xfffdff),
                surface_2: rgb(0xf7f6fb),
                sunken: rgb(0xeeeaf4),
                line: rgb(0xe4e1eb),
                line_soft: rgb(0xeceaf1),
                ink: rgb(0x1c1924),
                ink_2: rgb(0x3d3848),
                muted: rgb(0x5f5a6a),
                faint: rgb(0x8b8696),
                focus: rgb(0x000000),
                accent: rgb(0x5a4588),
                accent_ink: rgb(0x4b3874),
                accent_on: rgb(0xffffff),
                accent_wash: rgb(0xede8f6),
                accent_line: rgb(0xd4cce8),
                live: rgb(0xc94b40),
                live_ink: rgb(0xb43e35),
                live_on: rgb(0xffffff),
                live_wash: rgb(0xfbedec),
                live_line: rgb(0xf0c7c3),
                warn: rgb(0x8a5a28),
                warn_wash: rgb(0xf7eedf),
                scrim: rgba(0x1c19245c),
            }
        }
    }
}

pub(crate) struct TypeScale;

impl TypeScale {
    /// Eyebrows, chips, timecodes and other measured metadata.
    pub(crate) const META: Pixels = px(12.0);
    /// Scope chips and the capture bar's recording kind.
    pub(crate) const CHIP: Pixels = px(12.5);
    /// Buttons, Ask copy, and other chrome that should share one size.
    pub(crate) const CONTROL: Pixels = px(14.0);
    pub(crate) const BODY: Pixels = px(15.0);
    /// Home lede and the primary Record a call label.
    pub(crate) const LEDE: Pixels = px(16.0);
    /// The open session's title in the view bar.
    pub(crate) const TITLE: Pixels = px(17.0);
    pub(crate) const CLOCK: Pixels = px(22.0);
    /// Home headline. macOS ships New York; elsewhere GPUI falls back.
    pub(crate) const DISPLAY: Pixels = px(34.0);
    pub(crate) const READING: &'static str = "New York";
}

pub(crate) struct Space;

impl Space {
    pub(crate) const XS: Pixels = px(4.0);
    pub(crate) const SM: Pixels = px(8.0);
    pub(crate) const MD: Pixels = px(12.0);
    pub(crate) const LG: Pixels = px(16.0);
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, ops::Deref as _, rc::Rc};

    use gpui::{
        Bounds, Context, IntoElement, Render, TestAppContext, VisualTestContext, Window,
        WindowBounds, WindowOptions, div, point, prelude::*, px, size,
    };
    use gpui_component::{Root, Theme, ThemeMode, button::Button};

    use super::{DARK_FOCUS_RING, KeyboardRoot, LIGHT_FOCUS_RING, install_component_focus_ring};

    struct KeyboardProbe {
        activated: Rc<Cell<bool>>,
    }

    impl Render for KeyboardProbe {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let activated = Rc::clone(&self.activated);
            div()
                .child(crate::workspace::notes::evidence_control(
                    "keyboard-probe".into(),
                    "keyboard-probe".to_owned(),
                    "Show timecodes".to_owned(),
                    "Show or hide timecodes",
                    super::WorkspaceTokens::resolve(cx),
                    move |_| activated.set(true),
                ))
                .child(Button::new("keyboard-probe-second").label("Edit note"))
        }
    }

    #[test]
    fn focus_tokens_survive_both_appearance_changes() -> Result<(), Box<dyn std::error::Error>> {
        let cx = TestAppContext::single();
        cx.update(|cx| {
            gpui_component::init(cx);
            install_component_focus_ring(cx);
            assert_eq!(
                Theme::global(cx)
                    .light_theme
                    .colors
                    .ring
                    .as_ref()
                    .map(AsRef::as_ref),
                Some(LIGHT_FOCUS_RING)
            );
            assert_eq!(
                Theme::global(cx)
                    .dark_theme
                    .colors
                    .ring
                    .as_ref()
                    .map(AsRef::as_ref),
                Some(DARK_FOCUS_RING)
            );
            Theme::change(ThemeMode::Dark, None, cx);
            assert_eq!(Theme::global(cx).colors.ring, gpui::rgb(0xffffff).into());
            Theme::change(ThemeMode::Light, None, cx);
            assert_eq!(Theme::global(cx).colors.ring, gpui::rgb(0x000000).into());
        });
        Ok(())
    }

    #[test]
    fn scrollbars_stay_visible_across_appearance_changes() -> Result<(), Box<dyn std::error::Error>>
    {
        let cx = TestAppContext::single();
        cx.update(|cx| {
            gpui_component::init(cx);
            super::install_visible_scrollbars(cx);
            assert_eq!(
                Theme::global(cx).scrollbar_show,
                gpui_component::scroll::ScrollbarShow::Always
            );
            Theme::change(ThemeMode::Dark, None, cx);
            super::install_visible_scrollbars(cx);
            assert_eq!(
                Theme::global(cx).scrollbar_show,
                gpui_component::scroll::ScrollbarShow::Always
            );
            Theme::change(ThemeMode::Light, None, cx);
            super::install_visible_scrollbars(cx);
            assert_eq!(
                Theme::global(cx).scrollbar_show,
                gpui_component::scroll::ScrollbarShow::Always
            );
        });
        Ok(())
    }

    #[test]
    fn root_tab_focus_reaches_a_real_button_and_enter_bubbles_to_its_handler()
    -> Result<(), Box<dyn std::error::Error>> {
        let cx = TestAppContext::single();
        let activated = Rc::new(Cell::new(false));
        let probe = Rc::clone(&activated);
        let handle = cx.update(|cx| {
            gpui_component::init(cx);
            install_component_focus_ring(cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(0.0), px(0.0)),
                        size: size(px(320.0), px(180.0)),
                    })),
                    ..WindowOptions::default()
                },
                move |window, cx| {
                    let view = cx.new(|_| KeyboardProbe { activated: probe });
                    let keyboard_root = cx.new(|cx| KeyboardRoot::new(view, window, cx));
                    cx.new(|cx| Root::new(keyboard_root, window, cx))
                },
            )
        })?;
        let visual = VisualTestContext::from_window(*handle.deref(), &cx).into_mut();
        visual.update(|window, _| window.activate_window());
        visual.run_until_parked();
        let origin = visual.update(|window, cx| format!("{:?}", window.focused(cx)));
        visual.simulate_keystrokes("tab");
        let first = visual.update(|window, cx| format!("{:?}", window.focused(cx)));
        assert_ne!(first, origin, "Tab must leave the non-stop focus origin");
        visual.simulate_keystrokes("enter");
        assert!(
            activated.get(),
            "the focused button's ancestor must receive Enter"
        );
        visual.simulate_keystrokes("shift-tab");
        let previous = visual.update(|window, cx| format!("{:?}", window.focused(cx)));
        assert_ne!(previous, first, "Shift-Tab must move to the prior control");
        Ok(())
    }
}
