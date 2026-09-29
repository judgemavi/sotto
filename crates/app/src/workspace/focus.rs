//! Stock Longbridge button access.
//!
//! There is intentionally no Sotto button component or constructor.

use gpui_kit::component::Size as KitSize;
use gpui_kit::{InteractiveElement as _, IntoElement, ParentElement as _, SharedString, div};

pub(crate) type Size = KitSize;
pub(crate) use gpui_kit::component::button::Button;

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
