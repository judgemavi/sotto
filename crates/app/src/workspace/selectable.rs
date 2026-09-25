//! Retained selectable markdown leaves for the *current* meeting view.
//!
//! Kit `TextView` selection is window-scoped under `Root` (⌘C / Ctrl+C). Holding one
//! [`TextViewState`] per leaf id keeps selection stable across frames when source text is
//! unchanged. Retention is scoped to the open recording: switching tabs, changing the
//! selected meeting, going Home, or deleting clears the map so entities do not accumulate
//! for the life of the process (ADR-0025 review).
//!
//! A press on a leaf records that leaf as the drag origin; foreign leaves suppress kit
//! text selection so a drag past a row boundary cannot silently jump. Copy prefers the
//! origin leaf, then window [`TextSelection`]. Retained leaves from a prior view are never
//! scanned — [`clear_retained`] runs whenever the displayed recording or stage changes.

use std::collections::HashMap;

use gpui_kit::base::{GlobalState, TextSelection};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::text::{TextView, TextViewState, TextViewStyle};
use gpui_kit::{App, AppContext, ClipboardItem, Entity, Global, KeyBinding, SharedString};

gpui_kit::actions!(selectable, [CopySelection]);

#[derive(Default)]
struct SelectableDocs {
    docs: HashMap<u64, Entity<TextViewState>>,
    /// Leaf that received the current primary press. Cross-row drags stay on it.
    press_origin: Option<u64>,
}

impl Global for SelectableDocs {}

struct CopyBindingsInstalled;

impl Global for CopyBindingsInstalled {}

fn ensure_docs(cx: &mut App) {
    if !cx.has_global::<SelectableDocs>() {
        cx.set_global(SelectableDocs::default());
    }
}

/// Bind ⌘C / Ctrl+C to [`CopySelection`] outside the Input key context.
///
/// Idempotent: product windows and test mounts both call this, and GPUI accumulates
/// duplicate bindings rather than replacing them.
pub(crate) fn bind_copy_keys(cx: &mut App) {
    if cx.has_global::<CopyBindingsInstalled>() {
        return;
    }
    cx.set_global(CopyBindingsInstalled);
    #[cfg(target_os = "macos")]
    cx.bind_keys([KeyBinding::new("cmd-c", CopySelection, None)]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([KeyBinding::new("ctrl-c", CopySelection, None)]);
    cx.on_action(copy_selection);
}

/// Record the leaf where a text drag began.
pub(crate) fn begin_leaf_press(key: u64, cx: &mut App) {
    ensure_docs(cx);
    cx.global_mut::<SelectableDocs>().press_origin = Some(key);
}

/// While a drag that started on another leaf crosses this one, suppress kit text selection.
pub(crate) fn suppress_if_foreign_leaf(key: u64, cx: &mut App) {
    let Some(origin) = cx
        .try_global::<SelectableDocs>()
        .and_then(|docs| docs.press_origin)
    else {
        return;
    };
    if origin != key {
        GlobalState::suppress_text_selection(cx);
    }
}

fn copy_selection(_: &CopySelection, cx: &mut App) {
    ensure_docs(cx);
    let origin = cx.global::<SelectableDocs>().press_origin;
    if let Some(key) = origin
        && let Some(doc) = cx.global::<SelectableDocs>().docs.get(&key).cloned()
    {
        let text = doc.read(cx).selected_text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            return;
        }
    }
    // Window-scoped selection only — never scan retained leaves from a prior view.
    // Retention is cleared whenever the displayed recording/view changes.
    if let Some(window) = cx.active_window() {
        let copied = window
            .update(cx, |_, window, cx| {
                let text = TextSelection::selected_text(window, cx);
                if text.is_empty() { None } else { Some(text) }
            })
            .ok()
            .flatten();
        if let Some(text) = copied {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }
}

/// Drop every retained leaf. Call when the open recording changes or is deleted.
pub(crate) fn clear_retained(cx: &mut App) {
    if cx.has_global::<SelectableDocs>() {
        let docs = cx.global_mut::<SelectableDocs>();
        docs.docs.clear();
        docs.press_origin = None;
    }
}

/// A retained markdown element for one selectable leaf in the current view.
///
/// `key` must be stable across frames for the same leaf. Changing `source` updates the entity.
pub(crate) fn retained_markdown(
    key: u64,
    source: impl Into<SharedString>,
    style: TextViewStyle,
    cx: &mut App,
) -> TextView {
    let source = source.into();
    ensure_docs(cx);
    let existing = cx.global::<SelectableDocs>().docs.get(&key).cloned();
    let entity = if let Some(existing) = existing {
        existing
    } else {
        let entity = cx.new(|cx| TextViewState::markdown(source.as_ref(), cx));
        cx.global_mut::<SelectableDocs>()
            .docs
            .insert(key, entity.clone());
        entity
    };
    entity.update(cx, |view, cx| {
        view.set_text(source.as_ref(), cx);
    });
    TextView::new(&entity).style(style).selectable(true)
}

/// Default reading-mode text style folded onto the active theme.
#[expect(dead_code, reason = "theme-aware reading style for future note leaves")]
pub(crate) fn reading_style(cx: &App) -> TextViewStyle {
    let _ = cx.theme();
    TextViewStyle::default()
}

#[cfg(test)]
mod tests {
    use gpui_kit::TestAppContext;

    use super::{SelectableDocs, clear_retained, retained_markdown};

    #[gpui_kit::test]
    fn clear_retained_drops_accumulated_leaves(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            let style = gpui_kit::component::text::TextViewStyle::default();
            let _ = retained_markdown(1, "one", style.clone(), cx);
            let _ = retained_markdown(2, "two", style, cx);
            assert_eq!(cx.global::<SelectableDocs>().docs.len(), 2);
            clear_retained(cx);
            assert!(cx.global::<SelectableDocs>().docs.is_empty());
        });
    }
}
