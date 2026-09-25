//! Recording workspace: library rail, capture/view bars, and the stage over one session record.

mod ask;
mod control_row;
pub(crate) mod focus;
pub(crate) mod icons;
pub(crate) mod input;
mod layout;
mod library;
pub(crate) mod motion;
mod notes;
mod pacing;
mod runtime;
mod selectable;
pub(crate) mod tokens;
mod transcript;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::mpsc::TryRecvError,
    time::Instant,
};

use gpui_kit::component::button::{Button as KitButton, ButtonVariants as _};
use gpui_kit::component::input::{EditorState, InputEvent, InputState};
use gpui_kit::component::{Root, ThemeMode, WindowExt as _, h_flex};
use gpui_kit::{
    App, Context, ElementId, Entity, KeyBinding, ListAlignment, ListState, Menu, MenuItem,
    SharedString, Subscription, Window, WindowHandle, div, prelude::*, px,
};
use std::rc::Rc;

use focus::{Button, Size};
use icons::IconName;
use providers::ReasoningSurface;
use serde::{Deserialize, Serialize};
use sotto_core::{Entry, EntryId, EventId, SessionId, TimelineEvent};

use crate::{
    devwindow::TimelineState,
    mcp::McpController,
    notes::NotesController,
    reasoning::ReasoningController,
    session::{RecordingLibrary, SessionController, SessionLifecycle},
    settings::{SettingsEvent, SettingsView},
};

// The actions macOS invokes on Sotto's behalf. They are declared here rather than in `main.rs`
// because their handlers reach into this workspace, and because the menu bar that carries them is
// built from this module's state.
gpui_kit::actions!(
    sotto,
    [
        /// `Sotto ▸ Settings…`, and ⌘, — the shortcut every macOS user tries first.
        OpenSettings,
        /// `View ▸ Appearance ▸ Follow System`.
        FollowSystemAppearance,
        /// `View ▸ Appearance ▸ Light`.
        UseLightAppearance,
        /// `View ▸ Appearance ▸ Dark`.
        UseDarkAppearance,
    ]
);

pub use tokens::KeyboardRoot;

/// Always-visible scrollbars after kit init (see [`tokens::install_visible_scrollbars`]).
pub fn install_visible_scrollbars(cx: &mut App) {
    tokens::install_visible_scrollbars(cx);
}

/// Where the traffic lights sit inside the title strip, and how tall that strip is.
///
/// The window titlebar is transparent so the shell's own ground colour reaches the very top of the
/// window. macOS draws its own titlebar in the *system* appearance, and gpui-pre exposes no
/// way to set a window's `NSAppearance` — so with `View ▸ Appearance` set to Dark against a light
/// system, the strip stayed light above a dark window. Painting it ourselves is the only way the
/// appearance choice reaches the whole window.
///
/// The cost is that the traffic lights now sit over the shell's own content, so the top of the
/// stage reserves this much room for them.
pub const TRAFFIC_LIGHT_INSET: gpui_kit::Pixels = gpui_kit::px(13.0);
pub(crate) const TITLE_STRIP_HEIGHT: gpui_kit::Pixels = gpui_kit::px(38.0);

/// An icon control: a picture the app owns, with its words in the tooltip.
///
/// Built on [`focus::Button`] rather than stock kit `IconButton`, so tests can attach a
/// `debug_selector` and chrome can use the kit Lucide catalog (ADR-0025).
///
/// Every icon button in the shell is built here so no call site can ship a picture with nothing
/// behind it. `label` is that control's accessible name and reaches a person through the tooltip.
///
/// Be precise about how far that goes: the pinned gpui-pre line still publishes **no**
/// platform accessibility tree — no ARIA, no `AXTitle`, no AccessKit bridge — so there is no
/// channel through which a screen reader could read this name, and drawing an invisible label to
/// satisfy the letter of the rule would be a dead control written as text. The tooltip is the whole
/// of what the framework offers,
/// which is why icons stay confined to chrome and to a destructive action whose words arrive in
/// the dialog it opens. Domain verbs — Start, Stop, Ask, Re-transcribe, Reveal — keep their words
/// even now that the pictures are real.
pub(crate) fn icon_button(
    id: impl Into<ElementId>,
    icon: IconName,
    label: impl Into<SharedString>,
    tokens: tokens::WorkspaceTokens,
) -> Button {
    Button::new(id, tokens)
        .icon(icon)
        .tooltip(label)
        .ghost()
        .with_size(Size::Small)
}

/// Delete, reduced to its settled picture. The words arrive in the dialog it opens.
///
/// The trailing ellipsis is the platform's own promise that this control asks before it acts;
/// `target` names what is at risk — "this recording", "the recording for “Sprint 41 planning”".
pub(crate) fn delete_icon_button(
    id: impl Into<ElementId>,
    target: &str,
    tokens: tokens::WorkspaceTokens,
) -> Button {
    Button::new(id, tokens)
        .icon(IconName::Delete)
        .tooltip(format!("Delete {target}…"))
        .danger()
        .with_size(Size::Small)
}

/// Debug selectors for a confirm dialog's two answers.
///
/// Kit dialog buttons carry no product selectors, so the footer wraps each answer. That is what
/// lets a mounted test click the same control a person clicks.
pub(crate) const CONFIRM_OK_SELECTOR: &str = "confirm-dialog-ok";
pub(crate) const CONFIRM_CANCEL_SELECTOR: &str = "confirm-dialog-cancel";

/// Opens the one shape every destructive confirmation in Sotto takes.
///
/// Cancel first, then the destructive verb; no outside-click dismiss; no close glyph. Escape and
/// Cancel close without running `on_ok`. Mounts under `Root` via [`WindowExt::open_alert_dialog`]
/// — kit AlertDialog disables backdrop dismissal by design.
pub(crate) fn open_confirm_delete_dialog(
    window: &mut Window,
    cx: &mut App,
    id: &'static str,
    title: &str,
    prompt: &str,
    ok_label: &str,
    on_ok: impl Fn(&mut Window, &mut App) + 'static,
) {
    let title = SharedString::from(title.to_owned());
    let prompt = SharedString::from(prompt.to_owned());
    let ok_label = SharedString::from(ok_label.to_owned());
    let on_ok = Rc::new(on_ok);
    let cancel_id = format!("{id}-cancel");
    let ok_id = format!("{id}-ok");
    window.open_alert_dialog(cx, move |alert, _, _| {
        let on_ok = Rc::clone(&on_ok);
        let ok_label = ok_label.clone();
        let cancel_id = cancel_id.clone();
        let ok_id = ok_id.clone();
        alert
            .confirm()
            .title(title.clone())
            .description(prompt.clone())
            .footer(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .id(CONFIRM_CANCEL_SELECTOR)
                            .debug_selector(|| CONFIRM_CANCEL_SELECTOR.into())
                            .child(KitButton::new(cancel_id).label("Cancel").on_click(
                                |_, window, cx| {
                                    window.close_dialog(cx);
                                },
                            )),
                    )
                    .child(
                        div()
                            .id(CONFIRM_OK_SELECTOR)
                            .debug_selector(|| CONFIRM_OK_SELECTOR.into())
                            .child(KitButton::new(ok_id).label(ok_label).danger().on_click({
                                let on_ok = Rc::clone(&on_ok);
                                move |_, window, cx| {
                                    on_ok(window, cx);
                                    window.close_dialog(cx);
                                }
                            })),
                    ),
            )
    });
}

/// The theme the person chose, as it survives a relaunch.
///
/// `gpui_kit::init` syncs the theme to the system appearance at launch, so `None` — no
/// recorded choice — means Sotto keeps following the system. A recorded choice is what makes the
/// appearance menu a real setting rather than a switch that forgets.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PersistedTheme {
    Light,
    Dark,
}

impl PersistedTheme {
    const fn mode(self) -> ThemeMode {
        match self {
            Self::Light => ThemeMode::Light,
            Self::Dark => ThemeMode::Dark,
        }
    }
}

/// What `View ▸ Appearance` offers, including the state that is *not* a fixed palette.
///
/// "Follow System" is a first-class choice rather than the absence of one, which is why it has to
/// appear in the menu: without it a person who tried Light once could never get back to the
/// behaviour they started with.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Appearance {
    #[default]
    FollowSystem,
    Light,
    Dark,
}

impl Appearance {
    #[must_use]
    const fn persisted(self) -> Option<PersistedTheme> {
        match self {
            Self::FollowSystem => None,
            Self::Light => Some(PersistedTheme::Light),
            Self::Dark => Some(PersistedTheme::Dark),
        }
    }

    const fn from_persisted(theme: Option<PersistedTheme>) -> Self {
        match theme {
            None => Self::FollowSystem,
            Some(PersistedTheme::Light) => Self::Light,
            Some(PersistedTheme::Dark) => Self::Dark,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::FollowSystem => "Follow System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

/// The three appearance items, and which one is currently in force.
///
/// gpui-pre's `MenuItem` exposes no checked state — there is no way to reach `NSMenuItem`'s
/// `state` through it — so the current choice is marked in the item's own text. U+2713 is a
/// text-presentation codepoint: unlike the trash bin that started T082, no font substitutes a
/// colour emoji for it, and this is a macOS menu string rather than one of Sotto's controls.
fn appearance_items(current: Appearance) -> Vec<MenuItem> {
    fn mark(item: Appearance, current: Appearance) -> String {
        if item == current {
            format!("\u{2713} {}", item.label())
        } else {
            format!("   {}", item.label())
        }
    }

    vec![
        MenuItem::action(
            mark(Appearance::FollowSystem, current),
            FollowSystemAppearance,
        ),
        MenuItem::action(mark(Appearance::Light, current), UseLightAppearance),
        MenuItem::action(mark(Appearance::Dark, current), UseDarkAppearance),
    ]
}

/// ⌘, — the shortcut a macOS user tries before reading the menu.
///
/// Binding it also makes `Sotto ▸ Settings…` display its key equivalent. It is built here rather
/// than inline in `main.rs` because `KeyBinding::new` **panics** on a keystroke it cannot parse,
/// and a typo in a string literal on the launch path is not something to discover by launching.
pub fn key_bindings() -> Vec<KeyBinding> {
    vec![KeyBinding::new("cmd-,", OpenSettings, None)]
}

/// App-level key bindings and action listeners that outlive any one window.
///
/// ⌘C for markdown selection is bound here as a fallback when window-scoped selection is empty;
/// `Root` already binds Copy for [`gpui_kit::base::TextSelection`] (see [`selectable`]).
pub fn install_global_bindings(cx: &mut App) {
    selectable::bind_copy_keys(cx);
}

/// Test helpers shared by workspace mounts that host kit Root overlays.
#[cfg(test)]
pub(crate) mod test_support {
    use gpui_kit::VisualTestContext;

    /// Kit Root dialogs animate in. Reduce motion + two draws matches gpui-component's own tests:
    /// one frame mounts the layer, the next paints it at rest so `debug_bounds` can see footers.
    pub(crate) fn settle_root_overlays(visual: &mut VisualTestContext) {
        visual.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        visual.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
    }
}

/// Wires the menu-bar actions to the workspace inside `window`.
///
/// This lives beside the workspace rather than in `main.rs` because it is the half of T082's first
/// defect that can be tested: a test can mount the same `Root`-rooted window, register these
/// listeners, and dispatch `OpenSettings` the way the platform's menu callback does.
pub fn register_menu_actions(window: WindowHandle<Root>, cx: &mut App) {
    cx.on_action(move |_: &OpenSettings, cx| {
        in_workspace(window, cx, |workspace, window, cx| {
            workspace.toggle_settings(window, cx);
        });
    });
    cx.on_action(move |_: &FollowSystemAppearance, cx| {
        in_workspace(window, cx, |workspace, window, cx| {
            workspace.set_appearance(Appearance::FollowSystem, window, cx);
        });
    });
    cx.on_action(move |_: &UseLightAppearance, cx| {
        in_workspace(window, cx, |workspace, window, cx| {
            workspace.set_appearance(Appearance::Light, window, cx);
        });
    });
    cx.on_action(move |_: &UseDarkAppearance, cx| {
        in_workspace(window, cx, |workspace, window, cx| {
            workspace.set_appearance(Appearance::Dark, window, cx);
        });
    });
}

/// Runs `body` against the workspace, **after** the dispatch that asked for it has unwound.
///
/// `cx.defer` is the whole of T082's first fix, and the reason is not visible in any single line of
/// the code it replaces.
///
/// A global action listener registered with `cx.on_action` is invoked from
/// `Window::dispatch_action_on_node`, during its capture phase — and that runs *inside*
/// `App::update_window_id`, which has taken this very window out of `App::windows` with
/// `Option::take` in order to hand out a `&mut Window`. A second `WindowHandle::update` on the same
/// window while it is out of the slot therefore cannot find it: `update_window_id` returns
/// `Err("window not found")` and the closure never runs at all — the downcast, the entity update
/// and `toggle_settings` were never reached, and a `let _ = …` discarded the error that said so.
/// The title-bar gear worked precisely because it dispatched from an element listener that was
/// already holding the window.
///
/// Deferring moves the work onto the effect queue, which the outermost `App::update` flushes once
/// the window has been put back, so the handle resolves exactly as it does from a click.
fn in_workspace(
    window: WindowHandle<Root>,
    cx: &mut App,
    body: impl FnOnce(&mut MeetingWorkspace, &mut Window, &mut Context<MeetingWorkspace>) + 'static,
) {
    cx.defer(move |cx| {
        let resolved = window.update(cx, |root, window, cx| {
            root.view()
                .clone()
                .downcast::<MeetingWorkspace>()
                .ok()
                .or_else(|| {
                    root.view()
                        .clone()
                        .downcast::<KeyboardRoot>()
                        .ok()
                        .and_then(|keyboard_root| {
                            keyboard_root
                                .read(cx)
                                .view()
                                .clone()
                                .downcast::<MeetingWorkspace>()
                                .ok()
                        })
                })
                .map(|workspace| {
                    workspace.update(cx, |workspace, cx| body(workspace, window, cx));
                })
                .is_some()
        });
        // Never silent. Swallowing this is what let `Sotto ▸ Settings…` look correctly wired while
        // doing nothing: the listener was reached, the window was not, and nothing said so.
        match resolved {
            Ok(true) => {}
            Ok(false) => eprintln!(
                "sotto: a menu action could not reach the workspace; this window's root view is \
                 not a MeetingWorkspace"
            ),
            Err(error) => eprintln!("sotto: a menu action could not reach the workspace: {error}"),
        }
    });
}

/// Sotto's menu bar, which is where the app name, Settings and the appearance switch live.
///
/// T081 drew these as a row inside the window. macOS already publishes all three, so that row was
/// a second copy of the operating system's chrome paid for in the vertical space the transcript
/// needs. Note that Settings sits in the app menu deliberately: it stays reachable while a
/// recording runs, whatever the window is showing, and Stop is unaffected because it lives on the
/// capture bar.
pub(crate) fn application_menus(appearance: Appearance) -> Vec<Menu> {
    vec![
        Menu {
            name: "Sotto".into(),
            items: vec![MenuItem::action("Settings…", OpenSettings)],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![MenuItem::submenu(Menu {
                name: "Appearance".into(),
                items: appearance_items(appearance),
                disabled: false,
            })],
            disabled: false,
        },
    ]
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
struct PersistedWorkspaceState {
    ask_open: bool,
    #[serde(default)]
    theme: Option<PersistedTheme>,
    /// Whether the library rail was collapsed when Sotto last quit.
    ///
    /// `#[serde(default)]` is what makes an existing `workspace-state.json` — which has no such
    /// field — still parse, and it defaults to *expanded*: a person who has never collapsed the
    /// rail must not come back to a window that hid its only navigator.
    #[serde(default)]
    library_collapsed: bool,
}

/// The two halves of a stopped session's single stage. Notes lead; the transcript is the evidence
/// layer one tab away.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum StageTab {
    #[default]
    Notes,
    Transcript,
}

impl StageTab {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Notes => "Notes",
            Self::Transcript => "Transcript",
        }
    }

    pub(crate) const fn element_id(self) -> &'static str {
        match self {
            Self::Notes => "stage-tab-notes",
            Self::Transcript => "stage-tab-transcript",
        }
    }

    pub(crate) const fn debug_selector(self) -> &'static str {
        self.element_id()
    }
}

/// What the open session's retained media measures, read once on open rather than every frame.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct OpenRecording {
    pub(crate) path: Option<String>,
    pub(crate) duration: Option<std::time::Duration>,
    pub(crate) byte_size: Option<u64>,
}

pub struct MeetingWorkspace {
    database: PathBuf,
    notes: Entity<NotesController>,
    timeline: Entity<TimelineState>,
    reasoning: Entity<ReasoningController>,
    session: Entity<SessionController>,
    pub(crate) mcp: Entity<McpController>,
    message: Option<String>,
    observed_live_session: Option<SessionId>,
    observed_completed_session: Option<SessionId>,
    entries: Vec<Entry>,
    selected_entry: Option<EntryId>,
    transcript_session: Option<SessionId>,
    transcript_events: Vec<TimelineEvent>,
    transcript_live: bool,
    stage_tab: StageTab,
    open_recording: Option<OpenRecording>,
    transcript_list: ListState,
    transcript_pacer: pacing::TranscriptPacer,
    follow_transcript: bool,
    focused_event: Option<EventId>,
    /// The contiguous finalized transcript rows explicitly offered as an Ask scope.
    ask_selection: Option<ask::AskSelection>,
    library_filter: Entity<InputState>,
    entry_title_input: Entity<InputState>,
    annotation_input: Entity<InputState>,
    notes_document_input: Entity<EditorState>,
    prepared_notes: Vec<String>,
    editing_annotation: Option<notes::AnnotationView>,
    editing_notes_document: bool,
    library_index: BTreeMap<SessionId, String>,
    entry_library_index: BTreeMap<EntryId, String>,
    library_footprint: library::LibraryFootprint,
    /// Whether the library rail is hidden. Persisted, the way `ask_open` is.
    library_collapsed: bool,
    ask_panel: Entity<ask::AskPanel>,
    pending_ask: Option<(ask::PendingAsk, String)>,
    ask_open: bool,
    /// The settings sheet, built the first time it is asked for and kept afterwards. Building it
    /// probes the Codex CLI and starts the MCP poll, so cold launch must not build it and each
    /// re-open must not build another.
    settings: Option<Entity<SettingsView>>,
    settings_open: bool,
    /// Confirm dialog currently open over the shell (delete recording / entry). Rendered last.
    theme: Option<PersistedTheme>,
    live_started_at: Option<Instant>,
    retranscription_running: bool,
    _subscriptions: Vec<Subscription>,
}

impl MeetingWorkspace {
    #[must_use]
    pub fn new(
        database: PathBuf,
        timeline: Entity<TimelineState>,
        reasoning: Entity<ReasoningController>,
        session: Entity<SessionController>,
        mcp: Entity<McpController>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        selectable::bind_copy_keys(cx);
        let mut controller = NotesController::new(database.clone());
        let message = controller.refresh_catalogue().err();
        let notes = cx.new(|_| controller);
        // The placeholder is the only place this control says what it searches, and it searches
        // more than the rail's titles: `library::search_index` folds each recording's transcript,
        // its user annotations and its generated notes into the same haystack. "Filter recordings"
        // would understate it, and T083 moved the control into the toolbar precisely so it stays
        // reachable when the rail it filters is collapsed.
        let library_filter =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search entries and notes"));
        let entry_title_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Name this entry"));
        let annotation_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Add a note to this recording"));
        let notes_document_input = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("markdown")
                .soft_wrap(true)
                .line_number(false)
                .placeholder("Edit the note as markdown")
        });
        let persisted = layout::load_workspace_state(&database);
        let ask_open = persisted.ask_open;
        // The component library supplies the ring geometry; the workspace owns the two colours
        // that keep it visible on Sotto's light and dark surfaces.
        tokens::install_component_focus_ring(cx);
        // A recorded theme choice is applied before the first frame, so the shell never paints one
        // theme and then flips to the other in front of the person who chose it.
        if let Some(theme) = persisted.theme {
            tokens::apply_theme(theme.mode(), Some(window), cx);
        }
        tokens::install_visible_scrollbars(cx);
        // The menu bar carries the app name, Settings and the appearance switch, so it has to be
        // published before the first frame and to already mark the recorded choice.
        cx.set_menus(application_menus(Appearance::from_persisted(
            persisted.theme,
        )));
        let ask_panel = cx.new(|cx| ask::AskPanel::new(window, cx));
        let following_system = cx.entity().downgrade();
        let subscriptions = vec![
            // "Follow System" has to keep following. `gpui_kit::init` reads the system
            // appearance once at launch and never again, so without this the choice would quietly
            // mean "match the system as it was when Sotto started". A recorded Light or Dark
            // choice is left alone: the person asked for a fixed palette.
            window.observe_window_appearance(move |window, cx| {
                let follows = following_system
                    .upgrade()
                    .is_some_and(|workspace| workspace.read(cx).theme.is_none());
                if follows {
                    tokens::sync_system_appearance(Some(window), cx);
                }
            }),
            cx.observe(&reasoning, |this, _, cx| {
                this.sync_reasoning(cx);
                cx.notify();
            }),
            cx.observe(&session, |this, _, cx| {
                this.refresh_after_session(cx);
                cx.notify();
            }),
            cx.observe(&mcp, |_, _, cx| cx.notify()),
            cx.observe(&timeline, |this: &mut Self, _, cx| {
                if this.transcript_live && this.follow_transcript {
                    this.transcript_list.scroll_to(gpui_kit::ListOffset {
                        item_ix: this.transcript_pacer.rows().len(),
                        offset_in_item: px(0.0),
                    });
                }
                cx.notify();
            }),
            // Observe, not only subscribe-to-Change: kit `InputState::set_value` notifies without
            // emitting `InputEvent::Change`, and a search that lands nowhere is still no search.
            // Typing also notifies, so one observer covers both paths. Clearing the query
            // deliberately does *not* collapse the rail again.
            cx.observe(&library_filter, |this: &mut Self, filter, cx| {
                if this.library_collapsed && !filter.read(cx).value().trim().is_empty() {
                    this.set_library_collapsed(false);
                }
                cx.notify();
            }),
            cx.subscribe_in(
                &annotation_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. })
                        && this.transcript_session.is_none()
                        && this.selected_entry.is_some()
                    {
                        this.submit_prepared_note(window, cx);
                        return;
                    }
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.submit_annotation(window, cx);
                    }
                },
            ),
            cx.subscribe(
                &ask_panel,
                |this, _, event: &ask::AskPanelEvent, cx| match event {
                    ask::AskPanelEvent::Submit(question) => this.start_ask(question.clone(), cx),
                    ask::AskPanelEvent::Cancel => this.cancel_ask(cx),
                    ask::AskPanelEvent::Citation(citation) => {
                        if this.transcript_session != Some(citation.session_id) {
                            this.select_meeting(citation.session_id, cx);
                        }
                        this.open_citation(citation.event_id, cx);
                    }
                    ask::AskPanelEvent::SelectScope(choice) => {
                        this.cancel_pending_ask();
                        let choice = *choice;
                        this.ask_panel
                            .update(cx, |panel, _| panel.select_scope(choice));
                        cx.notify();
                    }
                    ask::AskPanelEvent::ClearSelection => this.clear_ask_selection(cx),
                    ask::AskPanelEvent::SelectProvider(backend) => {
                        this.select_provider(ReasoningSurface::Ask, *backend, cx);
                    }
                },
            ),
        ];
        let mut workspace = Self {
            database,
            notes,
            timeline,
            reasoning,
            session,
            mcp,
            message,
            observed_live_session: None,
            observed_completed_session: None,
            entries: Vec::new(),
            selected_entry: None,
            transcript_session: None,
            transcript_events: vec![],
            transcript_live: false,
            stage_tab: StageTab::default(),
            open_recording: None,
            transcript_list: ListState::new(0, ListAlignment::Bottom, px(240.0)),
            transcript_pacer: pacing::TranscriptPacer::default(),
            follow_transcript: true,
            focused_event: None,
            ask_selection: None,
            library_filter,
            entry_title_input,
            annotation_input,
            notes_document_input,
            prepared_notes: Vec::new(),
            editing_annotation: None,
            editing_notes_document: false,
            library_index: BTreeMap::new(),
            entry_library_index: BTreeMap::new(),
            library_footprint: library::LibraryFootprint::default(),
            library_collapsed: persisted.library_collapsed,
            ask_panel,
            pending_ask: None,
            ask_open,
            settings: None,
            settings_open: false,
            theme: persisted.theme,
            live_started_at: None,
            retranscription_running: false,
            _subscriptions: subscriptions,
        };
        workspace.sync_reasoning(cx);
        workspace.rebuild_library_index(cx);
        // Launch lands on Home, deliberately. Nothing has been recorded since the app last quit,
        // so no recording is the one the person came back for; opening the newest one made Sotto
        // present itself as a viewer of a single capture and put its own entry points out of
        // reach. Home is the only state that is correct whatever the library holds.
        let ask_ready = workspace.ask_ready(cx);
        workspace.sync_ask_scope(ask_ready, cx);
        workspace.poll_notes(cx);
        workspace.poll_ask_updates(cx);
        workspace.poll_transcript_pacing(cx);
        workspace.poll_session_clock(cx);
        workspace
    }

    fn reasoning_backend(
        &self,
        surface: ReasoningSurface,
        cx: &Context<Self>,
    ) -> Result<Option<providers::ResolvedBackend>, String> {
        // No capability gate here. `JsonObjectOutput` means the backend can *guarantee* JSON
        // syntactically; lacking it is a downgrade, not an inability. Insight's normalization
        // policy owns that decision at a single seam, parses every reply defensively, and records
        // the loss on the result so the user sees it.
        self.reasoning
            .read(cx)
            .resolve(surface)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn notes_ready(&self, cx: &Context<Self>) -> bool {
        self.reasoning_backend(ReasoningSurface::Notes, cx)
            .is_ok_and(|backend| backend.is_some())
    }

    pub(crate) fn ask_ready(&self, cx: &Context<Self>) -> bool {
        self.reasoning_backend(ReasoningSurface::Ask, cx)
            .is_ok_and(|backend| backend.is_some())
    }

    fn sync_reasoning(&mut self, cx: &mut Context<Self>) {
        let notes_ready = self.notes_ready(cx);
        self.notes
            .update(cx, |notes, _| notes.set_reasoning_enabled(notes_ready));
        self.sync_ask_scope(self.ask_ready(cx), cx);
    }

    fn refresh_after_session(&mut self, cx: &mut Context<Self>) {
        let active = self.session.read(cx).active_session_id();
        if let Some(id) = active {
            let newly_active =
                runtime::newly_active_session(&mut self.observed_live_session, Some(id)).is_some();
            self.message = self
                .notes
                .update(cx, |notes, _| notes.refresh_catalogue())
                .err();
            if self.message.is_none() {
                self.rebuild_library_index(cx);
            }
            if newly_active {
                self.live_started_at = Some(Instant::now());
                self.show_live_transcript(id, cx);
            }
            return;
        }
        if !matches!(
            self.session.read(cx).lifecycle(),
            SessionLifecycle::Idle { .. } | SessionLifecycle::Error(_)
        ) {
            return;
        }
        let Some(completed) = self.session.read(cx).completed_session_id() else {
            return;
        };
        if self.observed_completed_session == Some(completed) {
            return;
        }
        self.live_started_at = None;
        let enabled = self.notes_ready(cx);
        self.message = self
            .notes
            .update(cx, |notes, _| {
                notes.refresh_catalogue()?;
                let _ = notes.select(completed, enabled);
                Ok::<(), String>(())
            })
            .err();
        if self.message.is_none() {
            self.observed_completed_session = Some(completed);
            self.transcript_pacer.drain();
            self.load_transcript(completed);
            self.rebuild_library_index(cx);
            self.mcp.update(cx, |controller, cx| {
                controller.select_session(completed, cx)
            });
        }
    }

    pub fn select_provider(
        &mut self,
        surface: ReasoningSurface,
        backend: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        self.message = self
            .reasoning
            .update(cx, |controller, cx| {
                controller.select_surface(surface, backend, cx)
            })
            .err()
            .map(|error| error.to_string());
        self.sync_reasoning(cx);
        cx.notify();
    }

    fn generate_notes(&mut self, cx: &mut Context<Self>) {
        let backend = match self.reasoning_backend(ReasoningSurface::Notes, cx) {
            Ok(Some(value)) => value,
            Ok(None) => {
                self.message =
                    Some("Add a ready provider in Settings, then pick it on Notes.".into());
                cx.notify();
                return;
            }
            Err(error) => {
                self.message = Some(error);
                cx.notify();
                return;
            }
        };
        self.message = self
            .mcp
            .read(cx)
            .freeze_selected_grounding()
            .map_err(|error| error.to_string())
            .and_then(|grounding| {
                self.notes.update(cx, |notes, _| {
                    notes.start_generation(backend, Some(grounding))
                })
            })
            .err();
        self.editing_notes_document = false;
        cx.notify();
    }

    pub(crate) fn select_meeting(&mut self, id: SessionId, cx: &mut Context<Self>) {
        self.on_displayed_view_changed(cx);
        self.cancel_pending_ask();
        self.editing_notes_document = false;
        if self.session.read(cx).active_session_id() == Some(id) {
            self.show_live_transcript(id, cx);
            cx.notify();
            return;
        }
        let enabled = self.notes_ready(cx);
        let notes_session = self
            .entry_id_for_session(id)
            .and_then(|entry_id| self.entries.iter().find(|entry| entry.id() == entry_id))
            .and_then(|entry| entry.session_ids().last())
            .copied()
            .unwrap_or(id);
        if self
            .notes
            .update(cx, |notes, _| notes.select(notes_session, enabled))
        {
            self.message = None;
            self.clear_ask_selection(cx);
            self.load_transcript(id);
            self.mcp
                .update(cx, |controller, cx| controller.select_session(id, cx));
            self.sync_ask_scope(self.ask_ready(cx), cx);
        } else {
            self.message = Some("That recording is no longer available.".into());
        }
        cx.notify();
    }

    pub(crate) fn select_entry(&mut self, entry_id: EntryId, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.iter().find(|entry| entry.id() == entry_id) else {
            self.message = Some("That entry is no longer available.".to_owned());
            cx.notify();
            return;
        };
        self.selected_entry = Some(entry_id);
        if let Some(active) = self.session.read(cx).active_session_id()
            && entry.session_ids().contains(&active)
        {
            self.show_live_transcript(active, cx);
            cx.notify();
            return;
        }
        if let Some(session_id) = entry.session_ids().last().copied() {
            self.select_meeting(session_id, cx);
            return;
        }
        self.on_displayed_view_changed(cx);
        self.cancel_pending_ask();
        self.clear_ask_selection(cx);
        self.transcript_session = None;
        self.transcript_events.clear();
        self.transcript_live = false;
        self.transcript_pacer.clear();
        self.transcript_list.reset(0);
        self.open_recording = None;
        self.focused_event = None;
        self.message = None;
        self.refresh_prepared_notes(entry_id);
        self.sync_ask_scope(self.ask_ready(cx), cx);
        cx.notify();
    }

    pub(crate) fn create_prepared_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.entry_title_input.read(cx).value().to_string();
        let Some(title) = sotto_core::types::RecordingTitle::new(&raw) else {
            self.message = Some("Name the entry before preparing it.".to_owned());
            cx.notify();
            return;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let entry_id = EntryId::new(now);
        let created_at = u64::try_from(now / 1_000_000).unwrap_or(u64::MAX);
        let result = crate::persistence_runtime::block_on(async {
            rag::Store::open(&self.database)
                .await?
                .create_entry(&Entry::new(entry_id, created_at, Some(title)))
                .await
        });
        match result {
            Ok(()) => {
                self.entry_title_input
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.rebuild_library_index(cx);
                self.select_entry(entry_id, cx);
            }
            Err(error) => {
                self.message = Some(format!("Could not prepare this entry: {error}"));
                cx.notify();
            }
        }
    }

    fn submit_prepared_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry_id) = self.selected_entry else {
            return;
        };
        let text = self.annotation_input.read(cx).value().to_string();
        if text.trim().is_empty() {
            self.message = Some("Type a prep note before saving it.".to_owned());
            cx.notify();
            return;
        }
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or_default();
        let operation = insight::NotesOverlayOperation::Add {
            user_block_id: format!("prep-{created_at}"),
            section: insight::RecordingNotesSectionKind::Overview,
            text,
            action: false,
            owner: None,
            due_date: None,
        };
        let result = crate::persistence_runtime::block_on(async {
            let store = rag::Store::open(&self.database).await?;
            insight::append_notes_overlay_operation(
                &store,
                entry_id,
                &insight::RecordingNotes::default(),
                &operation,
                created_at,
            )
            .await
            .map_err(|error| sotto_core::RagError::Storage(error.to_string()))?;
            Ok::<_, sotto_core::RagError>(())
        });
        match result {
            Ok(()) => {
                self.annotation_input
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.refresh_prepared_notes(entry_id);
                self.rebuild_library_index(cx);
                self.message = Some("Prep note saved in this entry.".to_owned());
            }
            Err(error) => self.message = Some(format!("Could not save the prep note: {error}")),
        }
        cx.notify();
    }

    fn refresh_prepared_notes(&mut self, entry_id: EntryId) {
        self.prepared_notes = crate::persistence_runtime::block_on(async {
            let store = rag::Store::open(&self.database).await?;
            let document = insight::load_presented_notes_document(
                &store,
                entry_id,
                &insight::RecordingNotes::default(),
            )
            .await
            .map_err(|error| sotto_core::RagError::Storage(error.to_string()))?;
            Ok::<_, sotto_core::RagError>(
                document
                    .blocks
                    .into_iter()
                    .map(|block| block.text)
                    .collect(),
            )
        })
        .unwrap_or_default();
    }

    fn load_transcript(&mut self, id: SessionId) {
        match crate::persistence_runtime::block_on(async {
            let store = rag::Store::open(&self.database).await?;
            Ok::<_, sotto_core::RagError>((
                store.load_session(id).await?,
                store.load_derived_transcript(id).await?,
            ))
        }) {
            Ok((mut events, derived)) => {
                // Event ids are the append-only ordering authority. Persisted timestamps can come
                // from different capture clocks, so timestamp order may place a replacement before
                // the event it supersedes and make an otherwise valid recording replay as empty.
                events.sort_by_key(TimelineEvent::id);
                let projection = derived.map_or_else(
                    || transcript::project_completed_transcript(&events),
                    |derived| {
                        transcript::project_completed_derived_transcript(
                            &events,
                            &derived.utterances,
                        )
                    },
                );
                self.transcript_pacer.replace(projection.committed);
                self.transcript_list
                    .reset(self.transcript_pacer.rows().len());
                self.transcript_session = Some(id);
                self.selected_entry = self.entry_id_for_session(id);
                self.transcript_events = events;
                self.transcript_live = false;
                self.follow_transcript = false;
                self.focused_event = None;
                // A stopped session always opens on Notes: the summary is what the person came
                // back for, and the transcript stays one tab away.
                self.stage_tab = StageTab::Notes;
                self.open_recording = self.read_open_recording(id);
            }
            Err(error) => {
                self.message = Some(format!(
                    "Could not reopen this recording's transcript: {error}"
                ))
            }
        }
    }

    fn read_open_recording(&self, id: SessionId) -> Option<OpenRecording> {
        let recording = crate::persistence_runtime::block_on(async {
            rag::Store::open(&self.database)
                .await?
                .load_recording(id)
                .await
        })
        .ok()??;
        match recording {
            sotto_core::types::SessionRecording::Available {
                path,
                duration,
                byte_size,
                ..
            } => Some(OpenRecording {
                path: Some(path),
                duration: Some(duration),
                byte_size: Some(byte_size),
            }),
            sotto_core::types::SessionRecording::Missing { .. } => Some(OpenRecording::default()),
        }
    }

    fn show_live_transcript(&mut self, id: SessionId, cx: &mut Context<Self>) {
        self.on_displayed_view_changed(cx);
        self.cancel_pending_ask();
        self.clear_ask_selection(cx);
        self.transcript_session = Some(id);
        self.selected_entry = self.entry_id_for_session(id);
        self.transcript_events.clear();
        self.transcript_live = true;
        self.transcript_pacer.clear();
        self.transcript_list.reset(0);
        self.follow_transcript = true;
        self.focused_event = None;
        self.stage_tab = StageTab::Notes;
        self.open_recording = None;
    }

    /// Returns to Home: the open recording is deselected and the entry points come back.
    ///
    /// Home is a *state*, not a second navigation surface — the rail remains the only navigator,
    /// and this only clears the shell's selection. It deliberately touches neither the session
    /// controller nor the timeline, so a running capture keeps running and its bar, which follows
    /// the lifecycle rather than the stage, keeps Stop one visible action away.
    pub(crate) fn show_home(&mut self, cx: &mut Context<Self>) {
        self.on_displayed_view_changed(cx);
        self.cancel_pending_ask();
        self.clear_ask_selection(cx);
        self.transcript_session = None;
        self.selected_entry = None;
        self.transcript_events.clear();
        self.transcript_live = false;
        self.transcript_pacer.clear();
        self.transcript_list.reset(0);
        self.follow_transcript = false;
        self.focused_event = None;
        self.open_recording = None;
        self.message = None;
        let ready = self.ask_ready(cx);
        self.sync_ask_scope(ready, cx);
        cx.notify();
    }

    /// Returns to the running recording from a stopped session opened out of the rail.
    pub(crate) fn return_to_live(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.session.read(cx).active_session_id() {
            self.show_live_transcript(id, cx);
            cx.notify();
        }
    }

    pub(crate) fn select_stage_tab(&mut self, tab: StageTab, cx: &mut Context<Self>) {
        if self.stage_tab != tab {
            self.on_displayed_view_changed(cx);
            self.stage_tab = tab;
            cx.notify();
        }
    }

    /// Drop retained selectable leaves whenever the displayed recording or stage view changes.
    ///
    /// Call from every navigation path that swaps what the stage shows — including live return,
    /// prepared-entry open, and Home — not only [`Self::select_meeting`].
    fn on_displayed_view_changed(&mut self, cx: &mut Context<Self>) {
        selectable::clear_retained(cx);
    }

    /// Shows the retained recording in Finder. The file stays where it already lives on this Mac.
    pub(crate) fn reveal_open_recording(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self
            .open_recording
            .as_ref()
            .and_then(|recording| recording.path.clone())
        else {
            self.message = Some("This recording's media is no longer retained.".to_owned());
            cx.notify();
            return;
        };
        self.message = std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(&path)
            .spawn()
            .err()
            .map(|error| format!("Could not reveal {path}: {error}"));
        cx.notify();
    }

    /// Asks, in a dialog over the control, before deleting the open session and its retained media.
    ///
    /// The dialog is what makes this answerable: it blocks the shell underneath, so the question
    /// cannot be walked away from by clicking elsewhere, and it offers Cancel rather than leaving
    /// the way out to be discovered.
    pub(crate) fn delete_open_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.transcript_session.filter(|_| !self.transcript_live) else {
            return;
        };
        let prompt = self.delete_prompt(id, cx);
        let workspace = cx.entity().downgrade();
        open_confirm_delete_dialog(
            window,
            cx,
            "delete-recording",
            "Delete recording",
            &prompt,
            "Delete",
            move |_, cx| {
                let _ = workspace.update(cx, |workspace, cx| {
                    workspace.confirm_delete_session(id, cx);
                });
            },
        );
        cx.notify();
    }

    /// Deletes exactly the recording the dialog named — `id` is the one the prompt was written
    /// about, not whatever happens to be open when OK is clicked.
    fn confirm_delete_session(&mut self, id: SessionId, cx: &mut Context<Self>) {
        self.cancel_pending_ask();
        let owning_entry = self.entry_id_for_session(id);
        let recordings = RecordingLibrary::new(
            self.database.clone(),
            crate::session::application_recording_directory(),
        );
        let outcome = recordings.delete(id).and_then(|_| {
            crate::persistence_runtime::block_on(async {
                rag::Store::open(&self.database)
                    .await?
                    .delete_session(id)
                    .await
            })
        });
        if let Err(error) = outcome {
            self.message = Some(format!("Could not delete this recording: {error}"));
            cx.notify();
            return;
        }
        if self.transcript_session == Some(id) {
            self.on_displayed_view_changed(cx);
            self.transcript_session = None;
            self.transcript_events.clear();
            self.transcript_pacer.clear();
            self.transcript_list.reset(0);
            self.open_recording = None;
            self.focused_event = None;
            self.clear_ask_selection(cx);
        }
        self.observed_completed_session = None;
        self.message = self
            .notes
            .update(cx, |notes, _| notes.refresh_catalogue())
            .err();
        if self.message.is_none() {
            self.rebuild_library_index(cx);
        }
        if let Some(entry_id) = owning_entry
            && let Some(next) = self
                .entries
                .iter()
                .find(|entry| entry.id() == entry_id)
                .and_then(|entry| entry.session_ids().last())
                .copied()
        {
            self.select_meeting(next, cx);
            return;
        }
        // Deleting what you were reading lands you on Home, not inside an unrelated recording the
        // shell picked for you. Same reasoning as launch: nothing here is the obvious next thing.
        self.sync_ask_scope(self.ask_ready(cx), cx);
        cx.notify();
    }

    /// What the confirm dialog says before it removes the open recording.
    ///
    /// The control is a glyph, so this sentence carries the whole of the target: which recording,
    /// and how much of this Mac goes with it. A red picture on its own says neither. A recording
    /// with no retained media says so rather than naming a size it does not have.
    fn delete_prompt(&self, id: SessionId, cx: &Context<Self>) -> String {
        let title = self
            .notes
            .read(cx)
            .snapshot()
            .meetings
            .into_iter()
            .find(|meeting| meeting.id == id)
            .map_or_else(
                || "this recording".to_owned(),
                |meeting| format!("“{}”", library::recording_name(&meeting)),
            );
        let keeps_entry = self
            .selected_entry
            .and_then(|entry_id| self.entries.iter().find(|entry| entry.id() == entry_id))
            .is_some_and(|entry| entry.session_ids().len() > 1);
        let consequence = if keeps_entry {
            "This removes only that recording and transcript; the entry, its other recordings and its notes stay."
        } else {
            "This removes the recording and transcript; its automatically created empty entry is removed too."
        };
        match self
            .open_recording
            .as_ref()
            .and_then(|recording| recording.byte_size)
        {
            Some(bytes) => format!(
                "Delete {title} and its {}? {consequence}",
                layout::format_bytes(bytes),
            ),
            None => format!("Delete {title}? It has no retained media. {consequence}"),
        }
    }

    pub(crate) fn delete_open_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry_id) = self.selected_entry else {
            return;
        };
        let Some(entry) = self.entries.iter().find(|entry| entry.id() == entry_id) else {
            return;
        };
        let name = library::entry_name(entry, &self.notes.read(cx).snapshot().meetings);
        let count = entry.session_ids().len();
        let prompt = format!(
            "Delete the entry “{name}”? This removes its {count} recording(s), transcripts, retained media and notes from this Mac."
        );
        let workspace = cx.entity().downgrade();
        open_confirm_delete_dialog(
            window,
            cx,
            "delete-entry",
            "Delete entry",
            &prompt,
            "Delete entry",
            move |_, cx| {
                let _ = workspace.update(cx, |workspace, cx| {
                    workspace.confirm_delete_entry(entry_id, cx);
                });
            },
        );
        cx.notify();
    }

    fn confirm_delete_entry(&mut self, entry_id: EntryId, cx: &mut Context<Self>) {
        self.cancel_pending_ask();
        let recording_directory = self
            .database
            .parent()
            .map_or_else(std::env::temp_dir, Path::to_path_buf)
            .join("recordings");
        let result = crate::persistence_runtime::block_on(async {
            rag::Store::open(&self.database)
                .await?
                .delete_entry(entry_id, &recording_directory)
                .await
        });
        match result {
            Ok(()) => {
                self.observed_completed_session = None;
                self.message = self
                    .notes
                    .update(cx, |notes, _| notes.refresh_catalogue())
                    .err();
                self.rebuild_library_index(cx);
                self.show_home(cx);
            }
            Err(error) => {
                self.message = Some(format!("Could not delete this entry: {error}"));
                cx.notify();
            }
        }
    }

    fn cancel_pending_ask(&mut self) {
        if let Some((pending, _)) = self.pending_ask.take() {
            pending.cancellation.cancel();
        }
    }

    /// Tells the Ask panel which recording is open, if any, and whether it is still running.
    ///
    /// Ask is an app-level surface, not a property of the open note: it answers from the whole
    /// library by default and narrows to one recording only because the person chose to. So the
    /// open recording is reported whether it is live or stopped — a question about the call you are
    /// in is the most natural question there is — and reporting nothing simply means Ask has no
    /// single-recording scope to offer, never that Ask is unavailable.
    fn sync_ask_scope(&mut self, ready: bool, cx: &mut Context<Self>) {
        let meetings = self.notes.read(cx).snapshot().meetings;
        let scope = self.selected_entry.and_then(|entry_id| {
            self.entries
                .iter()
                .find(|entry| entry.id() == entry_id)
                .map(|entry| {
                    (
                        entry.session_ids().to_vec(),
                        library::entry_name(entry, &meetings),
                    )
                })
        });
        let live = self.transcript_live;
        let selection = self.ask_selection.clone();
        let openai_ok = self.reasoning.read(cx).openai_selectable();
        let codex_ok = self.reasoning.read(cx).codex_selectable();
        let selected_provider = self
            .reasoning
            .read(cx)
            .selected_id(ReasoningSurface::Ask)
            .map(|id| id.as_str().to_owned());
        self.ask_panel.update(cx, |panel, _| {
            panel.set_entry_scope(scope, ready, live);
            panel.set_providers(openai_ok, codex_ok, selected_provider);
            panel.set_selection(selection);
        });
    }

    fn set_ask_selection(&mut self, selection: Option<ask::AskSelection>, cx: &mut Context<Self>) {
        if self.ask_selection == selection {
            return;
        }
        if self.ask_panel.read(cx).effective() == ask::AskScope::Selection
            && self.pending_ask.is_some()
        {
            self.cancel_ask(cx);
        }
        self.ask_selection = selection.clone();
        self.ask_panel
            .update(cx, |panel, _| panel.set_selection(selection));
        cx.notify();
    }

    fn clear_ask_selection(&mut self, cx: &mut Context<Self>) {
        self.set_ask_selection(None, cx);
    }

    /// Lands a citation on the transcript row that carries its evidence.
    ///
    /// The whole gesture — showing the Transcript tab, resolving the supersede chain, redirecting a
    /// folded non-speech moment to the row that renders it, scrolling, anchoring and flashing —
    /// lives in one place: [`MeetingWorkspace::reveal_citation`]. Every citation surface (the
    /// summary's chips, the Ask panel's answers) arrives here so none of them can drift into a
    /// half-gesture. A `false` return means the message already explains why nothing moved.
    pub fn open_citation(&mut self, event_id: EventId, cx: &mut Context<Self>) {
        if let Some(cited_session) = self.notes.read(cx).snapshot().selected_session
            && self.transcript_session != Some(cited_session)
        {
            self.select_meeting(cited_session, cx);
        }
        self.reveal_citation(event_id, cx);
    }

    pub(crate) fn follow_live(&mut self, cx: &mut Context<Self>) {
        self.follow_transcript = true;
        self.focused_event = None;
        self.clear_ask_selection(cx);
        self.transcript_list.scroll_to(gpui_kit::ListOffset {
            item_ix: self.transcript_pacer.rows().len(),
            offset_in_item: px(0.0),
        });
        cx.notify();
    }
    pub(crate) fn pause_follow_live(&mut self, cx: &mut Context<Self>) {
        if self.transcript_live {
            self.follow_transcript = false;
            cx.notify();
        }
    }
    pub(crate) fn start_scoped_session(&mut self, cx: &mut Context<Self>) {
        if let Some(entry_id) = self.selected_entry {
            self.session
                .update(cx, |session, cx| session.start_in_entry(entry_id, cx));
        } else {
            self.session.update(cx, SessionController::start);
        }
    }

    pub(crate) fn start_microphone_session(&mut self, cx: &mut Context<Self>) {
        if let Some(entry_id) = self.selected_entry {
            self.session.update(cx, |session, cx| {
                session.start_microphone_only_in_entry(entry_id, cx);
            });
        } else {
            self.session
                .update(cx, SessionController::start_microphone_only);
        }
    }

    pub(crate) fn stop_session(&mut self, cx: &mut Context<Self>) {
        self.session.update(cx, SessionController::stop);
    }

    pub(crate) fn retranscribe_selected(&mut self, cx: &mut Context<Self>) {
        if self.retranscription_running {
            return;
        }
        let Some(model_path) = self
            .session
            .read(cx)
            .transcription_model()
            .ready_path()
            .map(Path::to_path_buf)
        else {
            self.message = self.session.read(cx).transcription_unavailability();
            cx.notify();
            return;
        };
        let Some(session_id) = self.transcript_session.filter(|_| !self.transcript_live) else {
            self.message = Some("Select a stopped recording before re-transcribing.".to_owned());
            cx.notify();
            return;
        };
        self.retranscription_running = true;
        self.message = Some(
            "Re-transcribing from the retained local recording with the selected Whisper model…"
                .to_owned(),
        );
        let database = self.database.clone();
        let recording_directory = crate::session::application_recording_directory();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let spawn = std::thread::Builder::new()
            .name("sotto-retranscription".to_owned())
            .spawn(move || {
                let result = RecordingLibrary::new(database, recording_directory)
                    .retranscribe(session_id, &model_path);
                let _ = sender.send(result);
            });
        if let Err(error) = spawn {
            self.retranscription_running = false;
            self.message = Some(format!("Could not start re-transcription: {error}"));
            cx.notify();
            return;
        }

        let workspace = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(50)).await;
                match receiver.try_recv() {
                    Ok(result) => {
                        let _ = workspace.update(cx, |workspace, cx| {
                            workspace.retranscription_running = false;
                            match result {
                                Ok(count) => {
                                    if workspace.transcript_session == Some(session_id)
                                        && !workspace.transcript_live
                                    {
                                        workspace.load_transcript(session_id);
                                    }
                                    workspace.rebuild_library_index(cx);
                                    workspace.message = Some(format!(
                                        "Re-transcription complete: {count} transcript row(s) replaced from retained media."
                                    ));
                                }
                                Err(error) => {
                                    workspace.message =
                                        Some(format!("Re-transcription failed: {error}"));
                                }
                            }
                            cx.notify();
                        });
                        return;
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        let _ = workspace.update(cx, |workspace, cx| {
                            workspace.retranscription_running = false;
                            workspace.message = Some(
                                "Re-transcription stopped before reporting a result.".to_owned(),
                            );
                            cx.notify();
                        });
                        return;
                    }
                }
            }
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn toggle_source_resource(
        &mut self,
        server: mcp::ServerId,
        uri: mcp::ResourceUri,
        cx: &mut Context<Self>,
    ) {
        self.message = self
            .mcp
            .update(cx, |controller, cx| {
                controller.toggle_resource(server, uri, cx)
            })
            .err()
            .map(|error| error.to_string());
        cx.notify();
    }
    pub(crate) fn toggle_query_disclosure(
        &mut self,
        server: mcp::ServerId,
        enabled: bool,
        cx: &mut Context<Self>,
    ) {
        self.message = self
            .mcp
            .update(cx, |controller, cx| {
                controller.set_query_disclosure(server, enabled, cx)
            })
            .err()
            .map(|error| error.to_string());
        cx.notify();
    }

    /// Collapses or restores the library rail from the toolbar, and writes the choice down.
    ///
    /// The rail is 248 px — a quarter of a 1000 px window — and a person reading a transcript
    /// should be able to reclaim it. The control that does it lives in the toolbar and *stays
    /// there* in both states, marked selected while the rail is showing: collapsing something you
    /// cannot see the way back from is a trap, and a hover-to-reveal window edge is exactly that.
    fn toggle_library(&mut self, cx: &mut Context<Self>) {
        self.set_library_collapsed(!self.library_collapsed);
        cx.notify();
    }

    fn set_library_collapsed(&mut self, collapsed: bool) {
        if self.library_collapsed == collapsed {
            return;
        }
        self.library_collapsed = collapsed;
        self.persist_workspace_state();
    }

    fn toggle_ask(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.ask_open = !self.ask_open;
        self.persist_workspace_state();
        cx.notify();
    }

    /// The appearance currently in force, as `View ▸ Appearance` shows it.
    pub(crate) const fn appearance(&self) -> Appearance {
        Appearance::from_persisted(self.theme)
    }

    /// Applies an appearance chosen from the menu bar, records it, and re-marks the menu.
    ///
    /// Kit `Theme::change` is what both the shell's tokens and every stock control resolve against,
    /// so changing it repaints the whole window. "Follow System" is not a no-op: it adopts the
    /// system appearance now and, because the recorded choice is cleared, the window's appearance
    /// observer keeps following it afterwards.
    pub fn set_appearance(
        &mut self,
        appearance: Appearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.theme = appearance.persisted();
        match self.theme {
            Some(theme) => tokens::apply_theme(theme.mode(), Some(window), cx),
            None => tokens::sync_system_appearance(Some(window), cx),
        }
        tokens::install_visible_scrollbars(cx);
        self.persist_workspace_state();
        self.refresh_application_menus(cx);
        cx.notify();
    }

    /// Rebuilds the menu bar so `View ▸ Appearance` marks what is actually in force.
    fn refresh_application_menus(&self, cx: &App) {
        cx.set_menus(application_menus(self.appearance()));
    }

    fn persist_workspace_state(&mut self) {
        if let Err(error) = layout::save_workspace_state(
            &self.database,
            PersistedWorkspaceState {
                ask_open: self.ask_open,
                theme: self.theme,
                library_collapsed: self.library_collapsed,
            },
        ) {
            self.message = Some(error);
        }
    }

    /// Shows or hides the settings sheet **inside this window**.
    ///
    /// Both entry points land here: the title bar's gear and the `Sotto ▸ Settings…` menu item,
    /// which macOS users reach for whether or not the window carries a control. Neither opens a
    /// window; the mock's `#setScrim` is an overlay over the workspace it configures.
    pub fn toggle_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open {
            self.close_settings(window, cx);
        } else {
            self.open_settings(window, cx);
        }
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.settings.clone() {
            Some(existing) => existing.update(cx, |view, cx| view.reopen(window, cx)),
            None => {
                let reasoning = self.reasoning.clone();
                let mcp = self.mcp.clone();
                let database = self.database.clone();
                let view = cx.new(|cx| SettingsView::new(window, reasoning, mcp, database, cx));
                self._subscriptions.push(cx.subscribe_in(
                    &view,
                    window,
                    |this, _, SettingsEvent::Dismissed, window, cx| {
                        this.close_settings(window, cx);
                    },
                ));
                self.settings = Some(view);
            }
        }
        self.settings_open = true;
        cx.notify();
    }

    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.settings_open {
            return;
        }
        self.settings_open = false;
        // The sheet held focus so Escape reached it. Release it rather than leaving focus parked on
        // an element that is no longer rendered.
        window.blur(cx);
        // Settings can delete retained media, so the shell re-measures what it claims about the
        // library and about the open recording rather than keeping a figure that just stopped
        // being true.
        self.rebuild_library_index(cx);
        if let Some(id) = self.transcript_session.filter(|_| !self.transcript_live) {
            self.open_recording = self.read_open_recording(id);
        }
        cx.notify();
    }

    /// Rebuilds everything the rail derives from the catalogue: the search index, and the measured
    /// footprint Home reports. Both are computed here rather than per frame — each one opens the
    /// store — and both are refreshed on exactly the events that change the catalogue.
    fn rebuild_library_index(&mut self, cx: &Context<Self>) {
        let meetings = self.notes.read(cx).snapshot().meetings;
        self.library_index = library::search_index(&self.database, &meetings);
        self.entries = crate::persistence_runtime::block_on(async {
            rag::Store::open(&self.database).await?.list_entries().await
        })
        .unwrap_or_default();
        self.entry_library_index =
            library::entry_search_index(&self.database, &self.entries, &meetings);
        self.library_footprint =
            library::LibraryFootprint::measure_entries(&self.database, &self.entries, &meetings);
    }

    fn entry_id_for_session(&self, session_id: SessionId) -> Option<EntryId> {
        self.entries
            .iter()
            .find(|entry| entry.session_ids().contains(&session_id))
            .map(Entry::id)
            .or_else(|| {
                crate::persistence_runtime::block_on(async {
                    rag::Store::open(&self.database)
                        .await?
                        .entry_for_session(session_id)
                        .await
                })
                .ok()
            })
    }

    fn poll_session_clock(&self, cx: &mut Context<Self>) {
        let workspace = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                if workspace.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        })
        .detach();
    }
}
