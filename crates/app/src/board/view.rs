//! GPUI board lens over stable projection geometry.

use std::path::PathBuf;

use gpui::{
    Context, Entity, ImgResourceLoader, IntoElement, ObjectFit, Render, Resource, Subscription,
    Window, div, img, prelude::*, px, rgb,
};
use gpui_component::button::Button;
use sotto_core::{EventId, SessionId, Source};

use crate::devwindow::TimelineState;

use super::{
    BoardItem, BoardItemKind, BoardNavigation, BoardState, BoardViewport,
    metrics::{BoardMetrics, BoardMetricsObservation},
    thumbnails::ThumbnailResidency,
};

/// Live and post-call board renderer. It observes the one shared timeline projection.
pub struct BoardCanvas {
    live_timeline: Entity<TimelineState>,
    board: Entity<BoardState>,
    navigation: BoardNavigation,
    thumbnails: ThumbnailResidency,
    metrics: BoardMetrics,
    _board_subscription: Subscription,
}

impl BoardCanvas {
    #[must_use]
    pub fn from_timeline(timeline: Entity<TimelineState>, cx: &mut Context<Self>) -> Self {
        let board = cx.new(|cx| BoardState::new(timeline.clone(), cx));
        let board_subscription = cx.observe(&board, |_, _, cx| cx.notify());
        Self {
            live_timeline: timeline,
            board,
            navigation: BoardNavigation::default(),
            thumbnails: ThumbnailResidency::default(),
            metrics: BoardMetrics::from_environment(),
            _board_subscription: board_subscription,
        }
    }

    pub fn show_session(&mut self, events: &[sotto_core::TimelineEvent], cx: &mut Context<Self>) {
        let board = cx.new(|_| BoardState::from_events(events));
        self._board_subscription = cx.observe(&board, |_, _, cx| cx.notify());
        self.board = board;
        self.navigation = BoardNavigation::default();
        self.thumbnails = ThumbnailResidency::default();
        cx.notify();
    }

    pub fn show_live_session(&mut self, session_id: SessionId, cx: &mut Context<Self>) {
        let board =
            cx.new(|cx| BoardState::for_live_session(self.live_timeline.clone(), session_id, cx));
        self._board_subscription = cx.observe(&board, |_, _, cx| cx.notify());
        self.board = board;
        self.navigation = BoardNavigation::default();
        self.thumbnails = ThumbnailResidency::default();
        cx.notify();
    }

    /// Navigates to a cited event only when it resolves to a stable item in the selected session.
    pub fn focus_event(
        &mut self,
        session_id: SessionId,
        event_id: EventId,
        screen_width: f32,
        screen_height: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        let rect = self
            .board
            .read(cx)
            .projection()
            .item_for_event(super::BoardEventKey::new(session_id, event_id))
            .map(|item| item.rect);
        let Some(rect) = rect else {
            return false;
        };
        self.navigation
            .focus_rect(rect, screen_width, screen_height);
        cx.notify();
        true
    }

    fn follow_frontier(&mut self, screen_width: f32, cx: &mut Context<Self>) {
        let frontier = self.board.read(cx).projection().frontier_x();
        self.navigation.follow(frontier, screen_width);
        cx.notify();
    }

    fn zoom_by(&mut self, factor: f32, screen_width: f32, frontier: f32, cx: &mut Context<Self>) {
        self.navigation.zoom_by(factor, screen_width, frontier);
        cx.notify();
    }

    fn reset_view(&mut self, screen_width: f32, frontier: f32, cx: &mut Context<Self>) {
        self.navigation.reset(frontier, screen_width);
        cx.notify();
    }

    fn evict_offscreen_thumbnails(
        &mut self,
        visible_items: &[BoardItem],
        viewport: BoardViewport,
        cx: &mut Context<Self>,
    ) {
        let visible_paths = visible_items.iter().filter_map(|item| {
            if !item.body_intersects(viewport) {
                return None;
            }
            match &item.kind {
                BoardItemKind::Snapshot(card) => Some(PathBuf::from(card.frame_ref.as_str())),
                BoardItemKind::Utterance(_) => None,
            }
        });
        for path in self.thumbnails.reconcile(visible_paths) {
            let resource: Resource = path.into();
            cx.remove_asset::<ImgResourceLoader>(&resource);
        }
    }
}

impl Render for BoardCanvas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.board.update(cx, |board, cx| {
            let _ = board.refresh(cx);
        });

        let bounds = window.bounds();
        let screen_width: f32 = bounds.size.width.into();
        let screen_height: f32 = bounds.size.height.into();
        let frontier = self.board.read(cx).projection().frontier_x();
        self.navigation.sync_frontier(frontier, screen_width);
        let viewport = self.navigation.viewport(screen_width, screen_height);
        let visible_items: Vec<_> = self
            .board
            .read(cx)
            .projection()
            .visible(viewport)
            .cloned()
            .collect();
        self.evict_offscreen_thumbnails(&visible_items, viewport, cx);

        self.metrics.observe(BoardMetricsObservation {
            total_items: self.board.read(cx).projection().items().len(),
            visible_items: visible_items.len(),
            eligible_thumbnail_paths: self.thumbnails.eligible_path_count(),
            following_frontier: self.navigation.is_following(),
        });
        if self.metrics.is_running() {
            window.request_animation_frame();
        }

        let zoom = self.navigation.zoom();
        let children = visible_items
            .into_iter()
            .map(|item| render_item(item, viewport, zoom));
        let follow_label = if self.navigation.is_following() {
            "Following newest"
        } else {
            "Follow newest"
        };
        let zoom_label = format!("Zoom {:.0}%", zoom * 100.0);

        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(rgb(0x111318))
            .on_scroll_wheel(
                cx.listener(move |canvas, event: &gpui::ScrollWheelEvent, _, cx| {
                    let delta = event.delta.pixel_delta(px(20.0));
                    let delta_x: f32 = delta.x.into();
                    let delta_y: f32 = delta.y.into();
                    if event.modifiers.control {
                        canvas
                            .navigation
                            .zoom_by(1.0 - delta_y * 0.005, screen_width, frontier);
                    } else {
                        let zoom = canvas.navigation.zoom();
                        canvas.navigation.pan_by(-delta_x / zoom, -delta_y / zoom);
                    }
                    cx.notify();
                }),
            )
            .children(children)
            .child(
                div()
                    .absolute()
                    .top_3()
                    .right_3()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("board-zoom-out")
                            .label("Zoom out")
                            .on_click(cx.listener(move |canvas, _, _, cx| {
                                canvas.zoom_by(0.8, screen_width, frontier, cx);
                            })),
                    )
                    .child(
                        Button::new("board-reset-view")
                            .label(zoom_label)
                            .on_click(cx.listener(move |canvas, _, _, cx| {
                                canvas.reset_view(screen_width, frontier, cx);
                            })),
                    )
                    .child(
                        Button::new("board-zoom-in")
                            .label("Zoom in")
                            .on_click(cx.listener(move |canvas, _, _, cx| {
                                canvas.zoom_by(1.25, screen_width, frontier, cx);
                            })),
                    )
                    .child(
                        Button::new("board-follow-frontier")
                            .label(follow_label)
                            .on_click(cx.listener(move |canvas, _, _, cx| {
                                canvas.follow_frontier(screen_width, cx);
                            })),
                    ),
            )
    }
}

fn render_item(item: BoardItem, viewport: BoardViewport, zoom: f32) -> gpui::AnyElement {
    let body_visible = item.body_intersects(viewport);
    let rect = item.rect;
    let temporal_rect = item.temporal_rect;
    let shell_width = rect.width.max(temporal_rect.width);
    let shell = div()
        .absolute()
        .left(px((rect.x - viewport.x) * zoom))
        .top(px((rect.y - viewport.y) * zoom))
        .w(px(shell_width * zoom))
        .h(px(rect.height * zoom))
        .overflow_hidden();
    let card_body = div()
        .absolute()
        .left_0()
        .top_0()
        .w(px(rect.width * zoom))
        .h(px(rect.height * zoom))
        .overflow_hidden()
        .rounded_lg();

    match item.kind {
        BoardItemKind::Utterance(card) => {
            let speaker = match card.source {
                Source::Mic => "You",
                Source::System => "Meeting audio",
            };
            let color = match card.source {
                Source::Mic => rgb(0x315237),
                Source::System => rgb(0x183a5a),
            };
            let prosody = card.prosody_label();
            shell
                .when(body_visible, |shell| {
                    shell.child(
                        card_body
                            .p_3()
                            .bg(color)
                            .text_color(rgb(0xf4f4f4))
                            .child(div().text_sm().child(speaker))
                            .child(div().mt_1().child(card.text))
                            .when(!prosody.is_empty(), |view| {
                                view.child(
                                    div()
                                        .mt_2()
                                        .text_xs()
                                        .text_color(rgb(0xb8c8d8))
                                        .child(prosody),
                                )
                            }),
                    )
                })
                .child(interval_line(temporal_rect.width, zoom, color))
                .into_any_element()
        }
        BoardItemKind::Snapshot(card) => shell
            .when(body_visible, |shell| {
                let path = PathBuf::from(card.frame_ref.as_str());
                let label = card
                    .window_title
                    .or(card.active_app)
                    .unwrap_or_else(|| "Captured screen".to_owned());
                shell.child(
                    card_body
                        .flex()
                        .flex_col()
                        .bg(rgb(0x252a31))
                        .text_color(rgb(0xe6e8eb))
                        .child(div().px_3().py_2().text_sm().child(label))
                        .child(
                            img(path)
                                .object_fit(ObjectFit::Cover)
                                .w_full()
                                .flex_grow()
                                .with_loading(|| {
                                    thumbnail_placeholder("Loading frame…").into_any_element()
                                })
                                .with_fallback(|| {
                                    thumbnail_placeholder("Frame unavailable").into_any_element()
                                }),
                        ),
                )
            })
            .child(interval_line(temporal_rect.width, zoom, rgb(0x7b8794)))
            .into_any_element(),
    }
}

fn interval_line(width: f32, zoom: f32, color: gpui::Rgba) -> impl IntoElement {
    div()
        .absolute()
        .left_0()
        .bottom_0()
        .w(px(width * zoom))
        .h(px((3.0 * zoom).clamp(1.0, 4.0)))
        .bg(color)
}

fn thumbnail_placeholder(label: &'static str) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(0x1b1f25))
        .text_color(rgb(0x8e98a6))
        .child(label)
}
