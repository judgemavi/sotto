//! Workspace-wide focus presentation for interactive controls.
//!
//! `gpui-component` gives each button the correct focus handle and tab-stop behavior, but draws
//! its built-in ring outside the control at 20% opacity. [`Button`] keeps those mechanics and
//! replaces the visible treatment with an opaque border painted inside the button's own bounds.
//! Transparent ghost fills cannot expose its interior, and clipping parents cannot eat it.

use gpui::{ElementId, Hsla, StyleRefinement, prelude::*};
use gpui_component::button::Button as ComponentButton;

fn focused_style(style: StyleRefinement, color: Hsla) -> StyleRefinement {
    style.border_2().border_color(color)
}

/// Constructs every workspace button with Sotto's focus treatment.
///
/// The returned value is still `gpui-component`'s button, so its focus handle, tab stop, pointer
/// behavior, keyboard activation, variants, and selected fill are unchanged.
pub(crate) struct Button;

impl Button {
    #[expect(
        clippy::new_ret_no_self,
        reason = "drop-in constructor keeps every existing Button::new call on the shared seam"
    )]
    pub(crate) fn new(
        id: impl Into<ElementId>,
        tokens: super::tokens::WorkspaceTokens,
    ) -> ComponentButton {
        let color = tokens.focus;
        ComponentButton::new(id)
            // The component's weak halo is an absolute child outside these bounds. Clip that
            // superseded treatment at the control itself; the inset border below remains visible.
            .overflow_hidden()
            .focus(move |style| focused_style(style, color.into()))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{StyleRefinement, px, rgb};

    use super::focused_style;

    #[test]
    fn focused_controls_bind_an_opaque_inset_border_and_no_shadow() {
        let style = focused_style(StyleRefinement::default(), rgb(0xffffff).into());

        assert_eq!(style.border_color, Some(rgb(0xffffff).into()));
        assert_eq!(style.border_widths.top, Some(px(2.0).into()));
        assert_eq!(style.border_widths.right, Some(px(2.0).into()));
        assert_eq!(style.border_widths.bottom, Some(px(2.0).into()));
        assert_eq!(style.border_widths.left, Some(px(2.0).into()));
        assert!(
            style.box_shadow.is_none(),
            "a transparent ghost button must have nothing painted behind its label"
        );
        assert!(
            style.background.is_none(),
            "focus must not replace the fill that distinguishes Ask's selected state"
        );
    }
}
