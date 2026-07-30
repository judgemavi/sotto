//! Settings state. Secrets are write-only and delegated to the OS credential store.

use futures_util::StreamExt;
use providers::{Provider, ProviderKind, Role};
use secrecy::SecretString;
use std::{collections::HashMap, time::Duration};

use gpui::{Context, Entity, IntoElement, Render, Timer, Window, div, prelude::*, rgb};
use gpui_component::button::Button;
use gpui_component::input::{Input, InputState};
use sotto_core::{CompletionMessage, CompletionRequest, MessageRole, ProviderError};

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KeyValidationStatus {
    Idle,
    Validating,
    Valid,
    BadKey,
    KeychainLocked,
    NetworkDown,
    RateLimited,
    Failed(String),
}

impl KeyValidationStatus {
    #[must_use]
    pub fn from_error(error: ProviderError) -> Self {
        match error {
            ProviderError::Auth => Self::BadKey,
            ProviderError::CredentialStore(_) => Self::KeychainLocked,
            ProviderError::Network(_) => Self::NetworkDown,
            ProviderError::RateLimit { .. } => Self::RateLimited,
            other => Self::Failed(other.to_string()),
        }
    }

    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Idle => "No validation run",
            Self::Validating => "Checking with provider…",
            Self::Valid => "Key is valid",
            Self::BadKey => "This key was rejected. Replace it and try again.",
            Self::KeychainLocked => "Keychain is locked. Unlock it and try again.",
            Self::NetworkDown => "Provider could not be reached. Check the network and retry.",
            Self::RateLimited => "Provider rate limit reached. Retry later.",
            Self::Failed(message) => message,
        }
    }
}

pub struct SettingsView {
    pub state: SettingsState,
    pub capture: Option<CaptureIndicator>,
    pub mic_level: f32,
    pub target_level: f32,
    provider: ProviderKind,
    key_input: Option<Entity<InputState>>,
    key_status: KeyValidationStatus,
    session_message: String,
}

impl Default for SettingsView {
    fn default() -> Self {
        Self {
            state: SettingsState::default(),
            capture: None,
            mic_level: 0.0,
            target_level: 0.0,
            provider: ProviderKind::Anthropic,
            key_input: None,
            key_status: KeyValidationStatus::Idle,
            session_message: "Capture is stopped".to_owned(),
        }
    }
}

impl SettingsView {
    #[must_use]
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            key_input: Some(cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Paste key (never shown again)")
                    .masked(true)
            })),
            ..Self::default()
        }
    }

    fn cycle_provider(&mut self, cx: &mut Context<Self>) {
        self.provider = match self.provider {
            ProviderKind::Anthropic => ProviderKind::OpenAi,
            ProviderKind::OpenAi => ProviderKind::Google,
            ProviderKind::Google => ProviderKind::OpenRouter,
            ProviderKind::OpenRouter => ProviderKind::Ollama,
            ProviderKind::Ollama => ProviderKind::Anthropic,
        };
        self.key_status = KeyValidationStatus::Idle;
        cx.notify();
    }

    fn save_input_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.key_input.as_ref() else {
            return;
        };
        let value = input.read(cx).value().to_string();
        let status = if value.is_empty() {
            KeyValidationStatus::BadKey
        } else {
            SettingsState::store_submitted_key(self.provider, SecretString::from(value))
                .map_or_else(KeyValidationStatus::from_error, |()| {
                    KeyValidationStatus::Idle
                })
        };
        // Erase the UI copy immediately. Stored credentials can only be read by the provider
        // path; this view never loads or renders them back.
        input.update(cx, |input, cx| input.set_value("", window, cx));
        self.key_status = status;
        cx.notify();
    }

    fn delete_selected_key(&mut self, cx: &mut Context<Self>) {
        self.key_status = SettingsState::delete_key(self.provider)
            .map_or_else(KeyValidationStatus::from_error, |()| {
                KeyValidationStatus::Idle
            });
        cx.notify();
    }

    fn cycle_speculation(&mut self, cx: &mut Context<Self>) {
        self.state.speculation = match self.state.speculation {
            SpeculationLevel::Off => SpeculationLevel::Conservative,
            SpeculationLevel::Conservative => SpeculationLevel::Balanced,
            SpeculationLevel::Balanced => SpeculationLevel::Aggressive,
            SpeculationLevel::Aggressive => SpeculationLevel::Off,
        };
        cx.notify();
    }

    fn apply_provider_to_role(&mut self, role: Role, cx: &mut Context<Self>) {
        self.state.select_model(
            role,
            ModelChoice {
                provider: self.provider,
                model: default_model(self.provider).to_owned(),
            },
        );
        cx.notify();
    }

    fn select_default_audio(&mut self, cx: &mut Context<Self>) {
        self.state.mic_device = Some("System default microphone".to_owned());
        self.state.target_audio_device = Some("Selected target audio".to_owned());
        cx.notify();
    }

    fn validate_selected_key(&mut self, cx: &mut Context<Self>) {
        if self.key_status == KeyValidationStatus::Validating {
            return;
        }
        self.key_status = KeyValidationStatus::Validating;
        let provider = self.provider;
        let model = default_model(provider).to_owned();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let status = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(validate_provider_key(provider, model)),
                Err(error) => {
                    KeyValidationStatus::Failed(format!("Could not start validation: {error}"))
                }
            };
            let _ = sender.send(status);
        });
        let view = cx.entity();
        cx.spawn(async move |_, cx| {
            loop {
                if let Ok(status) = receiver.try_recv() {
                    let _ = view.update(cx, |view, cx| {
                        view.key_status = status;
                        cx.notify();
                    });
                    return;
                }
                Timer::after(Duration::from_millis(50)).await;
            }
        })
        .detach();
        cx.notify();
    }
}

const fn default_model(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Anthropic => "claude-3-5-haiku-latest",
        ProviderKind::OpenAi => "gpt-4.1-nano",
        ProviderKind::Google => "gemini-2.0-flash-lite",
        ProviderKind::OpenRouter => "openai/gpt-4.1-nano",
        ProviderKind::Ollama => "llama3.2:1b",
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            .child(Button::new("cycle-provider").label(format!("Provider: {:?}", self.provider)).on_click(
                cx.listener(|this, _, _, cx| this.cycle_provider(cx)),
            ))
            .when_some(self.key_input.as_ref(), |view, input| {
                view.child(Input::new(input))
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(Button::new("provider-key").label("Save key").on_click(cx.listener(
                        |this, _, window, cx| this.save_input_key(window, cx),
                    )))
                    .child(Button::new("delete-key").label("Delete key").on_click(cx.listener(
                        |this, _, _, cx| this.delete_selected_key(cx),
                    )))
                    .child(
                        Button::new("validate-key")
                            .label("Validate with test call")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.validate_selected_key(cx);
                            })),
                    ),
            )
            .child(self.key_status.label().to_owned())
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(Button::new("watcher-model").label("Use for watcher").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.apply_provider_to_role(Role::Watcher, cx);
                        }),
                    ))
                    .child(Button::new("suggester-model").label("Use for suggester").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.apply_provider_to_role(Role::Suggester, cx);
                        }),
                    ))
                    .child(Button::new("summarizer-model").label("Use for summarizer").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.apply_provider_to_role(Role::Summarizer, cx);
                        }),
                    )),
            )
            .child(section(
                "Audio",
                &format!(
                    "Microphone level {:.0}% · target audio level {:.0}%",
                    self.mic_level.clamp(0.0, 1.0) * 100.0,
                    self.target_level.clamp(0.0, 1.0) * 100.0
                ),
            ))
            .child(Button::new("audio-defaults").label("Use system audio defaults").on_click(
                cx.listener(|this, _, _, cx| this.select_default_audio(cx)),
            ))
            .child(section(
                "Session",
                "Starting is always explicit: turn Sotto on, then choose an app or window in the system picker.",
            ))
            .child(Button::new("start-session").label("Turn on and choose target…"))
            .child(self.session_message.clone())
            .child(section(
                "Speculation",
                "More aggressive speculation can respond sooner, but may spend more of your provider tokens.",
            ))
            .child(Button::new("speculation").label(format!("Level: {:?}", self.state.speculation)).on_click(
                cx.listener(|this, _, _, cx| this.cycle_speculation(cx)),
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
    fn store_submitted_key(kind: ProviderKind, key: SecretString) -> Result<(), ProviderError> {
        Self::save_key(kind, &key)
    }
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

/// Makes the cheapest real completion call supported by the selected provider.
///
/// Credentials stay wrapped in `SecretString`, are loaded only for this call, and are never
/// returned to UI state. Ollama deliberately follows the same health-check path without a key.
pub async fn validate_provider_key(
    kind: ProviderKind,
    model: impl Into<String>,
) -> KeyValidationStatus {
    let key = match kind {
        ProviderKind::Ollama => None,
        _ => match providers::load_key(kind) {
            Ok(Some(key)) => Some(key),
            Ok(None) => return KeyValidationStatus::BadKey,
            Err(error) => return KeyValidationStatus::from_error(error),
        },
    };
    let model = model.into();
    let provider = Provider::new(kind, model.clone(), key);
    let request = CompletionRequest {
        model,
        system: None,
        messages: vec![CompletionMessage {
            role: MessageRole::User,
            content: "Reply OK".to_owned(),
            cache_boundary: false,
        }],
        max_tokens: Some(1),
        temperature: Some(0.0),
        stop: Vec::new(),
    };
    let mut stream = match provider.start(request) {
        Ok(call) => match call.stream().await {
            Ok(stream) => stream,
            Err(error) => return KeyValidationStatus::from_error(error),
        },
        Err(error) => return KeyValidationStatus::from_error(error),
    };
    if let Some(delta) = stream.next().await {
        return match delta {
            Ok(_) => KeyValidationStatus::Valid,
            Err(error) => KeyValidationStatus::from_error(error),
        };
    }
    KeyValidationStatus::Failed("Provider returned no response".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{CaptureIndicator, KeyValidationStatus};
    use sotto_core::ProviderError;

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

    #[test]
    fn key_validation_errors_keep_actionable_distinctions() {
        assert_eq!(
            KeyValidationStatus::from_error(ProviderError::Auth),
            KeyValidationStatus::BadKey
        );
        assert_eq!(
            KeyValidationStatus::from_error(ProviderError::CredentialStore("locked".to_owned())),
            KeyValidationStatus::KeychainLocked
        );
        assert_eq!(
            KeyValidationStatus::from_error(ProviderError::Network("offline".to_owned())),
            KeyValidationStatus::NetworkDown
        );
    }
}
