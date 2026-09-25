//! Thin product helpers over stock Longbridge buttons.
//!
//! ADR-0025: appearance comes from the kit. This module no longer paints a Sotto focus ring or
//! private variants. Prefer stock [`Button`] builders; use [`with_debug_selector`] when a test
//! must click a control the kit does not tag.

use gpui_kit::component::button::{Button as KitButton, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, Selectable as _, Sizable as _, Size as KitSize};
use gpui_kit::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement as _, IntoElement, ParentElement,
    SharedString, StyleRefinement, Styled, Window, div,
};

use super::icons::IconName;
use super::tokens::WorkspaceTokens;

pub(crate) type Size = KitSize;
pub(crate) type Button = ProductButton;

/// Fluent builder that mirrors the previous product API onto stock kit buttons.
///
/// `tokens` is accepted and ignored so existing `Button::new(id, tokens)` call sites compile
/// while chrome adopts Longbridge styling.
#[derive(IntoElement)]
pub(crate) struct ProductButton {
    inner: KitButton,
    debug_selector: Option<SharedString>,
}

impl ProductButton {
    pub(crate) fn new(id: impl Into<ElementId>, _tokens: WorkspaceTokens) -> Self {
        Self {
            inner: KitButton::new(id),
            debug_selector: None,
        }
    }

    pub(crate) fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.inner = self.inner.label(label);
        self
    }

    pub(crate) fn icon(mut self, icon: impl Into<IconName>) -> Self {
        self.inner = self.inner.icon(icon.into());
        self
    }

    pub(crate) fn tooltip(mut self, label: impl Into<SharedString>) -> Self {
        self.inner = self.inner.tooltip(label);
        self
    }

    pub(crate) fn debug_selector(mut self, selector: impl FnOnce() -> SharedString) -> Self {
        self.debug_selector = Some(selector());
        self
    }

    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.inner = self.inner.disabled(disabled);
        self
    }

    pub(crate) fn selected(mut self, selected: bool) -> Self {
        self.inner = self.inner.selected(selected);
        self
    }

    pub(crate) fn with_size(mut self, size: Size) -> Self {
        self.inner = self.inner.with_size(size);
        self
    }

    pub(crate) fn small(self) -> Self {
        self.with_size(KitSize::Small)
    }

    pub(crate) fn xsmall(self) -> Self {
        self.with_size(KitSize::XSmall)
    }

    pub(crate) fn ghost(mut self) -> Self {
        self.inner = self.inner.ghost();
        self
    }

    pub(crate) fn primary(mut self) -> Self {
        self.inner = self.inner.primary();
        self
    }

    pub(crate) fn danger(mut self) -> Self {
        self.inner = self.inner.danger();
        self
    }

    pub(crate) fn outline(mut self) -> Self {
        self.inner = self.inner.outline();
        self
    }

    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.inner = self.inner.on_click(handler);
        self
    }
}

impl Styled for ProductButton {
    fn style(&mut self) -> &mut StyleRefinement {
        self.inner.style()
    }
}

impl ParentElement for ProductButton {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.inner.extend(elements);
    }
}

impl gpui_kit::RenderOnce for ProductButton {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let button = self.inner;
        match self.debug_selector {
            Some(selector) => {
                let id = selector.clone();
                div()
                    .id(id)
                    .debug_selector(move || selector.to_string())
                    .child(button)
                    .into_any_element()
            }
            None => button.into_any_element(),
        }
    }
}

/// Wrap any stock control when a test needs a stable selector the kit does not provide.
#[expect(
    dead_code,
    reason = "test selector helper retained for call sites that need it"
)]
pub(crate) fn with_debug_selector(
    selector: impl Into<SharedString>,
    child: impl IntoElement,
) -> impl IntoElement {
    let selector = selector.into();
    let id = selector.clone();
    div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .child(child)
}
