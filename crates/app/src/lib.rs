//! Shared, append-only scene model used by both spike lenses.

#![deny(warnings)]

use std::sync::Arc;

/// Stable identifier for an object in the spike scene.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SceneId(u64);

/// World-space rectangle whose placement never changes after insertion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneRect {
    /// Left edge in world coordinates.
    pub x: f32,
    /// Top edge in world coordinates.
    pub y: f32,
    /// Width in world coordinates.
    pub width: f32,
    /// Height in world coordinates.
    pub height: f32,
}

impl SceneRect {
    fn intersects(self, other: Self) -> bool {
        self.x < other.x + other.width
            && self.x + self.width > other.x
            && self.y < other.y + other.height
            && self.y + self.height > other.y
    }
}

/// Fake object kind used to exercise canvas rendering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SceneObjectKind {
    /// A transcript block.
    Utterance { text: Arc<str>, customer: bool },
    /// A suggestion anchored to another scene object.
    Suggestion { anchor: SceneId, text: Arc<str> },
}

/// An immutable positioned object.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneObject {
    /// Stable object identifier.
    pub id: SceneId,
    /// Placement assigned exactly once at append time.
    pub rect: SceneRect,
    /// Object payload.
    pub kind: SceneObjectKind,
}

/// View transform used independently by each lens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    /// World-space visible bounds.
    pub world: SceneRect,
    /// Display scale.
    pub zoom: f32,
}

/// The single scene shared by board and overlay lenses.
#[derive(Debug, Default)]
pub struct Scene {
    objects: Vec<SceneObject>,
    frontier_x: f32,
    next_id: u64,
}

impl Scene {
    const BLOCK_WIDTH: f32 = 280.0;
    const BLOCK_HEIGHT: f32 = 96.0;
    const BLOCK_GAP: f32 = 32.0;

    /// Appends an utterance at the frontier without touching existing objects.
    pub fn append_utterance(&mut self, text: Arc<str>, customer: bool) -> SceneId {
        let id = self.allocate_id();
        let rect = SceneRect {
            x: self.frontier_x,
            y: if customer { 180.0 } else { 48.0 },
            width: Self::BLOCK_WIDTH,
            height: Self::BLOCK_HEIGHT,
        };
        self.frontier_x += Self::BLOCK_WIDTH + Self::BLOCK_GAP;
        self.objects.push(SceneObject {
            id,
            rect,
            kind: SceneObjectKind::Utterance { text, customer },
        });
        id
    }

    /// Appends an anchored suggestion, with no layout pass over prior objects.
    pub fn append_suggestion(&mut self, anchor: SceneId, text: Arc<str>) -> Option<SceneId> {
        let anchor_rect = self.objects.iter().find(|object| object.id == anchor)?.rect;
        let id = self.allocate_id();
        self.objects.push(SceneObject {
            id,
            rect: SceneRect {
                x: anchor_rect.x + 32.0,
                y: anchor_rect.y + anchor_rect.height + 32.0,
                width: 240.0,
                height: 112.0,
            },
            kind: SceneObjectKind::Suggestion { anchor, text },
        });
        Some(id)
    }

    /// Replaces only the streaming suggestion payload; its placement remains stable.
    pub fn stream_suggestion(&mut self, id: SceneId, text: Arc<str>) -> bool {
        let Some(object) = self.objects.iter_mut().find(|object| object.id == id) else {
            return false;
        };
        let SceneObjectKind::Suggestion { anchor, .. } = object.kind else {
            return false;
        };
        object.kind = SceneObjectKind::Suggestion { anchor, text };
        true
    }

    /// Returns only visible objects. Rendering cost therefore follows viewport density,
    /// not accumulated session duration.
    pub fn visible(&self, viewport: Viewport) -> impl Iterator<Item = &SceneObject> {
        let first_candidate = self
            .objects
            .partition_point(|object| object.rect.x + object.rect.width <= viewport.world.x);
        let right = viewport.world.x + viewport.world.width;
        self.objects[first_candidate..]
            .iter()
            .take_while(move |object| object.rect.x < right)
            .filter(move |object| object.rect.intersects(viewport.world))
    }

    /// Number of accumulated objects.
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    /// Whether the scene has no objects.
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Newest world-space edge, used by the overlay viewport.
    pub fn frontier_x(&self) -> f32 {
        self.frontier_x
    }

    fn allocate_id(&mut self) -> SceneId {
        let id = SceneId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::{Scene, SceneObjectKind, SceneRect, Viewport};
    use std::sync::Arc;

    #[test]
    fn appending_never_reflows_existing_objects() {
        let mut scene = Scene::default();
        let first = scene.append_utterance(Arc::from("first"), false);
        let before = scene.objects[0].clone();

        for index in 0..10_000 {
            scene.append_utterance(Arc::from(format!("utterance {index}")), index % 2 == 0);
        }

        assert_eq!(
            scene.objects[0], before,
            "the first placement must remain immutable"
        );
        assert_eq!(
            scene.objects[0].id, first,
            "the first identifier must remain stable"
        );
    }

    #[test]
    fn culling_cost_is_bounded_by_viewport_density() {
        let mut scene = Scene::default();
        for index in 0..10_000 {
            scene.append_utterance(Arc::from(format!("utterance {index}")), index % 2 == 0);
        }
        let visible = scene
            .visible(Viewport {
                world: SceneRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1_000.0,
                    height: 500.0,
                },
                zoom: 1.0,
            })
            .count();

        assert!(
            visible <= 4,
            "a narrow viewport should expose only nearby blocks"
        );

        let frontier_visible = scene
            .visible(Viewport {
                world: SceneRect {
                    x: scene.frontier_x() - 1_000.0,
                    y: 0.0,
                    width: 1_000.0,
                    height: 500.0,
                },
                zoom: 1.0,
            })
            .count();
        assert!(
            frontier_visible <= 4,
            "frontier culling should remain bounded after a long session"
        );
    }

    #[test]
    fn suggestion_streaming_preserves_anchor_and_placement() {
        let mut scene = Scene::default();
        let anchor = scene.append_utterance(Arc::from("pricing"), true);
        let suggestion = scene
            .append_suggestion(anchor, Arc::from("Ask"))
            .ok_or("anchor should exist");
        assert!(suggestion.is_ok(), "suggestion should be appended");
        let suggestion = suggestion.unwrap_or(super::SceneId(u64::MAX));
        let rect = scene.objects[1].rect;

        assert!(
            scene.stream_suggestion(suggestion, Arc::from("Ask about budget")),
            "stream update should succeed"
        );
        assert_eq!(
            scene.objects[1].rect, rect,
            "streaming text must not move the card"
        );
        assert!(
            matches!(scene.objects[1].kind, SceneObjectKind::Suggestion { anchor: value, .. } if value == anchor),
            "anchor must survive streaming updates"
        );
    }
}
