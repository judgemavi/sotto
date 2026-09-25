//! Thin product input element over Longbridge single-line controls.
//!
//! Notes full-document Markdown editing uses stock [`gpui_kit::component::input::Editor`]
//! directly (ADR-0025). Ask, rename, search, and annotation stay on [`InputState`].

use gpui_kit::component::input::{Input as KitInput, InputState};
use gpui_kit::component::{Sizable as _, Size};
use gpui_kit::{
    App, Entity, IntoElement, RenderOnce, StyleRefinement, Styled, Window, div, prelude::*,
};

/// Product single-line input over kit [`KitInput`].
#[derive(IntoElement)]
pub(crate) struct Input {
    state: Entity<InputState>,
    disabled: bool,
    cleanable: bool,
    size: Size,
    style: StyleRefinement,
}

impl Input {
    pub(crate) fn new(state: &Entity<InputState>) -> Self {
        Self {
            state: state.clone(),
            disabled: false,
            cleanable: false,
            size: Size::Medium,
            style: StyleRefinement::default(),
        }
    }

    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub(crate) fn cleanable(mut self, cleanable: bool) -> Self {
        self.cleanable = cleanable;
        self
    }

    pub(crate) fn with_size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    pub(crate) fn small(self) -> Self {
        self.with_size(Size::Small)
    }

    pub(crate) fn xsmall(self) -> Self {
        self.with_size(Size::XSmall)
    }
}

impl Styled for Input {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl RenderOnce for Input {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        // Stock `.disabled` updates InputState and rejects keyboard input — do not fake it
        // with opacity + a mouse-only overlay (ADR-0025 review).
        let field = KitInput::new(&self.state)
            .with_size(self.size)
            .disabled(self.disabled)
            .cleanable(self.cleanable);

        let mut view = div();
        view.style().refine(&self.style);
        view.relative().w_full().child(field)
    }
}
