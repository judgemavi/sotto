//! Executable harness for the GPUI canvas spike.

#![deny(warnings)]

use app::{Scene, SceneObjectKind, SceneRect, Viewport};
use gpui::{
    App, Application, Bounds, Context, Entity, Render, Timer, Window, WindowBounds, WindowKind,
    WindowOptions, div, prelude::*, px, rgb, size,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

#[cfg(target_os = "macos")]
fn set_click_through(window: &Window, enabled: bool) {
    use objc2::{msg_send, runtime::AnyObject};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return;
    };
    let view = handle.ns_view.as_ptr().cast::<AnyObject>();
    // SAFETY: GPUI owns this live NSView for the duration of `window`; AppKit's `window`
    // selector returns its owning NSWindow and `setIgnoresMouseEvents:` accepts BOOL.
    unsafe {
        let native_window: *mut AnyObject = msg_send![view, window];
        if !native_window.is_null() {
            let _: () = msg_send![native_window, setIgnoresMouseEvents: enabled];
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn set_click_through(_window: &Window, _enabled: bool) {}

enum FakeEvent {
    Utterance { text: Arc<str>, customer: bool },
    SuggestionStart,
    SuggestionText(Arc<str>),
}

struct Lens {
    scene: Entity<Scene>,
    overlay: bool,
    pan_x: f32,
    pan_y: f32,
    zoom: f32,
    started: Instant,
    last_frame: Option<Instant>,
    frame_intervals: Vec<Duration>,
    report_index: usize,
}

impl Render for Lens {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.request_animation_frame();
        let now = Instant::now();
        if !self.overlay {
            if let Some(last_frame) = self.last_frame {
                self.frame_intervals
                    .push(now.saturating_duration_since(last_frame));
            }
            self.last_frame = Some(now);
            const REPORT_SECONDS: [u64; 4] = [60, 600, 1_200, 1_800];
            if let Some(report_at) = REPORT_SECONDS.get(self.report_index)
                && now.duration_since(self.started).as_secs() >= *report_at
            {
                let mut sorted = self.frame_intervals.clone();
                sorted.sort_unstable();
                let p50 = sorted.get(sorted.len() / 2).copied().unwrap_or_default();
                let p95 = sorted
                    .get(sorted.len().saturating_mul(95) / 100)
                    .copied()
                    .unwrap_or_default();
                eprintln!(
                    "T020_FRAME interval_end_s={report_at} samples={} p50_ms={:.3} p95_ms={:.3}",
                    sorted.len(),
                    p50.as_secs_f64() * 1_000.0,
                    p95.as_secs_f64() * 1_000.0
                );
                self.frame_intervals.clear();
                self.report_index = self.report_index.saturating_add(1);
            }
        }
        let bounds = window.bounds();
        let width: f32 = bounds.size.width.into();
        let height: f32 = bounds.size.height.into();
        let scene = self.scene.read(cx);
        let left = if self.overlay {
            (scene.frontier_x() - (width / self.zoom)).max(0.0)
        } else {
            self.pan_x
        };
        let viewport = Viewport {
            world: SceneRect {
                x: left,
                y: self.pan_y,
                width: width / self.zoom,
                height: height / self.zoom,
            },
            zoom: self.zoom,
        };
        let children = scene.visible(viewport).map(|object| {
            let (label, color) = match &object.kind {
                SceneObjectKind::Utterance { text, customer } => (
                    text.to_string(),
                    if *customer {
                        rgb(0x183a5a)
                    } else {
                        rgb(0x315237)
                    },
                ),
                SceneObjectKind::Suggestion { text, .. } => (text.to_string(), rgb(0x604b19)),
            };
            div()
                .absolute()
                .left(px((object.rect.x - left) * self.zoom))
                .top(px((object.rect.y - self.pan_y) * self.zoom))
                .w(px(object.rect.width * self.zoom))
                .h(px(object.rect.height * self.zoom))
                .p_3()
                .rounded_lg()
                .bg(color)
                .text_color(rgb(0xf4f4f4))
                .child(label)
        });

        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(rgb(0x111318))
            .on_scroll_wheel(cx.listener(|lens, event: &gpui::ScrollWheelEvent, _, cx| {
                let delta = event.delta.pixel_delta(px(20.0));
                let dx: f32 = delta.x.into();
                let dy: f32 = delta.y.into();
                if event.modifiers.control {
                    lens.zoom = (lens.zoom * (1.0 - (dy * 0.005))).clamp(0.15, 3.0);
                } else {
                    lens.pan_x = (lens.pan_x - dx).max(0.0);
                    lens.pan_y = (lens.pan_y - dy).max(0.0);
                }
                cx.notify();
            }))
            .children(children)
    }
}

fn start_fake_pipeline() -> mpsc::Receiver<FakeEvent> {
    let (sender, receiver) = mpsc::channel(256);
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build();
        let Ok(runtime) = runtime else {
            return;
        };
        runtime.block_on(async move {
            let mut index = 0_u64;
            loop {
                let event = FakeEvent::Utterance {
                    text: Arc::from(format!(
                        "{}: fake live utterance {index}",
                        if index.is_multiple_of(2) {
                            "customer"
                        } else {
                            "rep"
                        }
                    )),
                    customer: index.is_multiple_of(2),
                };
                if sender.send(event).await.is_err() {
                    break;
                }
                if index % 12 == 5 {
                    if sender.send(FakeEvent::SuggestionStart).await.is_err() {
                        break;
                    }
                    let words = [
                        "Ask",
                        "Ask about",
                        "Ask about the",
                        "Ask about the impact",
                        "Ask about the impact on their workflow.",
                    ];
                    for text in words {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        if sender
                            .send(FakeEvent::SuggestionText(Arc::from(text)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                index = index.saturating_add(1);
                tokio::time::sleep(Duration::from_millis(16)).await;
            }
        });
    });
    receiver
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let scene = cx.new(|_| Scene::default());
        let mut receiver = start_fake_pipeline();
        let drain_scene = scene.clone();
        cx.spawn(async move |cx| {
            let mut newest_utterance = None;
            let mut streaming_suggestion = None;
            loop {
                Timer::after(Duration::from_millis(16)).await;
                while let Ok(event) = receiver.try_recv() {
                    let update = cx.update(|cx| {
                        drain_scene.update(cx, |scene, cx| {
                            match event {
                                FakeEvent::Utterance { text, customer } => {
                                    newest_utterance = Some(scene.append_utterance(text, customer));
                                }
                                FakeEvent::SuggestionStart => {
                                    streaming_suggestion = newest_utterance.and_then(|anchor| {
                                        scene.append_suggestion(anchor, Arc::from(""))
                                    });
                                }
                                FakeEvent::SuggestionText(text) => {
                                    if let Some(suggestion) = streaming_suggestion {
                                        scene.stream_suggestion(suggestion, text);
                                    }
                                }
                            }
                            cx.notify();
                        });
                    });
                    if update.is_err() {
                        return;
                    }
                }
            }
        })
        .detach();

        let board_bounds = Bounds::centered(None, size(px(1_200.0), px(700.0)), cx);
        let board_scene = scene.clone();
        if cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(board_bounds)),
                    ..Default::default()
                },
                move |_, cx| {
                    cx.new(|_| Lens {
                        scene: board_scene,
                        overlay: false,
                        pan_x: 0.0,
                        pan_y: 0.0,
                        zoom: 1.0,
                        started: Instant::now(),
                        last_frame: None,
                        frame_intervals: Vec::new(),
                        report_index: 0,
                    })
                },
            )
            .is_err()
        {
            cx.quit();
            return;
        }

        let overlay_bounds = Bounds::centered(None, size(px(520.0), px(320.0)), cx);
        let overlay = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(overlay_bounds)),
                titlebar: None,
                focus: false,
                kind: WindowKind::PopUp,
                is_resizable: false,
                is_minimizable: false,
                ..Default::default()
            },
            move |_, cx| {
                cx.new(|_| Lens {
                    scene,
                    overlay: true,
                    pan_x: 0.0,
                    pan_y: 0.0,
                    zoom: 1.0,
                    started: Instant::now(),
                    last_frame: None,
                    frame_intervals: Vec::new(),
                    report_index: 0,
                })
            },
        );
        let Ok(overlay) = overlay else {
            cx.quit();
            return;
        };
        cx.spawn(async move |cx| {
            let mut passive = false;
            loop {
                Timer::after(Duration::from_secs(5)).await;
                passive = !passive;
                if overlay
                    .update(cx, |_, window, _| set_click_through(window, passive))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
        cx.activate(true);
    });
}
