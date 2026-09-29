//! Product shell for the explicit, no-key map-session lifecycle.

#![deny(warnings)]

use app::{devwindow, mcp, reasoning, session, workspace};
use gpui_kit::component::{Root, TitleBar};
use gpui_kit::{App, Bounds, WindowBounds, WindowOptions, prelude::*, px, size};
use workspace::{KeyboardRoot, MeetingWorkspace};

fn main() {
    // Kit defaults plus the product Lucide icons Home / capture / mic / import embed
    // (ADR-0025). Default `Assets` alone omits those paths from the binary.
    gpui_kit::application()
        .with_assets(app::assets::AppAssets)
        .run(|cx: &mut App| {
            gpui_kit::init(cx);
            workspace::install_visible_scrollbars(cx);
            workspace::install_global_bindings(cx);
            let (timeline_ingress, timeline) = devwindow::attach_ingress(cx, 1_024);

            // Cold launch remains network-idle. The only model work is a background integrity check of
            // the persisted choice so Home can state whether transcription is ready before an action.
            let session_controller = cx.new(|cx| {
                let mut controller = session::SessionController::new(timeline_ingress);
                controller.begin_launch_model_check(cx);
                controller
            });
            let reasoning_controller = cx.new(|_| reasoning::ReasoningController::load_default());
            let mcp_controller = cx.new(|_| mcp::McpController::load_default());
            let quit_controller = session_controller.clone();
            cx.on_app_quit(move |cx| {
                quit_controller.update(cx, |controller, _| controller.shutdown_for_app_quit());
                async {}
            })
            .detach();

            let database = session::application_database_path();
            let workspace_reasoning = reasoning_controller;
            let workspace_session = session_controller.clone();
            let workspace_mcp = mcp_controller;
            let workspace_bounds = Bounds::centered(None, size(px(1_200.0), px(700.0)), cx);
            let close_controller = session_controller;
            let Ok(workspace_window) = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(workspace_bounds)),
                    // The kit owns the title bar, including its theme-resolved background,
                    // dragging behaviour, and macOS traffic-light placement. Sotto supplies
                    // only product actions as children of that stock component.
                    ..TitleBar::window_options()
                },
                move |window, cx| {
                    window.on_window_should_close(cx, move |_, cx| {
                        if close_controller
                            .read(cx)
                            .lifecycle()
                            .requires_visible_control()
                        {
                            close_controller.update(cx, |controller, cx| controller.stop(cx));
                            false
                        } else {
                            true
                        }
                    });
                    let view = cx.new(|cx| {
                        MeetingWorkspace::new(
                            database,
                            timeline,
                            workspace_reasoning,
                            workspace_session,
                            workspace_mcp,
                            window,
                            cx,
                        )
                    });
                    // KeyboardRoot keeps a non-tab-stop focus origin so the first Tab reaches a
                    // real control; it also paints Root's dialog/sheet/notification layers.
                    // Root owns dialog state, sheets, tooltips, and selectable copy.
                    let keyboard_root = cx.new(|cx| KeyboardRoot::new(view, window, cx));
                    cx.new(|cx| Root::new(keyboard_root, window, cx))
                },
            ) else {
                cx.quit();
                return;
            };

            // Settings is an overlay *inside* the workspace window, never a second window.
            workspace::register_menu_actions(workspace_window, cx);

            cx.bind_keys(workspace::key_bindings());

            cx.activate(true);
        });
}
