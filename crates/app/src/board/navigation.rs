//! Pure viewport navigation for the live and post-call board lenses.

use super::BoardViewport;

const MIN_ZOOM: f32 = 0.2;
const MAX_ZOOM: f32 = 3.0;
const FRONTIER_PADDING: f32 = 72.0;

/// Board pan/zoom state with an explicit follow-frontier mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoardNavigation {
    pan_x: f32,
    pan_y: f32,
    zoom: f32,
    follow_frontier: bool,
}

impl Default for BoardNavigation {
    fn default() -> Self {
        Self {
            pan_x: 0.0,
            pan_y: 0.0,
            zoom: 1.0,
            follow_frontier: true,
        }
    }
}

impl BoardNavigation {
    #[must_use]
    pub const fn zoom(&self) -> f32 {
        self.zoom
    }

    #[must_use]
    pub const fn is_following(&self) -> bool {
        self.follow_frontier
    }

    /// Pans in world-space units and yields follow mode immediately.
    pub fn pan_by(&mut self, delta_x: f32, delta_y: f32) {
        if delta_x.abs() <= f32::EPSILON && delta_y.abs() <= f32::EPSILON {
            return;
        }
        self.follow_frontier = false;
        self.pan_x = (self.pan_x + delta_x).max(0.0);
        self.pan_y = (self.pan_y + delta_y).max(0.0);
    }

    /// Zooms around the viewport centre. Zooming preserves follow mode when active.
    pub fn zoom_by(&mut self, factor: f32, screen_width: f32, frontier_x: f32) {
        let old_zoom = self.zoom;
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        if self.follow_frontier {
            self.sync_frontier(frontier_x, screen_width);
            return;
        }
        let centre = self.pan_x + screen_width / (2.0 * old_zoom);
        self.pan_x = (centre - screen_width / (2.0 * self.zoom)).max(0.0);
    }

    pub fn follow(&mut self, frontier_x: f32, screen_width: f32) {
        self.follow_frontier = true;
        self.sync_frontier(frontier_x, screen_width);
    }

    /// Restores a readable 100% view while keeping the newest edge visible.
    pub fn reset(&mut self, frontier_x: f32, screen_width: f32) {
        *self = Self::default();
        self.sync_frontier(frontier_x, screen_width);
    }

    /// Centers one cited board item and pauses frontier following.
    pub fn focus_rect(&mut self, rect: super::BoardRect, screen_width: f32, screen_height: f32) {
        self.follow_frontier = false;
        self.pan_x = (rect.x + rect.width / 2.0 - screen_width / (2.0 * self.zoom)).max(0.0);
        self.pan_y = (rect.y + rect.height / 2.0 - screen_height / (2.0 * self.zoom)).max(0.0);
    }

    pub fn sync_frontier(&mut self, frontier_x: f32, screen_width: f32) {
        if !self.follow_frontier {
            return;
        }
        let world_width = screen_width / self.zoom;
        self.pan_x = (frontier_x + FRONTIER_PADDING - world_width).max(0.0);
    }

    #[must_use]
    pub fn viewport(&self, screen_width: f32, screen_height: f32) -> BoardViewport {
        BoardViewport {
            x: self.pan_x,
            y: self.pan_y,
            width: screen_width / self.zoom,
            height: screen_height / self.zoom,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BoardNavigation, MAX_ZOOM, MIN_ZOOM};

    #[test]
    fn panning_yields_follow_without_allowing_negative_world_coordinates() {
        let mut navigation = BoardNavigation::default();
        navigation.pan_by(-200.0, 40.0);

        let viewport = navigation.viewport(800.0, 600.0);
        assert!(
            !navigation.is_following(),
            "manual pan must yield frontier follow"
        );
        assert!(
            viewport.x.abs() <= f32::EPSILON,
            "the board cannot pan before its origin"
        );
        assert!(
            (viewport.y - 40.0).abs() <= f32::EPSILON,
            "vertical panning should remain available"
        );
    }

    #[test]
    fn follow_keeps_the_frontier_inside_the_viewport() {
        let mut navigation = BoardNavigation::default();
        navigation.follow(2_000.0, 800.0);

        let viewport = navigation.viewport(800.0, 600.0);
        assert!(
            viewport.x + viewport.width > 2_000.0,
            "frontier follow should retain calm padding beyond newest content"
        );
    }

    #[test]
    fn zoom_is_bounded_and_preserves_manual_viewport_centre() {
        let mut navigation = BoardNavigation::default();
        navigation.pan_by(400.0, 0.0);
        let centre_before = navigation.viewport(800.0, 600.0).x + 400.0;
        navigation.zoom_by(100.0, 800.0, 0.0);
        let viewport = navigation.viewport(800.0, 600.0);
        let centre_after = viewport.x + viewport.width / 2.0;

        assert!(
            (navigation.zoom() - MAX_ZOOM).abs() <= f32::EPSILON,
            "zoom should stop at its upper bound"
        );
        assert!(
            (centre_before - centre_after).abs() <= f32::EPSILON,
            "manual zoom should keep the same world-space centre"
        );

        navigation.zoom_by(0.000_1, 800.0, 0.0);
        assert!(
            (navigation.zoom() - MIN_ZOOM).abs() <= f32::EPSILON,
            "zoom should stop at its lower bound"
        );
    }

    #[test]
    fn reset_restores_readable_zoom_and_frontier_following() {
        let mut navigation = BoardNavigation::default();
        navigation.pan_by(400.0, 100.0);
        navigation.zoom_by(0.01, 800.0, 2_000.0);
        navigation.reset(2_000.0, 800.0);

        let viewport = navigation.viewport(800.0, 600.0);
        assert!(
            (navigation.zoom() - 1.0).abs() <= f32::EPSILON,
            "reset must restore the readable 100% zoom"
        );
        assert!(
            navigation.is_following(),
            "reset must resume live following"
        );
        assert!(
            viewport.x + viewport.width > 2_000.0,
            "reset must retain the newest board edge"
        );
        assert!(
            viewport.y.abs() <= f32::EPSILON,
            "reset must restore the top meeting lanes"
        );
    }

    #[test]
    fn cited_item_focus_centers_without_following_frontier() {
        let mut navigation = BoardNavigation::default();
        navigation.focus_rect(
            super::super::BoardRect {
                x: 1_000.0,
                y: 500.0,
                width: 200.0,
                height: 100.0,
            },
            800.0,
            600.0,
        );
        let viewport = navigation.viewport(800.0, 600.0);
        assert!(!navigation.is_following());
        assert!((viewport.x + viewport.width / 2.0 - 1_100.0).abs() <= f32::EPSILON);
        assert!((viewport.y + viewport.height / 2.0 - 550.0).abs() <= f32::EPSILON);
    }
}
