//! Product shell for the explicit, no-key map-session lifecycle.

#![deny(warnings)]

use app::{devwindow, mcp, reasoning, session, workspace};
use gpui::{
    App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, prelude::*, px,
    size,
};
use gpui_component::Root;
use workspace::{Assets, KeyboardRoot, MeetingWorkspace};

fn main() {
    // Registering the vendored icons here is what makes every `icons/*.svg` path resolvable —
    // Sotto's own markers and `gpui_component`'s built-in `IconName` variants alike. Before T082
    // the app registered no asset source at all, so an icon was never a possibility and every
    // control had to be a Unicode codepoint whose picture the font, not Sotto, chose.
    let (opened_link_sender, mut opened_link_receiver) = tokio::sync::mpsc::unbounded_channel();
    let application = Application::new().with_assets(Assets);
    application.on_open_urls(move |urls| {
        let _ = opened_link_sender.send(urls);
    });
    application.run(move |cx: &mut App| {
        gpui_component::init(cx);
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
                // The app's name is macOS's job now, so it has to actually be given to macOS:
                // T081's in-window row said "Sotto" and the window frame said nothing.
                // Transparent so *Sotto* paints the title strip, not macOS. The system titlebar is
                // drawn in the system appearance and gpui 0.2.2 exposes no way to set a window's
                // `NSAppearance`, so with `View ▸ Appearance` set to Dark against a light system
                // the strip stayed light above a dark window. The title is still supplied — the
                // window manager uses it in Mission Control and the window list even though the
                // frame no longer draws it.
                titlebar: Some(TitlebarOptions {
                    title: Some("Sotto".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(
                        workspace::TRAFFIC_LIGHT_INSET,
                        workspace::TRAFFIC_LIGHT_INSET,
                    )),
                }),
                ..Default::default()
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
                let keyboard_root = cx.new(|cx| KeyboardRoot::new(view, window, cx));
                cx.new(|cx| Root::new(keyboard_root, window, cx))
            },
        ) else {
            cx.quit();
            return;
        };

        // Settings is an overlay *inside* the workspace window, never a second window: the mock's
        // `#setScrim` covers the workspace it configures. The menu bar carries it — along with the
        // app name and the appearance switch — because that is where macOS puts an application's
        // chrome, and because it stays reachable there whatever the window is showing, including
        // while a recording runs. The menus themselves are published by the workspace, whose state
        // decides which appearance the menu marks.
        workspace::register_menu_actions(workspace_window, cx);

        // Register the citation scheme at runtime as well as in the signed bundle. The platform
        // callback is outside GPUI's app context, so it only queues URLs; the local receiver hands
        // them to the already-mounted workspace without network or process indirection.
        cx.register_url_scheme("sotto").detach();
        let workspace_view = workspace_window
            .update(cx, |root, _, root_cx| {
                root.view()
                    .clone()
                    .downcast::<KeyboardRoot>()
                    .and_then(|keyboard_root| {
                        keyboard_root
                            .read(root_cx)
                            .view()
                            .clone()
                            .downcast::<MeetingWorkspace>()
                    })
            })
            .ok()
            .and_then(Result::ok);
        if let Some(workspace_view) = workspace_view {
            cx.spawn(async move |cx| {
                while let Some(urls) = opened_link_receiver.recv().await {
                    if cx
                        .update(|cx| {
                            for url in urls {
                                if let Ok(link) = rag::SottoLink::parse(&url) {
                                    workspace_view.update(cx, |workspace, cx| {
                                        workspace.open_sotto_link(link, cx)
                                    });
                                }
                            }
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            })
            .detach();
        }

        cx.bind_keys(workspace::key_bindings());

        cx.activate(true);
    });
}
