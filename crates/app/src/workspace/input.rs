//! Re-export of the stock Longbridge single-line input.
//!
//! Sotto does not own an input component. Ask, rename, search, and annotations all use the
//! gpui-kit control directly so its theme, focus, validation, and sizing remain kit behaviour.

pub(crate) use gpui_kit::component::input::Input;
