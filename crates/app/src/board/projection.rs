//! Deterministic projection from the append-only timeline into stable board geometry.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use sotto_core::{
    Annotation, EventId, EventPayload, FrameRef, SessionId, Source, TimelineEvent, Utterance,
};

const TIME_SCALE: f32 = 64.0;
const TIME_ORIGIN_X: f32 = 48.0;
const REP_LANE_Y: f32 = 56.0;
const CUSTOMER_LANE_Y: f32 = 216.0;
const SCREEN_LANE_Y: f32 = 400.0;
const UTTERANCE_MIN_WIDTH: f32 = 72.0;
const UTTERANCE_MAX_WIDTH: f32 = 440.0;
const UTTERANCE_HEIGHT: f32 = 112.0;
const SNAPSHOT_MIN_WIDTH: f32 = 180.0;
const SNAPSHOT_MAX_WIDTH: f32 = 360.0;
const SNAPSHOT_HEIGHT: f32 = 212.0;

/// Session-qualified source identity for an object on the board.
///
/// `EventId` is monotonic only within one session, while the shared UI timeline may retain
/// several sessions over the application's lifetime.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BoardEventKey {
    pub session_id: SessionId,
    pub event_id: EventId,
}

impl BoardEventKey {
    #[must_use]
    pub const fn new(session_id: SessionId, event_id: EventId) -> Self {
        Self {
            session_id,
            event_id,
        }
    }

    fn for_event(event: &TimelineEvent) -> Self {
        Self::new(event.session_id(), event.id())
    }
}

/// Stable identity for an object on the board.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BoardItemId(u64);

impl BoardItemId {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Immutable world-space placement assigned when an object first appears.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoardRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl BoardRect {
    fn intersects(self, other: Self) -> bool {
        self.x < other.x + other.width
            && self.x + self.width > other.x
            && self.y < other.y + other.height
            && self.y + self.height > other.y
    }
}

/// World-space bounds currently visible through the board lens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoardViewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Transcript content rendered into one stable board card.
#[derive(Clone, Debug, PartialEq)]
pub struct UtteranceCard {
    pub source: Source,
    pub start: Duration,
    pub end: Duration,
    pub text: String,
    pub annotations: Vec<Annotation>,
    pub is_final: bool,
}

impl UtteranceCard {
    #[must_use]
    pub fn prosody_label(&self) -> String {
        self.annotations
            .iter()
            .map(Annotation::render_inline)
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

/// Local frame pinned to the interval in which it was visible.
///
/// Deliberately contains no reasoning state: displaying this frame never sends it or its OCR
/// text to a model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotCard {
    pub frame_ref: FrameRef,
    pub active_app: Option<String>,
    pub window_title: Option<String>,
    pub visible_from: Duration,
    pub visible_to: Option<Duration>,
}

/// The map-tier objects T016 renders. Proposals and derived regions remain later overlays.
#[derive(Clone, Debug, PartialEq)]
pub enum BoardItemKind {
    Utterance(UtteranceCard),
    Snapshot(SnapshotCard),
}

/// A positioned board object. Supersession may replace `kind`, but never `rect` or `id`.
#[derive(Clone, Debug, PartialEq)]
pub struct BoardItem {
    pub id: BoardItemId,
    pub current_event: BoardEventKey,
    /// Bounded body used for readable text/image rendering.
    pub rect: BoardRect,
    /// Actual timeline interval, retained even when the visual body is capped.
    pub temporal_rect: BoardRect,
    pub kind: BoardItemKind,
}

impl BoardItem {
    fn culling_rect(&self) -> BoardRect {
        BoardRect {
            width: self.rect.width.max(self.temporal_rect.width),
            ..self.rect
        }
    }

    #[must_use]
    pub fn body_intersects(&self, viewport: BoardViewport) -> bool {
        self.rect.intersects(BoardRect {
            x: viewport.x,
            y: viewport.y,
            width: viewport.width,
            height: viewport.height,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
struct IntervalNode {
    start: f32,
    end: f32,
    max_end: f32,
    item_index: usize,
    priority: u64,
    left: Option<Box<Self>>,
    right: Option<Box<Self>>,
}

impl IntervalNode {
    fn new(item_index: usize, item: &BoardItem) -> Self {
        let rect = item.culling_rect();
        let end = rect.x + rect.width;
        Self {
            start: rect.x,
            end,
            max_end: end,
            item_index,
            priority: interval_priority(item_index),
            left: None,
            right: None,
        }
    }

    fn insert(root: Option<Box<Self>>, node: Box<Self>) -> Box<Self> {
        let Some(mut root) = root else {
            return node;
        };
        let before = node.start.total_cmp(&root.start).is_lt()
            || (node.start == root.start && node.item_index < root.item_index);
        if before {
            root.left = Some(Self::insert(root.left.take(), node));
            if root
                .left
                .as_ref()
                .is_some_and(|left| left.priority > root.priority)
            {
                return Self::rotate_right(root);
            }
        } else {
            root.right = Some(Self::insert(root.right.take(), node));
            if root
                .right
                .as_ref()
                .is_some_and(|right| right.priority > root.priority)
            {
                return Self::rotate_left(root);
            }
        }
        root.recompute_max_end();
        root
    }

    fn remove(root: Option<Box<Self>>, start: f32, item_index: usize) -> Option<Box<Self>> {
        let mut root = root?;
        let ordering = start
            .total_cmp(&root.start)
            .then_with(|| item_index.cmp(&root.item_index));
        match ordering {
            std::cmp::Ordering::Less => {
                root.left = Self::remove(root.left.take(), start, item_index);
            }
            std::cmp::Ordering::Greater => {
                root.right = Self::remove(root.right.take(), start, item_index);
            }
            std::cmp::Ordering::Equal => return Self::merge(root.left.take(), root.right.take()),
        }
        root.recompute_max_end();
        Some(root)
    }

    fn merge(left: Option<Box<Self>>, right: Option<Box<Self>>) -> Option<Box<Self>> {
        match (left, right) {
            (None, right) => right,
            (left, None) => left,
            (Some(mut left), Some(mut right)) => {
                if left.priority > right.priority {
                    left.right = Self::merge(left.right.take(), Some(right));
                    left.recompute_max_end();
                    Some(left)
                } else {
                    right.left = Self::merge(Some(left), right.left.take());
                    right.recompute_max_end();
                    Some(right)
                }
            }
        }
    }

    fn rotate_left(mut root: Box<Self>) -> Box<Self> {
        let Some(mut next) = root.right.take() else {
            return root;
        };
        root.right = next.left.take();
        root.recompute_max_end();
        next.left = Some(root);
        next.recompute_max_end();
        next
    }

    fn rotate_right(mut root: Box<Self>) -> Box<Self> {
        let Some(mut next) = root.left.take() else {
            return root;
        };
        root.left = next.right.take();
        root.recompute_max_end();
        next.right = Some(root);
        next.recompute_max_end();
        next
    }

    fn recompute_max_end(&mut self) {
        self.max_end = self
            .left
            .as_ref()
            .map_or(self.end, |left| self.end.max(left.max_end));
        if let Some(right) = &self.right {
            self.max_end = self.max_end.max(right.max_end);
        }
    }

    fn collect_visible(&self, viewport: BoardRect, items: &[BoardItem], visible: &mut Vec<usize>) {
        if let Some(left) = &self.left
            && left.max_end > viewport.x
        {
            left.collect_visible(viewport, items, visible);
        }
        if self.start < viewport.x + viewport.width
            && self.end > viewport.x
            && items[self.item_index].culling_rect().intersects(viewport)
        {
            visible.push(self.item_index);
        }
        if self.start < viewport.x + viewport.width
            && let Some(right) = &self.right
        {
            right.collect_visible(viewport, items, visible);
        }
    }
}

fn interval_priority(item_index: usize) -> u64 {
    let mut value = item_index as u64 + 0x9e37_79b9_7f4a_7c15;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// Append-only, deterministic board projection shared by live and replay paths.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BoardProjection {
    items: Vec<BoardItem>,
    event_items: HashMap<BoardEventKey, usize>,
    seen_events: HashSet<BoardEventKey>,
    interval_root: Option<Box<IntervalNode>>,
    frontier_x: f32,
    next_id: u64,
}

impl BoardProjection {
    /// Rebuilds the same projection used live from a persisted timeline.
    #[must_use]
    pub fn from_events(events: &[TimelineEvent]) -> Self {
        let mut projection = Self::default();
        projection.extend(events);
        projection
    }

    /// Consumes newly appended timeline events. Events already seen are ignored.
    pub fn extend(&mut self, events: &[TimelineEvent]) {
        for event in events {
            self.apply(event);
        }
    }

    /// Projects one event without revisiting placement assigned to prior events.
    pub fn apply(&mut self, event: &TimelineEvent) {
        let event_key = BoardEventKey::for_event(event);
        if !self.seen_events.insert(event_key) {
            return;
        }

        let kind = match event.payload() {
            EventPayload::UtterancePartial(utterance) => {
                Some(BoardItemKind::Utterance(utterance_card(utterance, false)))
            }
            EventPayload::UtteranceFinal(utterance) => {
                Some(BoardItemKind::Utterance(utterance_card(utterance, true)))
            }
            EventPayload::ScreenSnapshot(snapshot) => Some(BoardItemKind::Snapshot(SnapshotCard {
                frame_ref: snapshot.frame_ref.clone(),
                active_app: snapshot.active_app.clone(),
                window_title: snapshot.window_title.clone(),
                visible_from: snapshot.visible_from,
                visible_to: snapshot.visible_to,
            })),
            EventPayload::Vad(_)
            | EventPayload::Prosody(_)
            | EventPayload::Proposal(_)
            | EventPayload::ProposalDisposition(_)
            | EventPayload::ProposalRunAudit(_)
            | EventPayload::UserAnnotation(_)
            | EventPayload::Error(_) => None,
        };
        let Some(kind) = kind else {
            return;
        };

        if let Some(index) = event.supersedes().and_then(|superseded| {
            self.event_items
                .get(&BoardEventKey::new(event.session_id(), superseded))
                .copied()
        }) {
            let interval_start = self.items[index].culling_rect().x;
            self.interval_root =
                IntervalNode::remove(self.interval_root.take(), interval_start, index);
            let (_, replacement_temporal_rect) = rects_for(&kind);
            let stable_rect = self.items[index].rect;
            self.items[index].current_event = event_key;
            self.items[index].temporal_rect = BoardRect {
                width: replacement_temporal_rect.width,
                ..stable_rect
            };
            self.items[index].kind = kind;
            self.event_items.insert(event_key, index);
            let node = Box::new(IntervalNode::new(index, &self.items[index]));
            self.interval_root = Some(IntervalNode::insert(self.interval_root.take(), node));
            let culling_rect = self.items[index].culling_rect();
            self.frontier_x = self.frontier_x.max(culling_rect.x + culling_rect.width);
            return;
        }

        let (rect, temporal_rect) = rects_for(&kind);
        let index = self.items.len();
        let item = BoardItem {
            id: BoardItemId(self.next_id),
            current_event: event_key,
            rect,
            temporal_rect,
            kind,
        };
        self.next_id = self.next_id.saturating_add(1);
        let culling_rect = item.culling_rect();
        self.frontier_x = self.frontier_x.max(culling_rect.x + culling_rect.width);
        self.items.push(item);
        self.event_items.insert(event_key, index);
        let node = Box::new(IntervalNode::new(index, &self.items[index]));
        self.interval_root = Some(IntervalNode::insert(self.interval_root.take(), node));
    }

    #[must_use]
    pub fn items(&self) -> &[BoardItem] {
        &self.items
    }

    #[must_use]
    pub fn item_for_event(&self, key: BoardEventKey) -> Option<&BoardItem> {
        self.event_items
            .get(&key)
            .and_then(|index| self.items.get(*index))
    }

    #[must_use]
    pub fn frontier_x(&self) -> f32 {
        self.frontier_x
    }

    /// Returns only viewport-adjacent objects, so rendering cost follows visible density rather
    /// than total call duration.
    pub fn visible(&self, viewport: BoardViewport) -> impl Iterator<Item = &BoardItem> {
        let world = BoardRect {
            x: viewport.x,
            y: viewport.y,
            width: viewport.width,
            height: viewport.height,
        };
        let mut visible = Vec::new();
        if let Some(root) = &self.interval_root {
            root.collect_visible(world, &self.items, &mut visible);
        }
        visible.into_iter().map(|index| &self.items[index])
    }
}

fn utterance_card(utterance: &Utterance, is_final: bool) -> UtteranceCard {
    UtteranceCard {
        source: utterance.source,
        start: utterance.start,
        end: utterance.end,
        text: utterance.text.clone(),
        annotations: utterance.annotations.clone(),
        is_final,
    }
}

fn rects_for(kind: &BoardItemKind) -> (BoardRect, BoardRect) {
    match kind {
        BoardItemKind::Utterance(card) => {
            let duration = card.end.saturating_sub(card.start).as_secs_f32();
            let rect = BoardRect {
                x: timeline_x(card.start),
                y: match card.source {
                    Source::Mic => REP_LANE_Y,
                    Source::System => CUSTOMER_LANE_Y,
                },
                width: (duration * TIME_SCALE).clamp(UTTERANCE_MIN_WIDTH, UTTERANCE_MAX_WIDTH),
                height: UTTERANCE_HEIGHT,
            };
            let temporal_rect = BoardRect {
                width: (duration * TIME_SCALE).max(1.0),
                ..rect
            };
            (rect, temporal_rect)
        }
        BoardItemKind::Snapshot(card) => {
            let duration = card
                .visible_to
                .map(|end| end.saturating_sub(card.visible_from).as_secs_f32())
                .unwrap_or(5.0);
            let rect = BoardRect {
                x: timeline_x(card.visible_from),
                y: SCREEN_LANE_Y,
                width: (duration * TIME_SCALE).clamp(SNAPSHOT_MIN_WIDTH, SNAPSHOT_MAX_WIDTH),
                height: SNAPSHOT_HEIGHT,
            };
            let temporal_rect = BoardRect {
                width: (duration * TIME_SCALE).max(1.0),
                ..rect
            };
            (rect, temporal_rect)
        }
    }
}

fn timeline_x(timestamp: Duration) -> f32 {
    TIME_ORIGIN_X + timestamp.as_secs_f32() * TIME_SCALE
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sotto_core::{
        Annotation, CaptureTarget, EventPayload, FrameRef, ScreenSnapshot, Session, SessionId,
        Source, TargetKind, TimelineBuilder, Utterance,
    };

    use super::{BoardEventKey, BoardItemKind, BoardProjection, BoardViewport, timeline_x};

    fn builder() -> TimelineBuilder {
        builder_for(16)
    }

    fn builder_for(session_id: u128) -> TimelineBuilder {
        TimelineBuilder::new(Session::new(
            SessionId::new(session_id),
            CaptureTarget {
                bundle_id: Some("us.zoom.xos".to_owned()),
                display_name: "Zoom".to_owned(),
                window_title: Some("Acme".to_owned()),
                kind: TargetKind::Window,
                audio_scoped: true,
            },
            0,
        ))
    }

    fn utterance(source: Source, start: u64, end: u64, text: &str) -> Utterance {
        Utterance {
            source,
            start: Duration::from_secs(start),
            end: Duration::from_secs(end),
            text: text.to_owned(),
            avg_logprob: -0.1,
            annotations: vec![Annotation::SpeechRate(130.0)],
        }
    }

    #[test]
    fn final_supersession_changes_text_without_moving_the_card()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut timeline = builder();
        let partial = timeline.append(
            Duration::from_secs(1),
            EventPayload::UtterancePartial(utterance(Source::System, 1, 2, "price")),
        );
        let final_event = timeline.supersede(
            Duration::from_secs(2),
            EventPayload::UtteranceFinal(utterance(
                Source::System,
                1,
                4,
                "the price is above budget",
            )),
            &partial,
        )?;
        let mut projection = BoardProjection::default();
        projection.apply(&partial);
        let before = projection.items()[0].rect;
        let temporal_before = projection.items()[0].temporal_rect;
        projection.apply(&final_event);

        assert_eq!(
            projection.items().len(),
            1,
            "supersession must not append a second card"
        );
        assert_eq!(
            projection.items()[0].rect,
            before,
            "supersession must preserve placement"
        );
        assert!(
            projection.items()[0].temporal_rect.width > temporal_before.width,
            "the stable card body must remain distinct from the corrected real interval"
        );
        assert_eq!(
            projection.items()[0].temporal_rect.x + projection.items()[0].temporal_rect.width,
            timeline_x(Duration::from_secs(4)),
            "the corrected interval must reach the final utterance end"
        );
        assert_eq!(
            projection.item_for_event(BoardEventKey::new(partial.session_id(), partial.id())),
            projection.item_for_event(BoardEventKey::new(
                final_event.session_id(),
                final_event.id(),
            )),
            "both event ids must resolve to the same stable card"
        );
        let BoardItemKind::Utterance(card) = &projection.items()[0].kind else {
            return Err("expected an utterance card".into());
        };
        assert_eq!(
            card.text, "the price is above budget",
            "the final text must be visible"
        );
        assert!(
            card.is_final,
            "the projected replacement must carry finality"
        );
        Ok(())
    }

    #[test]
    fn time_axis_makes_pauses_gaps_and_interruptions_overlap() {
        let mut timeline = builder();
        let rep = timeline.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance(Source::Mic, 0, 3, "Let me explain")),
        );
        let customer = timeline.append(
            Duration::from_secs(2),
            EventPayload::UtteranceFinal(utterance(Source::System, 2, 4, "But what about cost?")),
        );
        let later = timeline.append(
            Duration::from_secs(8),
            EventPayload::UtteranceFinal(utterance(Source::Mic, 8, 9, "Good question")),
        );
        let projection =
            BoardProjection::from_events(&[rep.clone(), customer.clone(), later.clone()]);
        let rep_rect = projection
            .item_for_event(BoardEventKey::new(rep.session_id(), rep.id()))
            .map(|item| item.temporal_rect);
        let customer_rect = projection
            .item_for_event(BoardEventKey::new(customer.session_id(), customer.id()))
            .map(|item| item.temporal_rect);
        let later_rect = projection
            .item_for_event(BoardEventKey::new(later.session_id(), later.id()))
            .map(|item| item.temporal_rect);

        assert!(
            rep_rect.zip(customer_rect).is_some_and(|(left, right)| {
                left.x < right.x + right.width && left.x + left.width > right.x
            }),
            "overlapping speech must overlap on the horizontal time axis"
        );
        assert!(
            rep_rect
                .zip(later_rect)
                .is_some_and(|(left, right)| left.x + left.width < right.x),
            "a long pause must remain literal empty space"
        );
    }

    #[test]
    fn screen_frame_is_local_board_content_independent_of_ocr()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut timeline = builder();
        let event = timeline.append(
            Duration::from_secs(10),
            EventPayload::ScreenSnapshot(ScreenSnapshot {
                frame_ref: FrameRef::new("/tmp/local-frame.png"),
                ocr_text: "must not enter the board card".to_owned(),
                active_app: Some("Keynote".to_owned()),
                window_title: Some("Pricing".to_owned()),
                visible_from: Duration::from_secs(5),
                visible_to: Some(Duration::from_secs(15)),
            }),
        );
        let projection = BoardProjection::from_events(std::slice::from_ref(&event));
        let Some(item) =
            projection.item_for_event(BoardEventKey::new(event.session_id(), event.id()))
        else {
            return Err("expected the screen event to produce a board item".into());
        };
        let BoardItemKind::Snapshot(card) = &item.kind else {
            return Err("expected the board item to be a screen snapshot".into());
        };

        assert_eq!(
            card.frame_ref.as_str(),
            "/tmp/local-frame.png",
            "the local frame must remain addressable"
        );
        assert_eq!(
            card.window_title.as_deref(),
            Some("Pricing"),
            "capture metadata should label the frame"
        );
        Ok(())
    }

    #[test]
    fn session_local_event_ids_do_not_alias_on_the_shared_timeline() {
        let mut first = builder_for(16);
        let mut second = builder_for(17);
        let first_event = first.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance(Source::Mic, 0, 1, "first session")),
        );
        let second_event = second.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance(Source::Mic, 0, 1, "second session")),
        );
        assert_eq!(
            first_event.id(),
            second_event.id(),
            "the regression requires the ordinary cross-session id collision"
        );

        let projection = BoardProjection::from_events(&[first_event.clone(), second_event.clone()]);
        let first_item = projection.item_for_event(BoardEventKey::new(
            first_event.session_id(),
            first_event.id(),
        ));
        let second_item = projection.item_for_event(BoardEventKey::new(
            second_event.session_id(),
            second_event.id(),
        ));

        assert_eq!(projection.items().len(), 2, "both sessions must survive");
        assert!(
            first_item.zip(second_item).is_some_and(|(first, second)| {
                first.id != second.id
                    && first.current_event.session_id != second.current_event.session_id
            }),
            "board identity and source identity must remain distinct across sessions"
        );
    }

    #[test]
    fn long_utterance_extent_remains_visible_after_the_card_body_ends()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut timeline = builder();
        let event = timeline.append(
            Duration::ZERO,
            EventPayload::UtteranceFinal(utterance(Source::System, 10, 40, "long answer")),
        );
        let projection = BoardProjection::from_events(std::slice::from_ref(&event));
        let key = BoardEventKey::new(event.session_id(), event.id());
        let item = projection
            .item_for_event(key)
            .ok_or("fixture item must exist")?;

        assert_eq!(
            item.temporal_rect.x,
            timeline_x(Duration::from_secs(10)),
            "the interval must remain anchored to the utterance start"
        );
        assert_eq!(
            item.temporal_rect.x + item.temporal_rect.width,
            timeline_x(Duration::from_secs(40)),
            "the interval must span the uncapped utterance duration"
        );
        assert!(
            item.temporal_rect.width > item.rect.width,
            "the readable body may remain capped independently"
        );
        assert_eq!(
            projection
                .visible(BoardViewport {
                    x: timeline_x(Duration::from_secs(30)),
                    y: 0.0,
                    width: 100.0,
                    height: 380.0,
                })
                .map(|visible| visible.current_event)
                .collect::<Vec<_>>(),
            vec![key],
            "culling must retain an active long utterance after its body width is exhausted"
        );
        Ok(())
    }

    #[test]
    fn long_snapshot_extent_remains_pinned_across_its_visible_interval()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut timeline = builder();
        let event = timeline.append(
            Duration::from_secs(120),
            EventPayload::ScreenSnapshot(ScreenSnapshot {
                frame_ref: FrameRef::new("/tmp/long-frame.png"),
                ocr_text: String::new(),
                active_app: Some("Keynote".to_owned()),
                window_title: Some("Roadmap".to_owned()),
                visible_from: Duration::from_secs(20),
                visible_to: Some(Duration::from_secs(120)),
            }),
        );
        let projection = BoardProjection::from_events(std::slice::from_ref(&event));
        let key = BoardEventKey::new(event.session_id(), event.id());
        let item = projection
            .item_for_event(key)
            .ok_or("fixture item must exist")?;

        assert_eq!(
            item.temporal_rect.x,
            timeline_x(Duration::from_secs(20)),
            "the snapshot interval must anchor at visible_from"
        );
        assert_eq!(
            item.temporal_rect.x + item.temporal_rect.width,
            timeline_x(Duration::from_secs(120)),
            "the snapshot interval must reach visible_to without a visual-width cap"
        );
        assert_eq!(
            projection
                .visible(BoardViewport {
                    x: timeline_x(Duration::from_secs(90)),
                    y: 380.0,
                    width: 100.0,
                    height: 260.0,
                })
                .map(|visible| visible.current_event)
                .collect::<Vec<_>>(),
            vec![key],
            "the retained frame must remain pinned throughout its real visible interval"
        );
        assert!(
            !item.body_intersects(BoardViewport {
                x: timeline_x(Duration::from_secs(90)),
                y: 380.0,
                width: 100.0,
                height: 260.0,
            }),
            "temporal visibility must not make an off-screen thumbnail body resident"
        );
        Ok(())
    }

    #[test]
    fn live_append_and_persisted_replay_produce_identical_geometry()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut timeline = builder();
        let partial = timeline.append(
            Duration::ZERO,
            EventPayload::UtterancePartial(utterance(Source::Mic, 0, 1, "hello")),
        );
        let final_event = timeline.supersede(
            Duration::from_secs(1),
            EventPayload::UtteranceFinal(utterance(Source::Mic, 0, 2, "hello there")),
            &partial,
        )?;
        let customer = timeline.append(
            Duration::from_secs(3),
            EventPayload::UtteranceFinal(utterance(Source::System, 3, 5, "hi")),
        );
        let events = vec![partial, final_event, customer];
        let mut live = BoardProjection::default();
        for event in &events {
            live.apply(event);
        }
        let replay = BoardProjection::from_events(&events);

        assert_eq!(
            live, replay,
            "live append and persisted replay must be the same projection"
        );
        Ok(())
    }

    #[test]
    fn viewport_culling_stays_bounded_after_a_long_timeline() {
        let mut timeline = builder();
        let mut projection = BoardProjection::default();
        for second in 0..10_000_u64 {
            let event = timeline.append(
                Duration::from_secs(second),
                EventPayload::UtteranceFinal(utterance(
                    if second.is_multiple_of(2) {
                        Source::Mic
                    } else {
                        Source::System
                    },
                    second,
                    second.saturating_add(1),
                    "fixture utterance",
                )),
            );
            projection.apply(&event);
        }
        let visible = projection
            .visible(BoardViewport {
                x: projection.frontier_x() - 1_200.0,
                y: 0.0,
                width: 1_200.0,
                height: 700.0,
            })
            .count();

        assert!(
            visible > 0,
            "the frontier viewport must include known recent events"
        );
        assert!(
            visible <= 30,
            "visible work must follow viewport density, not total events"
        );
    }
}
