//! Settings state. Secrets are write-only and delegated to the OS credential store.

use providers::{ProviderKind, Role};
use secrecy::SecretString;
use std::collections::HashMap;

use gpui::{Context, IntoElement, Render, Window, div, prelude::*, rgb};
use gpui_component::button::Button;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SpeculationLevel {
    Off,
    Conservative,
    #[default]
    Balanced,
    Aggressive,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelChoice {
    pub provider: ProviderKind,
    pub model: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureIndicator {
    pub target: String,
    pub screen_scoped: bool,
    pub audio_scoped: bool,
}

impl CaptureIndicator {
    #[must_use]
    pub fn label(&self) -> String {
        let screen = if self.screen_scoped {
            &self.target
        } else {
            "system"
        };
        let audio = if self.audio_scoped {
            &self.target
        } else {
            "system"
        };
        format!("Recording · screen: {screen} · audio: {audio}")
    }
}

#[derive(Default)]
pub struct SettingsState {
    models: HashMap<Role, ModelChoice>,
    pub mic_device: Option<String>,
    pub target_audio_device: Option<String>,
    pub speculation: SpeculationLevel,
}

pub struct SettingsView {
    pub state: SettingsState,
    pub capture: Option<CaptureIndicator>,
    pub mic_level: f32,
    pub target_level: f32,
}

impl Default for SettingsView {
    fn default() -> Self {
        Self {
            state: SettingsState::default(),
            capture: None,
            mic_level: 0.0,
            target_level: 0.0,
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let indicator = self.capture.as_ref().map(CaptureIndicator::label);
        div()
            .size_full()
            .p_6()
            .gap_4()
            .flex()
            .flex_col()
            .bg(rgb(0x111318))
            .text_color(rgb(0xe6e8eb))
            .when_some(indicator, |view, label| {
                view.child(
                    div()
                        .p_3()
                        .rounded_lg()
                        .bg(rgb(0x6b241f))
                        .child(label),
                )
            })
            .child(section(
                "Models",
                "Watcher, suggester and summarizer are selected independently. Ollama needs no key.",
            ))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(Button::new("provider-key").label("Add or replace key"))
                    .child(Button::new("delete-key").label("Delete key"))
                    .child(Button::new("validate-key").label("Validate with test call")),
            )
            .child(section(
                "Audio",
                &format!(
                    "Microphone level {:.0}% · target audio level {:.0}%",
                    self.mic_level.clamp(0.0, 1.0) * 100.0,
                    self.target_level.clamp(0.0, 1.0) * 100.0
                ),
            ))
            .child(section(
                "Session",
                "Starting is always explicit: turn Sotto on, then choose an app or window in the system picker.",
            ))
            .child(Button::new("start-session").label("Turn on and choose target…"))
            .child(section(
                "Speculation",
                "More aggressive speculation can respond sooner, but may spend more of your provider tokens.",
            ))
    }
}

fn section(title: &'static str, detail: &str) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_lg().child(title))
        .child(div().text_color(rgb(0xaeb4be)).child(detail.to_owned()))
}

impl SettingsState {
    pub fn save_key(
        kind: ProviderKind,
        key: &SecretString,
    ) -> Result<(), sotto_core::ProviderError> {
        providers::store_key(kind, key)
    }

    pub fn delete_key(kind: ProviderKind) -> Result<(), sotto_core::ProviderError> {
        providers::delete_key(kind)
    }

    pub fn has_key(kind: ProviderKind) -> Result<bool, sotto_core::ProviderError> {
        providers::has_key(kind)
    }

    pub fn select_model(&mut self, role: Role, choice: ModelChoice) {
        self.models.insert(role, choice);
    }

    #[must_use]
    pub fn model(&self, role: Role) -> Option<&ModelChoice> {
        self.models.get(&role)
    }
}

#[cfg(test)]
mod tests {
    use super::CaptureIndicator;

    #[test]
    fn indicator_names_scope_truthfully() {
        let indicator = CaptureIndicator {
            target: "Zoom — Acme".to_owned(),
            screen_scoped: true,
            audio_scoped: false,
        };
        assert_eq!(
            indicator.label(),
            "Recording · screen: Zoom — Acme · audio: system"
        );
    }
}
