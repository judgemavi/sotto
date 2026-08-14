//! Flat diagnostic timeline and the single Tokio-to-GPUI ingress seam.

mod seam;

#[cfg(test)]
pub(crate) use seam::test_ingress;
pub use seam::{TimelineIngress, TimelineState, attach_ingress};

use gpui::{
    Context, Entity, IntoElement, Render, ScrollStrategy, UniformListScrollHandle, Window, div,
    prelude::*, rgb, uniform_list,
};
use gpui_component::button::Button;
use sotto_core::{EventKind, EventPayload, TimelineEvent};
use std::time::{Duration, Instant};

pub struct DevTimeline {
    timeline: Entity<TimelineState>,
    filter: Option<EventKind>,
    auto_scroll: bool,
    scroll_handle: UniformListScrollHandle,
    validation_started: Instant,
    last_frame: Option<Instant>,
    frame_intervals: Vec<Duration>,
    report_index: usize,
    frame_validation: bool,
}

impl DevTimeline {
    #[must_use]
    pub fn new(timeline: Entity<TimelineState>) -> Self {
        Self {
            timeline,
            filter: None,
            auto_scroll: true,
            scroll_handle: UniformListScrollHandle::new(),
            validation_started: Instant::now(),
            last_frame: None,
            frame_intervals: Vec::new(),
            report_index: 0,
            frame_validation: std::env::var("SOTTO_FRAME_VALIDATION").as_deref() == Ok("1"),
        }
    }

    pub fn set_filter(&mut self, filter: Option<EventKind>, cx: &mut Context<Self>) {
        self.filter = filter;
        cx.notify();
    }

    fn choose_filter(&mut self, kind: Option<EventKind>, cx: &mut Context<Self>) {
        self.set_filter(kind, cx);
        self.auto_scroll = true;
    }

    fn record_frame_interval(&mut self) {
        const REPORT_SECONDS: [u64; 5] = [60, 600, 1_200, 1_800, 3_600];
        let Some(report_at) = REPORT_SECONDS.get(self.report_index) else {
            return;
        };
        let now = Instant::now();
        if let Some(last) = self.last_frame.replace(now) {
            self.frame_intervals
                .push(now.saturating_duration_since(last));
        }
        if now.duration_since(self.validation_started).as_secs() < *report_at {
            return;
        }
        let mut sorted = std::mem::take(&mut self.frame_intervals);
        sorted.sort_unstable();
        let p50 = sorted.get(sorted.len() / 2).copied().unwrap_or_default();
        let p95 = sorted
            .get(sorted.len().saturating_mul(95) / 100)
            .copied()
            .unwrap_or_default();
        let max = sorted.last().copied().unwrap_or_default();
        let over_budget = sorted
            .iter()
            .filter(|interval| **interval > Duration::from_micros(16_667))
            .count();
        eprintln!(
            "T012_FRAME interval_end_s={report_at} samples={} p50_ms={:.3} p95_ms={:.3} max_ms={:.3} over_budget={over_budget}",
            sorted.len(),
            p50.as_secs_f64() * 1_000.0,
            p95.as_secs_f64() * 1_000.0,
            max.as_secs_f64() * 1_000.0
        );
        self.report_index = self.report_index.saturating_add(1);
    }
}

impl Render for DevTimeline {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.frame_validation {
            window.request_animation_frame();
            self.record_frame_interval();
        }
        let state = self.timeline.read(cx);
        let rows: Vec<_> = state
            .events()
            .iter()
            .filter(|event| self.filter.is_none_or(|kind| event.kind() == kind))
            .cloned()
            .collect();
        if self.auto_scroll && !rows.is_empty() {
            self.scroll_handle
                .scroll_to_item(rows.len() - 1, ScrollStrategy::Bottom);
        }
        let status = if self.auto_scroll {
            "Following newest events"
        } else {
            "Auto-scroll paused"
        };
        div()
            .size_full()
            .bg(rgb(0x111318))
            .text_color(rgb(0xe6e8eb))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .p_2()
                    .child(
                        Button::new("filter-all")
                            .label("All")
                            .on_click(cx.listener(|this, _, _, cx| this.choose_filter(None, cx))),
                    )
                    .child(
                        Button::new("filter-partials")
                            .label("Partials")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.choose_filter(Some(EventKind::UtterancePartial), cx);
                            })),
                    )
                    .child(
                        Button::new("filter-finals")
                            .label("Finals")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.choose_filter(Some(EventKind::UtteranceFinal), cx);
                            })),
                    )
                    .child(Button::new("filter-errors").label("Errors").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.choose_filter(Some(EventKind::Error), cx)
                        }),
                    ))
                    .child(
                        Button::new("follow-newest")
                            .label(status)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.auto_scroll = true;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                uniform_list("timeline-events", rows.len(), move |range, _, _| {
                    range
                        .map(|index| {
                            div()
                                .id(("event", index))
                                .px_3()
                                .py_2()
                                .border_b_1()
                                .border_color(rgb(0x2b3038))
                                .child(render_event(&rows[index]))
                        })
                        .collect()
                })
                .track_scroll(self.scroll_handle.clone())
                .on_scroll_wheel(cx.listener(|this, _, _, cx| {
                    this.auto_scroll = false;
                    cx.notify();
                }))
                .h_full(),
            )
    }
}

fn render_event(event: &TimelineEvent) -> String {
    let supersedes = event
        .supersedes()
        .map(|id| format!(" supersedes=#{}", id.get()))
        .unwrap_or_default();
    let detail = match event.payload() {
        EventPayload::UtterancePartial(value) | EventPayload::UtteranceFinal(value) => {
            value.render_inline()
        }
        EventPayload::Vad(value) => format!("{} {:?}", value.source.speaker_name(), value.kind),
        EventPayload::Prosody(value) => format!(
            "{} talk={:.0}% {:?}",
            value.source.speaker_name(),
            value.talk_time_ratio * 100.0,
            value.annotations
        ),
        EventPayload::ScreenSnapshot(value) => format!(
            "screen={} window={} OCR={}",
            value.active_app.as_deref().unwrap_or("unknown"),
            value.window_title.as_deref().unwrap_or("unknown"),
            value.ocr_text
        ),
        other => format!("{other:?}"),
    };
    format!(
        "#{:<6} {:>9.3}s {:<20}{}  {}",
        event.id().get(),
        event.ts().as_secs_f64(),
        event.kind().as_str(),
        supersedes,
        detail
    )
}
