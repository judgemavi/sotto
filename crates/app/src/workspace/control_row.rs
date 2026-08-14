//! Flex rows whose children declare what must survive horizontal pressure.
//!
//! This is the Rust form of the normative mock's three shrink classes:
//!
//! | mock class   | [`ControlRole`]         | behaviour                                      |
//! |--------------|-------------------------|------------------------------------------------|
//! | `.keep`      | [`ControlRole::Essential`]  | intrinsic width, never compressed, never clipped |
//! | `.ellip`     | [`ControlRole::Ellipsizing`] | absorbs the remaining width and truncates      |
//! | `.collapses` | [`ControlRole::Expendable`] | dropped entirely once the row gets narrow       |
//!
//! Call sites choose a role, never a width. Bounding widths at the call site is what produced the
//! clipping defects this primitive exists to end, so [`ControlRow::finish`] deliberately hands back
//! a value that cannot accept an unclassified child.

use std::panic::Location;

use gpui::{
    AnyElement, Bounds, Div, Element, ElementId, GlobalElementId, InspectorElementId,
    Interactivity, IntoElement, LayoutId, Pixels, StyleRefinement, div, prelude::*, px,
};

/// The horizontal survival contract for one control-row child.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ControlRole {
    /// Retains its intrinsic width. Use for terminal controls and status values.
    Essential,
    /// Receives the remaining width and visibly truncates overflowing text.
    Ellipsizing,
    /// Gives up its width, and then its place in the row, before anything essential is compressed.
    Expendable,
}

/// A row that makes horizontal survival priorities explicit at each call site.
pub(crate) struct ControlRow {
    row: Div,
    available: Pixels,
    essential_count: usize,
    rendered_children: usize,
}

/// A finished control row.
///
/// It forwards styling, interactivity and element behaviour to the flex row it wraps, but it
/// deliberately does *not* implement [`ParentElement`]. A control appended after `finish()` would
/// carry no shrink role, and that is exactly the mistake this primitive exists to make impossible:
/// the only way to add a control is [`ControlRow::child`], which demands a role.
pub(crate) struct ControlRowElement {
    row: Div,
}

impl ControlRow {
    /// Row width at or below which every expendable child is dropped rather than squeezed.
    ///
    /// The mock stages this across two media queries (chips at 900 px, the remaining `.collapses`
    /// controls at 700 px). T072 gives the primitive a single expendable role, so it uses one
    /// threshold, placed high enough that no expendable control is ever painted mid-clip.
    pub(crate) const COLLAPSE_WIDTH: Pixels = px(760.0);

    /// Builds a row inside a container whose width the caller cannot see — a rail or a column
    /// header. Expendable children still yield their width first, but nothing is dropped, because
    /// the row has no honest basis for deciding that it must be.
    pub(crate) fn new() -> Self {
        Self::for_width(Pixels::MAX)
    }

    /// Builds a row that knows how much width it has, so it can drop rather than clip.
    pub(crate) fn for_width(available: Pixels) -> Self {
        Self {
            row: div()
                .w_full()
                .min_w_0()
                .overflow_hidden()
                .flex()
                .items_center(),
            available,
            essential_count: 0,
            rendered_children: 0,
        }
    }

    pub(crate) fn child(mut self, role: ControlRole, child: impl IntoElement) -> Self {
        if role == ControlRole::Expendable && self.available <= Self::COLLAPSE_WIDTH {
            return self;
        }
        let child = child.into_any_element();
        self.row = self.row.child(role.wrap(child));
        self.rendered_children += 1;
        if role == ControlRole::Essential {
            self.essential_count += 1;
        }
        self
    }

    /// How many role-bearing children survived this row's width.
    #[cfg(test)]
    pub(crate) const fn rendered_children(&self) -> usize {
        self.rendered_children
    }

    /// Adds a child only when `condition` holds, so optional controls still name a role.
    pub(crate) fn child_when(
        self,
        condition: bool,
        role: ControlRole,
        child: impl FnOnce() -> AnyElement,
    ) -> Self {
        if condition {
            self.child(role, child())
        } else {
            self
        }
    }

    /// Inserts flexible empty space, used by the view bar to centre its tab pair.
    pub(crate) fn spacer(mut self) -> Self {
        self.row = self.row.child(div().flex_1().min_w_0());
        self
    }

    /// Completes the row, rejecting contracts that protect nothing.
    pub(crate) fn finish(self) -> ControlRowElement {
        assert!(
            self.essential_count > 0,
            "a control row must mark at least one child Essential"
        );
        ControlRowElement { row: self.row }
    }
}

impl Styled for ControlRowElement {
    fn style(&mut self) -> &mut StyleRefinement {
        self.row.style()
    }
}

impl InteractiveElement for ControlRowElement {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.row.interactivity()
    }
}

impl IntoElement for ControlRowElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ControlRowElement {
    type RequestLayoutState = <Div as Element>::RequestLayoutState;
    type PrepaintState = <Div as Element>::PrepaintState;

    fn id(&self) -> Option<ElementId> {
        Element::id(&self.row)
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        self.row.source_location()
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.row.request_layout(id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> Self::PrepaintState {
        self.row
            .prepaint(id, inspector_id, bounds, request_layout, window, cx)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) {
        self.row.paint(
            id,
            inspector_id,
            bounds,
            request_layout,
            prepaint,
            window,
            cx,
        );
    }
}

impl ControlRole {
    fn wrap(self, child: AnyElement) -> Div {
        match self {
            Self::Essential => div()
                .flex_none()
                .debug_selector(|| "control-row-essential".into())
                .child(child),
            // `flex: 1 1 auto`, exactly as the mock's `.ellip`. The natural width is the basis, so
            // a title is only ever squeezed by real pressure — not by sharing free space evenly
            // with the spacers that centre the view bar's tab pair.
            Self::Ellipsizing => div()
                .flex_auto()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .debug_selector(|| "control-row-ellipsizing".into())
                .child(child),
            Self::Expendable => div()
                .flex_initial()
                .min_w_0()
                .overflow_hidden()
                .debug_selector(|| "control-row-expendable".into())
                .child(child),
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{ParentElement as _, div, px};

    use super::{ControlRole, ControlRow};

    #[test]
    #[should_panic(expected = "must mark at least one child Essential")]
    fn a_row_that_marks_nothing_essential_is_rejected() {
        let _ = ControlRow::for_width(px(1200.0))
            .child(ControlRole::Ellipsizing, div().child("title"))
            .child(ControlRole::Expendable, div().child("secondary"))
            .finish();
    }

    #[test]
    fn a_narrow_row_drops_expendable_children_instead_of_squeezing_them() {
        let wide = ControlRow::for_width(ControlRow::COLLAPSE_WIDTH + px(1.0))
            .child(ControlRole::Essential, div().child("stop"))
            .child(ControlRole::Expendable, div().child("pause"));
        assert_eq!(
            wide.rendered_children(),
            2,
            "above the collapse width an expendable control keeps its place"
        );

        let narrow = ControlRow::for_width(ControlRow::COLLAPSE_WIDTH)
            .child(ControlRole::Essential, div().child("stop"))
            .child(ControlRole::Expendable, div().child("pause"));
        assert_eq!(
            narrow.rendered_children(),
            1,
            "at the collapse width the expendable control is dropped, not clipped"
        );
    }
}
