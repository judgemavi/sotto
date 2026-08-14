//! One shared product controller for optional, call-pinned reasoning backends.

pub mod inspection;
mod persistence;

use std::{
    fmt,
    path::PathBuf,
    sync::{Arc, mpsc},
    thread::JoinHandle,
};

use futures_util::StreamExt;
use gpui::Context;
use providers::{
    AuthStatus, BackendDescriptor, BackendId, CODEX_CLI_BACKEND_ID, OPENAI_RESPONSES_BACKEND_ID,
    Registry, RegistryError, ResolvedBackend, Role,
    codex::{CodexDescriptorMode, CodexProbe, CodexProvider},
    openai::OpenAiProvider,
};
use secrecy::SecretString;
use sotto_core::{
    CancellationToken, CompletionMessage, CompletionProvider, CompletionRequest, MessageRole,
    ProviderError,
};

use persistence::{PersistedBackends, PersistedRoles, PersistedSettings};

pub const DEFAULT_OPENAI_MODEL: &str = "gpt-4.1-nano";
pub const DEFAULT_CODEX_MODEL: &str = "gpt-5.6-luna";
const SETTINGS_VERSION: u32 = 1;
const CODEX_CONSENT_VERSION: u32 = 1;
const CODEX_NOT_CHECKED_REASON: &str = "Codex CLI readiness has not been checked yet";

pub trait CodexProbeSource: Send + Sync {
    fn probe(&self, cancellation: CancellationToken) -> CodexProbe;
}

struct InstalledCodexProbeSource;

impl CodexProbeSource for InstalledCodexProbeSource {
    fn probe(&self, cancellation: CancellationToken) -> CodexProbe {
        let provider = CodexProvider::installed(DEFAULT_CODEX_MODEL);
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(async move {
                tokio::select! {
                    _ = cancellation.cancelled() => unavailable_codex_probe("Codex readiness check was cancelled"),
                    probe = provider.probe() => probe,
                }
            }),
            Err(error) => CodexProbe {
                version: None,
                login_status: AuthStatus::Unavailable {
                    reason: format!("Codex readiness check could not start: {error}"),
                },
            },
        }
    }
}

fn unavailable_codex_probe(reason: &str) -> CodexProbe {
    CodexProbe {
        version: None,
        login_status: AuthStatus::Unavailable {
            reason: reason.to_owned(),
        },
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodexReadiness {
    NotChecked,
    Checking,
    NotInstalled,
    NeedsLogin,
    Ready { version: Option<String> },
    UnsupportedLogin(String),
    Unavailable(String),
}

impl CodexReadiness {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::NotChecked => "Codex CLI has not been checked yet.".to_owned(),
            Self::Checking => "Checking the Codex CLI and ChatGPT login…".to_owned(),
            Self::NotInstalled => {
                "Codex CLI was not found. Install it, then check again.".to_owned()
            }
            Self::NeedsLogin => {
                "Codex CLI is installed but signed out. Run `codex login`, then check again."
                    .to_owned()
            }
            Self::Ready {
                version: Some(version),
            } => format!("Ready — authenticated with ChatGPT ({version}). No API key is required."),
            Self::Ready { version: None } => {
                "Ready — authenticated with ChatGPT. No API key is required.".to_owned()
            }
            Self::UnsupportedLogin(reason) => {
                format!("Codex login is not a ChatGPT subscription login: {reason}")
            }
            Self::Unavailable(reason) => format!("Codex readiness check failed: {reason}"),
        }
    }
}

pub trait OpenAiCredentialStore: Send + Sync {
    fn store(&self, key: &SecretString) -> Result<(), ProviderError>;
    fn load(&self) -> Result<Option<SecretString>, ProviderError>;
    fn delete(&self) -> Result<(), ProviderError>;
}

pub struct KeychainOpenAiCredentialStore;

impl OpenAiCredentialStore for KeychainOpenAiCredentialStore {
    fn store(&self, key: &SecretString) -> Result<(), ProviderError> {
        providers::openai::store_api_key(key)
    }

    fn load(&self) -> Result<Option<SecretString>, ProviderError> {
        providers::openai::load_api_key()
    }

    fn delete(&self) -> Result<(), ProviderError> {
        providers::openai::delete_api_key()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenAiReadiness {
    NoKey,
    Stored,
    Validating,
    Ready,
    BadKey,
    CredentialStore(String),
    NetworkDown,
    RateLimited,
    Failed(String),
}

impl OpenAiReadiness {
    #[must_use]
    pub fn from_provider_error(error: ProviderError) -> Self {
        match error {
            ProviderError::Auth => Self::BadKey,
            ProviderError::CredentialStore(reason) => Self::CredentialStore(reason),
            ProviderError::Network(_) => Self::NetworkDown,
            ProviderError::RateLimit { .. } => Self::RateLimited,
            other => Self::Failed(other.to_string()),
        }
    }

    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::NoKey => "No OpenAI API key is stored.".to_owned(),
            Self::Stored => "API key stored in Keychain; not validated yet.".to_owned(),
            Self::Validating => "Checking the key with OpenAI…".to_owned(),
            Self::Ready => "OpenAI API is ready.".to_owned(),
            Self::BadKey => "OpenAI rejected this key. Replace it and retry.".to_owned(),
            Self::CredentialStore(reason) => format!("Keychain error: {reason}"),
            Self::NetworkDown => "OpenAI could not be reached. Check the network.".to_owned(),
            Self::RateLimited => "OpenAI rate limit reached. Retry later.".to_owned(),
            Self::Failed(reason) => format!("OpenAI validation failed: {reason}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReasoningError {
    Persistence(String),
    Credential(String),
    Contract(String),
    Unavailable(String),
    ValidationInProgress,
    CodexProbeInProgress,
}

impl fmt::Display for ReasoningError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Persistence(reason) => write!(formatter, "settings persistence failed: {reason}"),
            Self::Credential(reason) => write!(formatter, "credential operation failed: {reason}"),
            Self::Contract(reason) => write!(formatter, "reasoning configuration failed: {reason}"),
            Self::Unavailable(reason) => formatter.write_str(reason),
            Self::ValidationInProgress => {
                formatter.write_str("OpenAI validation is already in progress")
            }
            Self::CodexProbeInProgress => {
                formatter.write_str("Codex readiness is already being checked")
            }
        }
    }
}

impl std::error::Error for ReasoningError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationOutcome {
    Completed(OpenAiReadiness),
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationResult {
    generation: u64,
    outcome: ValidationOutcome,
}

pub struct ValidationTicket {
    generation: u64,
    cancellation: CancellationToken,
    receiver: mpsc::Receiver<ValidationResult>,
}

impl ValidationTicket {
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub fn try_recv(&self) -> Result<ValidationResult, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }
}

impl Drop for ValidationTicket {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

struct ValidationAttempt {
    generation: u64,
    cancellation: CancellationToken,
    prior_readiness: OpenAiReadiness,
    worker: JoinHandle<()>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodexProbeResult {
    generation: u64,
    probe: CodexProbe,
}

pub struct CodexProbeTicket {
    generation: u64,
    cancellation: CancellationToken,
    receiver: mpsc::Receiver<CodexProbeResult>,
}

impl Drop for CodexProbeTicket {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl CodexProbeTicket {
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn try_recv(&self) -> Result<CodexProbeResult, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }
}

struct CodexProbeAttempt {
    generation: u64,
    cancellation: CancellationToken,
    worker: JoinHandle<()>,
}

pub struct ReasoningController {
    registry: Registry,
    settings: PersistedSettings,
    settings_path: Option<PathBuf>,
    credentials: Arc<dyn OpenAiCredentialStore>,
    codex_probe_source: Arc<dyn CodexProbeSource>,
    codex_probe: CodexProbe,
    codex_probe_generation: u64,
    codex_probe_attempt: Option<CodexProbeAttempt>,
    retired_codex_probe_workers: Vec<JoinHandle<()>>,
    openai_readiness: OpenAiReadiness,
    status_message: Option<String>,
    runtime_revision: u64,
    validation_generation: u64,
    validation_attempt: Option<ValidationAttempt>,
    retired_validation_workers: Vec<JoinHandle<()>>,
}

impl ReasoningController {
    #[must_use]
    pub fn load_default() -> Self {
        let codex_probe_source = Arc::new(InstalledCodexProbeSource);
        match persistence::application_support_path() {
            Ok(path) => Self::load_with_probe_source(
                path,
                Arc::new(KeychainOpenAiCredentialStore),
                codex_probe_source,
            ),
            Err(error) => Self::load_inner(
                None,
                Arc::new(KeychainOpenAiCredentialStore),
                codex_probe_source,
                Some(error.to_string()),
            ),
        }
    }

    #[must_use]
    pub fn load(path: PathBuf, credentials: Arc<dyn OpenAiCredentialStore>) -> Self {
        Self::load_with_probe_source(path, credentials, Arc::new(InstalledCodexProbeSource))
    }

    #[must_use]
    pub fn load_with_probe_source(
        path: PathBuf,
        credentials: Arc<dyn OpenAiCredentialStore>,
        codex_probe_source: Arc<dyn CodexProbeSource>,
    ) -> Self {
        Self::load_inner(Some(path), credentials, codex_probe_source, None)
    }

    fn load_inner(
        settings_path: Option<PathBuf>,
        credentials: Arc<dyn OpenAiCredentialStore>,
        codex_probe_source: Arc<dyn CodexProbeSource>,
        initial_message: Option<String>,
    ) -> Self {
        let (settings, mut status_message) = match settings_path.as_deref() {
            Some(path) => match persistence::load(path) {
            Ok(Some(settings)) if settings.version == SETTINGS_VERSION => (settings, None),
            Ok(Some(_)) => (
                Self::default_settings(),
                Some(
                    "Reasoning settings used an unsupported version; no reasoning was selected."
                        .to_owned(),
                ),
            ),
            Ok(None) => (Self::default_settings(), None),
            Err(error) => (Self::default_settings(), Some(error.to_string())),
            },
            None => (Self::default_settings(), initial_message),
        };
        let loaded_key = credentials.load();
        let (key, readiness) = match loaded_key {
            Ok(Some(key)) => (Some(key), OpenAiReadiness::Stored),
            Ok(None) => (None, OpenAiReadiness::NoKey),
            Err(error) => {
                status_message = Some(error.to_string());
                (None, OpenAiReadiness::from_provider_error(error))
            }
        };
        let codex_probe = CodexProbe {
            version: None,
            login_status: AuthStatus::Unavailable {
                reason: CODEX_NOT_CHECKED_REASON.to_owned(),
            },
        };
        let (registry, normalized, registry_error) =
            Self::build_registry(settings, key, &codex_probe);
        if let Some(error) = registry_error {
            status_message = Some(error);
        }
        Self {
            registry,
            settings: normalized,
            settings_path,
            credentials,
            codex_probe_source,
            codex_probe,
            codex_probe_generation: 0,
            codex_probe_attempt: None,
            retired_codex_probe_workers: Vec::new(),
            openai_readiness: readiness,
            status_message,
            runtime_revision: 0,
            validation_generation: 0,
            validation_attempt: None,
            retired_validation_workers: Vec::new(),
        }
    }

    fn default_settings() -> PersistedSettings {
        PersistedSettings {
            version: SETTINGS_VERSION,
            backends: PersistedBackends {
                openai_responses_model_id: DEFAULT_OPENAI_MODEL.to_owned(),
                codex_model_id: DEFAULT_CODEX_MODEL.to_owned(),
                codex_experimental_consent_version: None,
            },
            roles: PersistedRoles::default(),
        }
    }

    fn build_registry(
        mut settings: PersistedSettings,
        key: Option<SecretString>,
        codex_probe: &CodexProbe,
    ) -> (Registry, PersistedSettings, Option<String>) {
        if settings
            .backends
            .openai_responses_model_id
            .trim()
            .is_empty()
        {
            settings.backends.openai_responses_model_id = DEFAULT_OPENAI_MODEL.to_owned();
        }
        if settings.backends.codex_model_id.trim().is_empty() {
            settings.backends.codex_model_id = DEFAULT_CODEX_MODEL.to_owned();
        }
        let mut registry = Registry::default();
        let mut error = None;

        let consented =
            settings.backends.codex_experimental_consent_version == Some(CODEX_CONSENT_VERSION);
        let codex = CodexProvider::installed(settings.backends.codex_model_id.clone());
        let codex = if consented {
            codex.with_experimental_user_opt_in()
        } else {
            codex
        };
        let codex_mode = if consented {
            CodexDescriptorMode::ExperimentalUserOptIn
        } else {
            CodexDescriptorMode::RequireVerifiedIsolation
        };
        match codex.descriptor_with_mode(codex_probe, codex_mode) {
            Ok(descriptor) => {
                if let Err(cause) = registry.register_reasoning(descriptor, Arc::new(codex)) {
                    error = Some(cause.to_string());
                }
            }
            Err(cause) => error = Some(cause.to_string()),
        }

        let openai = OpenAiProvider::new(settings.backends.openai_responses_model_id.clone(), key);
        match openai.descriptor() {
            Ok(descriptor) => {
                if let Err(cause) = registry.register_reasoning(descriptor, Arc::new(openai)) {
                    error = Some(cause.to_string());
                }
            }
            Err(cause) => error = Some(cause.to_string()),
        }

        for role in [Role::Watcher, Role::Suggester, Role::Summarizer] {
            let selected = Self::persisted_role(&settings.roles, role).cloned();
            let Some(selected) = selected else {
                continue;
            };
            if selected == CODEX_CLI_BACKEND_ID && role != Role::Summarizer {
                Self::set_persisted_role(&mut settings.roles, role, None);
                error = Some(
                    "Codex experimental is notes-only; realtime roles now use no reasoning."
                        .to_owned(),
                );
                continue;
            }
            let id = match BackendId::new(selected.clone()) {
                Ok(id)
                    if id.as_str() == OPENAI_RESPONSES_BACKEND_ID
                        || (id.as_str() == CODEX_CLI_BACKEND_ID
                            && settings.backends.codex_experimental_consent_version
                                == Some(CODEX_CONSENT_VERSION)) =>
                {
                    id
                }
                _ => {
                    Self::set_persisted_role(&mut settings.roles, role, None);
                    error = Some(format!(
                        "Stored backend {selected:?} is not selectable; {role:?} now uses no reasoning."
                    ));
                    continue;
                }
            };
            if let Err(cause) = registry.select(role, Some(&id)) {
                if id.as_str() == CODEX_CLI_BACKEND_ID {
                    // Preserve an explicitly selected Codex role while the bounded startup
                    // readiness probe is pending or the user needs to sign in again.
                    continue;
                }
                Self::set_persisted_role(&mut settings.roles, role, None);
                error = Some(cause.to_string());
            }
        }
        (registry, settings, error)
    }

    fn persisted_role(roles: &PersistedRoles, role: Role) -> Option<&String> {
        match role {
            Role::Watcher => roles.watcher.as_ref(),
            Role::Suggester => roles.suggester.as_ref(),
            Role::Summarizer => roles.summarizer.as_ref(),
        }
    }

    fn set_persisted_role(roles: &mut PersistedRoles, role: Role, value: Option<String>) {
        match role {
            Role::Watcher => roles.watcher = value,
            Role::Suggester => roles.suggester = value,
            Role::Summarizer => roles.summarizer = value,
        }
    }

    pub fn resolve(&self, role: Role) -> Result<Option<ResolvedBackend>, RegistryError> {
        self.registry.resolve(role)
    }

    #[must_use]
    pub fn selected_id(&self, role: Role) -> Option<&BackendId> {
        self.registry.selected_id(role)
    }

    #[must_use]
    pub fn descriptor(&self, id: &BackendId) -> Option<&BackendDescriptor> {
        self.registry.descriptor(id)
    }

    #[must_use]
    pub fn openai_model(&self) -> &str {
        &self.settings.backends.openai_responses_model_id
    }

    #[must_use]
    pub fn codex_model(&self) -> &str {
        &self.settings.backends.codex_model_id
    }

    #[must_use]
    pub fn codex_experimental_enabled(&self) -> bool {
        self.settings.backends.codex_experimental_consent_version == Some(CODEX_CONSENT_VERSION)
    }

    #[must_use]
    pub fn codex_readiness(&self) -> CodexReadiness {
        if self.codex_probe_attempt.is_some() {
            return CodexReadiness::Checking;
        }
        match &self.codex_probe.login_status {
            AuthStatus::Ready => CodexReadiness::Ready {
                version: self.codex_probe.version.clone(),
            },
            AuthStatus::NeedsLogin => CodexReadiness::NeedsLogin,
            AuthStatus::Failed { reason } => CodexReadiness::UnsupportedLogin(reason.clone()),
            AuthStatus::Unavailable { reason } if reason == CODEX_NOT_CHECKED_REASON => {
                CodexReadiness::NotChecked
            }
            AuthStatus::Unavailable { reason }
                if reason.contains("could not be started") =>
            {
                CodexReadiness::NotInstalled
            }
            AuthStatus::Unavailable { reason } => CodexReadiness::Unavailable(reason.clone()),
            AuthStatus::NeedsApiKey => CodexReadiness::UnsupportedLogin(
                "the CLI reported API-key authentication; run `codex login` to use subscription access"
                    .to_owned(),
            ),
            _ => CodexReadiness::Unavailable(
                "the installed CLI returned an unsupported authentication state".to_owned(),
            ),
        }
    }

    #[must_use]
    pub const fn openai_readiness(&self) -> &OpenAiReadiness {
        &self.openai_readiness
    }

    #[must_use]
    pub fn status_message(&self) -> Option<&str> {
        self.status_message.as_deref()
    }

    pub fn apply_to_all(
        &mut self,
        backend: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Result<(), ReasoningError> {
        let before = self.observable_state();
        let result = self.apply_to_all_inner(backend);
        self.notify_if_changed(before, cx);
        result
    }

    fn apply_to_all_inner(&mut self, backend: Option<&str>) -> Result<(), ReasoningError> {
        self.invalidate_validation();
        let mut candidate = self.settings.clone();
        if backend == Some(CODEX_CLI_BACKEND_ID) {
            return Err(ReasoningError::Contract(
                "Codex experimental is notes-only and cannot be applied to realtime roles."
                    .to_owned(),
            ));
        }
        for role in [Role::Watcher, Role::Suggester, Role::Summarizer] {
            Self::set_persisted_role(&mut candidate.roles, role, backend.map(ToOwned::to_owned));
        }
        self.commit_settings(candidate)
    }

    pub fn select_role(
        &mut self,
        role: Role,
        backend: Option<&str>,
        cx: &mut Context<Self>,
    ) -> Result<(), ReasoningError> {
        let before = self.observable_state();
        let result = self.select_role_inner(role, backend);
        self.notify_if_changed(before, cx);
        result
    }

    fn select_role_inner(
        &mut self,
        role: Role,
        backend: Option<&str>,
    ) -> Result<(), ReasoningError> {
        self.invalidate_validation();
        if backend == Some(CODEX_CLI_BACKEND_ID) && role != Role::Summarizer {
            return Err(ReasoningError::Contract(
                "Codex experimental is available only for meeting notes.".to_owned(),
            ));
        }
        let mut candidate = self.settings.clone();
        Self::set_persisted_role(&mut candidate.roles, role, backend.map(ToOwned::to_owned));
        self.commit_settings(candidate)
    }

    pub fn set_openai_model(
        &mut self,
        model: String,
        cx: &mut Context<Self>,
    ) -> Result<(), ReasoningError> {
        let before = self.observable_state();
        let result = self.set_openai_model_inner(model);
        self.notify_if_changed(before, cx);
        result
    }

    fn set_openai_model_inner(&mut self, model: String) -> Result<(), ReasoningError> {
        self.invalidate_validation();
        if model.trim().is_empty() {
            return Err(ReasoningError::Contract(
                "OpenAI model id must not be empty".to_owned(),
            ));
        }
        let mut candidate = self.settings.clone();
        candidate.backends.openai_responses_model_id = model;
        self.commit_settings(candidate)
    }

    pub fn set_codex_model(
        &mut self,
        model: String,
        cx: &mut Context<Self>,
    ) -> Result<(), ReasoningError> {
        let before = self.observable_state();
        let result = self.set_codex_model_inner(model);
        self.notify_if_changed(before, cx);
        result
    }

    fn set_codex_model_inner(&mut self, model: String) -> Result<(), ReasoningError> {
        if model.trim().is_empty() {
            return Err(ReasoningError::Contract(
                "Codex model id must not be empty".to_owned(),
            ));
        }
        let mut candidate = self.settings.clone();
        candidate.backends.codex_model_id = model;
        self.commit_settings(candidate)
    }

    pub fn enable_codex_experimental(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Result<(), ReasoningError> {
        let before = self.observable_state();
        let result = self.enable_codex_experimental_inner();
        self.notify_if_changed(before, cx);
        result
    }

    fn enable_codex_experimental_inner(&mut self) -> Result<(), ReasoningError> {
        if !matches!(self.codex_readiness(), CodexReadiness::Ready { .. }) {
            return Err(ReasoningError::Unavailable(
                "Codex must be installed and authenticated with ChatGPT before experimental use can be enabled."
                    .to_owned(),
            ));
        }
        let mut candidate = self.settings.clone();
        candidate.backends.codex_experimental_consent_version = Some(CODEX_CONSENT_VERSION);
        self.commit_settings(candidate)
    }

    pub fn disable_codex_experimental(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Result<(), ReasoningError> {
        let before = self.observable_state();
        let result = self.disable_codex_experimental_inner();
        self.notify_if_changed(before, cx);
        result
    }

    fn disable_codex_experimental_inner(&mut self) -> Result<(), ReasoningError> {
        let mut candidate = self.settings.clone();
        candidate.backends.codex_experimental_consent_version = None;
        for role in [Role::Watcher, Role::Suggester, Role::Summarizer] {
            if Self::persisted_role(&candidate.roles, role)
                .is_some_and(|id| id == CODEX_CLI_BACKEND_ID)
            {
                Self::set_persisted_role(&mut candidate.roles, role, None);
            }
        }
        self.commit_settings(candidate)
    }

    fn commit_settings(&mut self, candidate: PersistedSettings) -> Result<(), ReasoningError> {
        for selected in [
            candidate.roles.watcher.as_deref(),
            candidate.roles.suggester.as_deref(),
            candidate.roles.summarizer.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if selected == CODEX_CLI_BACKEND_ID {
                if candidate.backends.codex_experimental_consent_version
                    != Some(CODEX_CONSENT_VERSION)
                {
                    return Err(ReasoningError::Unavailable(
                        "Enable experimental Codex subscription use first.".to_owned(),
                    ));
                }
                if !matches!(self.codex_readiness(), CodexReadiness::Ready { .. }) {
                    return Err(ReasoningError::Unavailable(self.codex_readiness().label()));
                }
                continue;
            }
            if selected != OPENAI_RESPONSES_BACKEND_ID {
                return Err(ReasoningError::Contract(format!(
                    "backend {selected} is not a v1 product choice"
                )));
            }
        }
        for role in [Role::Watcher, Role::Suggester] {
            if Self::persisted_role(&candidate.roles, role)
                .is_some_and(|selected| selected == CODEX_CLI_BACKEND_ID)
            {
                return Err(ReasoningError::Contract(
                    "Codex experimental is notes-only and cannot be selected for realtime roles."
                        .to_owned(),
                ));
            }
        }
        let openai_is_selected = [
            candidate.roles.watcher.as_deref(),
            candidate.roles.suggester.as_deref(),
            candidate.roles.summarizer.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|selected| selected == OPENAI_RESPONSES_BACKEND_ID);
        let mut credential_message = None;
        let key = match self.credentials.load() {
            Ok(key) => key,
            Err(error) if openai_is_selected => {
                self.openai_readiness = OpenAiReadiness::from_provider_error(error.clone());
                return Err(ReasoningError::Credential(error.to_string()));
            }
            Err(error) => {
                self.openai_readiness = OpenAiReadiness::from_provider_error(error.clone());
                credential_message = Some(error.to_string());
                None
            }
        };
        let (registry, normalized, error) = Self::build_registry(candidate, key, &self.codex_probe);
        if let Some(error) = error {
            return Err(ReasoningError::Contract(error));
        }
        let path = self.settings_path.as_deref().ok_or_else(|| {
            ReasoningError::Persistence(
                "Application Support is unavailable; selection was not changed".to_owned(),
            )
        })?;
        persistence::save(path, &normalized)?;
        self.registry = registry;
        self.settings = normalized;
        self.status_message = credential_message;
        self.runtime_revision = self.runtime_revision.saturating_add(1);
        Ok(())
    }

    pub fn store_openai_key(
        &mut self,
        key: SecretString,
        cx: &mut Context<Self>,
    ) -> Result<(), ReasoningError> {
        let before = self.observable_state();
        let result = self.store_openai_key_inner(key);
        self.notify_if_changed(before, cx);
        result
    }

    fn store_openai_key_inner(&mut self, key: SecretString) -> Result<(), ReasoningError> {
        self.invalidate_validation();
        self.credentials.store(&key).map_err(|error| {
            self.openai_readiness = OpenAiReadiness::from_provider_error(error.clone());
            ReasoningError::Credential(error.to_string())
        })?;
        let (registry, settings, error) =
            Self::build_registry(self.settings.clone(), Some(key), &self.codex_probe);
        if let Some(error) = error {
            return Err(ReasoningError::Contract(error));
        }
        self.registry = registry;
        self.settings = settings;
        self.openai_readiness = OpenAiReadiness::Stored;
        self.status_message = None;
        self.runtime_revision = self.runtime_revision.saturating_add(1);
        Ok(())
    }

    pub fn delete_openai_key(&mut self, cx: &mut Context<Self>) -> Result<(), ReasoningError> {
        let before = self.observable_state();
        let result = self.delete_openai_key_inner();
        self.notify_if_changed(before, cx);
        result
    }

    fn delete_openai_key_inner(&mut self) -> Result<(), ReasoningError> {
        self.invalidate_validation();
        self.credentials.delete().map_err(|error| {
            self.openai_readiness = OpenAiReadiness::from_provider_error(error.clone());
            ReasoningError::Credential(error.to_string())
        })?;
        let (registry, settings, error) =
            Self::build_registry(self.settings.clone(), None, &self.codex_probe);
        if let Some(error) = error {
            return Err(ReasoningError::Contract(error));
        }
        self.registry = registry;
        self.settings = settings;
        self.openai_readiness = OpenAiReadiness::NoKey;
        self.status_message = None;
        self.runtime_revision = self.runtime_revision.saturating_add(1);
        Ok(())
    }

    pub fn begin_codex_probe(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Result<CodexProbeTicket, ReasoningError> {
        let before = self.observable_state();
        let result = self.begin_codex_probe_inner();
        self.notify_if_changed(before, cx);
        result
    }

    fn begin_codex_probe_inner(&mut self) -> Result<CodexProbeTicket, ReasoningError> {
        self.reap_finished_codex_probe_workers();
        if self.codex_probe_attempt.is_some() {
            return Err(ReasoningError::CodexProbeInProgress);
        }
        self.codex_probe_generation = self.codex_probe_generation.saturating_add(1);
        let generation = self.codex_probe_generation;
        let source = self.codex_probe_source.clone();
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let _ = sender.send(CodexProbeResult {
                generation,
                probe: source.probe(worker_cancellation),
            });
        });
        self.codex_probe_attempt = Some(CodexProbeAttempt {
            generation,
            cancellation: cancellation.clone(),
            worker,
        });
        Ok(CodexProbeTicket {
            generation,
            cancellation,
            receiver,
        })
    }

    pub fn finish_codex_probe(&mut self, result: CodexProbeResult, cx: &mut Context<Self>) -> bool {
        let accepted = self.finish_codex_probe_inner(result);
        if accepted {
            cx.notify();
        }
        accepted
    }

    fn finish_codex_probe_inner(&mut self, result: CodexProbeResult) -> bool {
        let Some(attempt) = self.codex_probe_attempt.take() else {
            return false;
        };
        if attempt.generation != result.generation {
            self.codex_probe_attempt = Some(attempt);
            return false;
        }
        self.retired_codex_probe_workers.push(attempt.worker);
        self.codex_probe = result.probe;
        match self.credentials.load() {
            Ok(key) => {
                let (registry, settings, error) =
                    Self::build_registry(self.settings.clone(), key, &self.codex_probe);
                self.registry = registry;
                self.settings = settings;
                self.status_message = error;
                self.runtime_revision = self.runtime_revision.saturating_add(1);
            }
            Err(error) => {
                self.openai_readiness = OpenAiReadiness::from_provider_error(error.clone());
                let (registry, settings, registry_error) =
                    Self::build_registry(self.settings.clone(), None, &self.codex_probe);
                self.registry = registry;
                self.settings = settings;
                self.status_message = Some(match registry_error {
                    Some(registry_error) => format!("{error}; {registry_error}"),
                    None => error.to_string(),
                });
                self.runtime_revision = self.runtime_revision.saturating_add(1);
            }
        }
        self.reap_finished_codex_probe_workers();
        true
    }

    pub fn codex_probe_worker_disconnected(
        &mut self,
        generation: u64,
        cx: &mut Context<Self>,
    ) -> bool {
        self.finish_codex_probe(
            CodexProbeResult {
                generation,
                probe: CodexProbe {
                    version: None,
                    login_status: AuthStatus::Unavailable {
                        reason: "Codex readiness worker stopped unexpectedly".to_owned(),
                    },
                },
            },
            cx,
        )
    }

    fn reap_finished_codex_probe_workers(&mut self) {
        let mut pending = Vec::new();
        for worker in self.retired_codex_probe_workers.drain(..) {
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                pending.push(worker);
            }
        }
        self.retired_codex_probe_workers = pending;
    }

    pub fn begin_openai_validation(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Result<ValidationTicket, ReasoningError> {
        let before = self.observable_state();
        let result = self.begin_openai_validation_inner();
        self.notify_if_changed(before, cx);
        result
    }

    fn begin_openai_validation_inner(&mut self) -> Result<ValidationTicket, ReasoningError> {
        self.reap_finished_validation_workers();
        if self.validation_attempt.is_some() {
            return Err(ReasoningError::ValidationInProgress);
        }
        let id = BackendId::new(OPENAI_RESPONSES_BACKEND_ID)
            .map_err(|error| ReasoningError::Contract(error.to_string()))?;
        let descriptor = self.registry.descriptor(&id).ok_or_else(|| {
            ReasoningError::Contract("OpenAI backend is not registered".to_owned())
        })?;
        if !descriptor.auth_status().is_ready() {
            self.openai_readiness = OpenAiReadiness::NoKey;
            return Err(ReasoningError::Unavailable(
                "Store an OpenAI API key before validation.".to_owned(),
            ));
        }
        let model = descriptor.model_id().to_owned();
        let temporary_role = Role::Summarizer;
        let previous = self.registry.selected_id(temporary_role).cloned();
        self.registry
            .select(temporary_role, Some(&id))
            .map_err(|error| ReasoningError::Contract(error.to_string()))?;
        let resolved = self
            .registry
            .resolve(temporary_role)
            .map_err(|error| ReasoningError::Contract(error.to_string()))?
            .ok_or_else(|| ReasoningError::Contract("OpenAI did not resolve".to_owned()))?;
        self.registry
            .select(temporary_role, previous.as_ref())
            .map_err(|error| ReasoningError::Contract(error.to_string()))?;
        let provider = resolved.provider();
        let prior_readiness = self.openai_readiness.clone();
        self.validation_generation = self.validation_generation.saturating_add(1);
        let generation = self.validation_generation;
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let outcome = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => {
                    runtime.block_on(validate_openai(provider, model, worker_cancellation))
                }
                Err(error) => ValidationOutcome::Completed(OpenAiReadiness::Failed(format!(
                    "Could not start validation: {error}"
                ))),
            };
            let _ = sender.send(ValidationResult {
                generation,
                outcome,
            });
        });
        self.openai_readiness = OpenAiReadiness::Validating;
        self.validation_attempt = Some(ValidationAttempt {
            generation,
            cancellation: cancellation.clone(),
            prior_readiness,
            worker,
        });
        Ok(ValidationTicket {
            generation,
            cancellation,
            receiver,
        })
    }

    pub fn finish_openai_validation(
        &mut self,
        result: ValidationResult,
        cx: &mut Context<Self>,
    ) -> bool {
        let accepted = self.finish_openai_validation_inner(result);
        if accepted {
            cx.notify();
        }
        accepted
    }

    fn finish_openai_validation_inner(&mut self, result: ValidationResult) -> bool {
        let Some(attempt) = self.validation_attempt.take() else {
            return false;
        };
        if attempt.generation != result.generation {
            self.validation_attempt = Some(attempt);
            return false;
        }
        self.retired_validation_workers.push(attempt.worker);
        let readiness = match result.outcome {
            ValidationOutcome::Completed(readiness) => readiness,
            ValidationOutcome::Cancelled => attempt.prior_readiness,
        };
        let auth_status = match &readiness {
            OpenAiReadiness::Ready => Some(AuthStatus::Ready),
            OpenAiReadiness::BadKey => Some(AuthStatus::Failed {
                reason: "OpenAI rejected the stored API key".to_owned(),
            }),
            OpenAiReadiness::NoKey => Some(AuthStatus::NeedsApiKey),
            _ => None,
        };
        if let Some(status) = auth_status
            && let Ok(id) = BackendId::new(OPENAI_RESPONSES_BACKEND_ID)
        {
            let _ = self.registry.refresh_auth_status(&id, status);
        }
        self.openai_readiness = readiness;
        self.reap_finished_validation_workers();
        true
    }

    pub fn validation_worker_disconnected(
        &mut self,
        generation: u64,
        cx: &mut Context<Self>,
    ) -> bool {
        self.finish_openai_validation(
            ValidationResult {
                generation,
                outcome: ValidationOutcome::Completed(OpenAiReadiness::Failed(
                    "Validation worker stopped unexpectedly. Retry the check.".to_owned(),
                )),
            },
            cx,
        )
    }

    fn invalidate_validation(&mut self) -> bool {
        self.validation_generation = self.validation_generation.saturating_add(1);
        let Some(attempt) = self.validation_attempt.take() else {
            self.reap_finished_validation_workers();
            return false;
        };
        attempt.cancellation.cancel();
        self.openai_readiness = attempt.prior_readiness;
        self.retired_validation_workers.push(attempt.worker);
        self.reap_finished_validation_workers();
        true
    }

    fn reap_finished_validation_workers(&mut self) {
        let mut pending = Vec::new();
        for worker in self.retired_validation_workers.drain(..) {
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                pending.push(worker);
            }
        }
        self.retired_validation_workers = pending;
    }

    fn observable_state(
        &self,
    ) -> (
        PersistedSettings,
        OpenAiReadiness,
        CodexReadiness,
        Option<String>,
        bool,
        u64,
    ) {
        (
            self.settings.clone(),
            self.openai_readiness.clone(),
            self.codex_readiness(),
            self.status_message.clone(),
            self.validation_attempt.is_some(),
            self.runtime_revision,
        )
    }

    fn notify_if_changed(
        &self,
        before: (
            PersistedSettings,
            OpenAiReadiness,
            CodexReadiness,
            Option<String>,
            bool,
            u64,
        ),
        cx: &mut Context<Self>,
    ) {
        if self.observable_state() != before {
            cx.notify();
        }
    }
}

impl Drop for ReasoningController {
    fn drop(&mut self) {
        if let Some(attempt) = self.codex_probe_attempt.take() {
            attempt.cancellation.cancel();
            let _ = attempt.worker.join();
        }
        for worker in self.retired_codex_probe_workers.drain(..) {
            let _ = worker.join();
        }
        if let Some(attempt) = self.validation_attempt.take() {
            attempt.cancellation.cancel();
            let _ = attempt.worker.join();
        }
        for worker in self.retired_validation_workers.drain(..) {
            let _ = worker.join();
        }
    }
}

pub async fn validate_openai(
    provider: Arc<dyn CompletionProvider>,
    model: String,
    cancellation: CancellationToken,
) -> ValidationOutcome {
    let request = CompletionRequest {
        model,
        system: None,
        messages: vec![CompletionMessage {
            role: MessageRole::User,
            content: "Return exactly this JSON object: {\"ok\":true}".to_owned(),
            cache_boundary: false,
        }],
        max_tokens: Some(8),
        temperature: None,
        stop: Vec::new(),
    };
    let result = tokio::select! {
        _ = cancellation.cancelled() => Err(ProviderError::Cancelled),
        result = provider.stream(request, cancellation.clone()) => result,
    };
    let result = match result {
        Ok(mut stream) => tokio::select! {
            _ = cancellation.cancelled() => Err(ProviderError::Cancelled),
            result = stream.next() => match result {
                Some(result) => result.map(|_| ()),
                None => Err(ProviderError::Decode("OpenAI returned no response".to_owned())),
            },
        },
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => ValidationOutcome::Completed(OpenAiReadiness::Ready),
        Err(ProviderError::Cancelled) => ValidationOutcome::Cancelled,
        Err(error) => ValidationOutcome::Completed(OpenAiReadiness::from_provider_error(error)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use futures_util::stream;
    use gpui::{AppContext, Subscription, TestAppContext};
    use secrecy::{ExposeSecret, SecretString};

    use super::persistence::SETTINGS_FILE_NAME;
    use super::*;

    #[derive(Default)]
    struct FakeCredentials {
        key: Mutex<Option<SecretString>>,
        failure: Mutex<Option<ProviderError>>,
    }

    impl OpenAiCredentialStore for FakeCredentials {
        fn store(&self, key: &SecretString) -> Result<(), ProviderError> {
            let failure = self.failure.lock().map_err(lock_error)?.clone();
            if let Some(error) = failure {
                return Err(error);
            }
            *self.key.lock().map_err(lock_error)? = Some(key.clone());
            Ok(())
        }

        fn load(&self) -> Result<Option<SecretString>, ProviderError> {
            let failure = self.failure.lock().map_err(lock_error)?.clone();
            if let Some(error) = failure {
                return Err(error);
            }
            Ok(self.key.lock().map_err(lock_error)?.clone())
        }

        fn delete(&self) -> Result<(), ProviderError> {
            let failure = self.failure.lock().map_err(lock_error)?.clone();
            if let Some(error) = failure {
                return Err(error);
            }
            *self.key.lock().map_err(lock_error)? = None;
            Ok(())
        }
    }

    struct FakeCodexProbeSource {
        probe: Mutex<CodexProbe>,
        calls: AtomicUsize,
    }

    impl FakeCodexProbeSource {
        fn new(probe: CodexProbe) -> Self {
            Self {
                probe: Mutex::new(probe),
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl CodexProbeSource for FakeCodexProbeSource {
        fn probe(&self, _cancellation: CancellationToken) -> CodexProbe {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.probe.lock().map_or_else(
                |_| unavailable_probe("fake probe lock poisoned"),
                |probe| probe.clone(),
            )
        }
    }

    fn unavailable_probe(reason: &str) -> CodexProbe {
        CodexProbe {
            version: None,
            login_status: AuthStatus::Unavailable {
                reason: reason.to_owned(),
            },
        }
    }

    fn ready_probe() -> CodexProbe {
        CodexProbe {
            version: Some("codex-cli test".to_owned()),
            login_status: AuthStatus::Ready,
        }
    }

    fn lock_error<T>(_: std::sync::PoisonError<T>) -> ProviderError {
        ProviderError::CredentialStore("fake credential lock poisoned".to_owned())
    }

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn fixture() -> Result<
        (tempfile::TempDir, Arc<FakeCredentials>, ReasoningController),
        Box<dyn std::error::Error>,
    > {
        let directory = tempfile::tempdir()?;
        let store = Arc::new(FakeCredentials::default());
        let controller =
            ReasoningController::load(directory.path().join(SETTINGS_FILE_NAME), store.clone());
        Ok((directory, store, controller))
    }

    fn install_test_attempt(controller: &mut ReasoningController) -> (u64, CancellationToken) {
        controller.validation_generation = controller.validation_generation.saturating_add(1);
        let generation = controller.validation_generation;
        let cancellation = CancellationToken::new();
        controller.validation_attempt = Some(ValidationAttempt {
            generation,
            cancellation: cancellation.clone(),
            prior_readiness: controller.openai_readiness.clone(),
            worker: std::thread::spawn(|| {}),
        });
        controller.openai_readiness = OpenAiReadiness::Validating;
        (generation, cancellation)
    }

    #[test]
    fn cold_start_is_complete_no_reasoning_without_credentials() -> TestResult {
        let (_directory, _store, controller) = fixture()?;
        for role in [Role::Watcher, Role::Suggester, Role::Summarizer] {
            assert!(controller.selected_id(role).is_none());
            assert!(controller.resolve(role).is_ok_and(|value| value.is_none()));
        }
        assert_eq!(controller.openai_readiness(), &OpenAiReadiness::NoKey);
        Ok(())
    }

    #[test]
    fn persistence_contains_identifiers_and_never_secret_or_prompt_material() -> TestResult {
        let (directory, store, mut controller) = fixture()?;
        let secret = "sk-test-never-persist-this";
        controller.store_openai_key_inner(SecretString::from(secret.to_owned()))?;
        controller.apply_to_all_inner(Some(OPENAI_RESPONSES_BACKEND_ID))?;
        let text = std::fs::read_to_string(directory.path().join(SETTINGS_FILE_NAME))?;
        assert!(text.contains(OPENAI_RESPONSES_BACKEND_ID));
        assert!(text.contains(DEFAULT_OPENAI_MODEL));
        for forbidden in [secret, "prompt", "api_key", "token", "credential_path"] {
            assert!(
                !text.contains(forbidden),
                "persisted forbidden field {forbidden}"
            );
        }
        let stored = store.key.lock().map_err(lock_error)?;
        assert_eq!(
            stored.as_ref().map(ExposeSecret::expose_secret),
            Some(secret)
        );
        drop(stored);
        let reloaded =
            ReasoningController::load(directory.path().join(SETTINGS_FILE_NAME), store.clone());
        for role in [Role::Watcher, Role::Suggester, Role::Summarizer] {
            assert_eq!(
                reloaded.selected_id(role).map(BackendId::as_str),
                Some(OPENAI_RESPONSES_BACKEND_ID)
            );
        }
        let temporary_files = std::fs::read_dir(directory.path())?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(temporary_files, 0, "atomic save must not leave temp files");
        Ok(())
    }

    #[test]
    fn injected_codex_probe_recognizes_chatgpt_login_without_live_cli() -> TestResult {
        let directory = tempfile::tempdir()?;
        let store = Arc::new(FakeCredentials::default());
        let probe_source = Arc::new(FakeCodexProbeSource::new(ready_probe()));
        let mut controller = ReasoningController::load_with_probe_source(
            directory.path().join(SETTINGS_FILE_NAME),
            store,
            probe_source.clone(),
        );
        assert_eq!(controller.codex_readiness(), CodexReadiness::NotChecked);
        let ticket = controller.begin_codex_probe_inner()?;
        assert_eq!(controller.codex_readiness(), CodexReadiness::Checking);
        let result = ticket
            .receiver
            .recv_timeout(std::time::Duration::from_secs(1))?;
        assert!(controller.finish_codex_probe_inner(result));
        assert_eq!(probe_source.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            controller.codex_readiness(),
            CodexReadiness::Ready {
                version: Some("codex-cli test".to_owned())
            }
        );
        Ok(())
    }

    #[test]
    fn codex_requires_persisted_consent_then_resolves_without_api_key() -> TestResult {
        let directory = tempfile::tempdir()?;
        let settings_path = directory.path().join(SETTINGS_FILE_NAME);
        let store = Arc::new(FakeCredentials::default());
        let source = Arc::new(FakeCodexProbeSource::new(ready_probe()));
        let mut controller = ReasoningController::load_with_probe_source(
            settings_path.clone(),
            store.clone(),
            source,
        );
        let ticket = controller.begin_codex_probe_inner()?;
        let result = ticket
            .receiver
            .recv_timeout(std::time::Duration::from_secs(1))?;
        assert!(controller.finish_codex_probe_inner(result));
        let id = BackendId::new(CODEX_CLI_BACKEND_ID)?;
        assert!(matches!(
            controller.descriptor(&id).map(BackendDescriptor::auth_status),
            Some(AuthStatus::Unavailable { reason }) if reason.contains("isolation")
        ));
        assert!(matches!(
            controller.apply_to_all_inner(Some(CODEX_CLI_BACKEND_ID)),
            Err(ReasoningError::Contract(_))
        ));

        controller.enable_codex_experimental_inner()?;
        // Consent selects the connector; it does not manufacture a capability. Strict
        // output-schema cannot express "any JSON object", so Codex never advertises
        // `JsonObjectOutput` — the request normalization seam downgrades and records it instead.
        assert!(controller.descriptor(&id).is_some_and(|descriptor| {
            !descriptor
                .capabilities()
                .contains(providers::BackendCapability::JsonObjectOutput)
        }));
        controller.set_codex_model_inner("meeting-model".to_owned())?;
        assert!(
            controller
                .apply_to_all_inner(Some(CODEX_CLI_BACKEND_ID))
                .is_err()
        );
        assert!(
            controller
                .select_role_inner(Role::Watcher, Some(CODEX_CLI_BACKEND_ID))
                .is_err()
        );
        controller.select_role_inner(Role::Summarizer, Some(CODEX_CLI_BACKEND_ID))?;
        assert!(controller.selected_id(Role::Watcher).is_none());
        assert!(controller.selected_id(Role::Suggester).is_none());
        assert_eq!(
            controller
                .selected_id(Role::Summarizer)
                .map(BackendId::as_str),
            Some(CODEX_CLI_BACKEND_ID)
        );
        assert!(store.key.lock().map_err(lock_error)?.is_none());
        let persisted = std::fs::read_to_string(&settings_path)?;
        assert!(persisted.contains("codex_experimental_consent_version"));
        assert!(persisted.contains("meeting-model"));
        for forbidden in ["api_key", "auth.json", "credential_path"] {
            assert!(!persisted.contains(forbidden));
        }

        let reload_source = Arc::new(FakeCodexProbeSource::new(ready_probe()));
        let mut reloaded = ReasoningController::load_with_probe_source(
            settings_path,
            store,
            reload_source.clone(),
        );
        assert!(reloaded.codex_experimental_enabled());
        assert_eq!(
            reloaded
                .selected_id(Role::Summarizer)
                .map(BackendId::as_str),
            Some(CODEX_CLI_BACKEND_ID)
        );
        assert!(reloaded.resolve(Role::Summarizer).is_err());
        let ticket = reloaded.begin_codex_probe_inner()?;
        let result = ticket
            .receiver
            .recv_timeout(std::time::Duration::from_secs(1))?;
        assert!(reloaded.finish_codex_probe_inner(result));
        assert_eq!(
            reloaded
                .selected_id(Role::Summarizer)
                .map(BackendId::as_str),
            Some(CODEX_CLI_BACKEND_ID)
        );
        *reload_source.probe.lock().map_err(lock_error)? = CodexProbe {
            version: Some("codex-cli test".to_owned()),
            login_status: AuthStatus::NeedsLogin,
        };
        let ticket = reloaded.begin_codex_probe_inner()?;
        let result = ticket
            .receiver
            .recv_timeout(std::time::Duration::from_secs(1))?;
        assert!(reloaded.finish_codex_probe_inner(result));
        assert_eq!(reloaded.codex_readiness(), CodexReadiness::NeedsLogin);
        assert!(reloaded.codex_experimental_enabled());
        assert_eq!(
            reloaded
                .selected_id(Role::Summarizer)
                .map(BackendId::as_str),
            Some(CODEX_CLI_BACKEND_ID)
        );
        assert!(reloaded.resolve(Role::Summarizer).is_err());
        reloaded.disable_codex_experimental_inner()?;
        assert!(!reloaded.codex_experimental_enabled());
        for role in [Role::Watcher, Role::Suggester, Role::Summarizer] {
            assert!(reloaded.selected_id(role).is_none());
        }
        Ok(())
    }

    #[test]
    fn signed_out_codex_cannot_be_enabled() -> TestResult {
        let directory = tempfile::tempdir()?;
        let store = Arc::new(FakeCredentials::default());
        let source = Arc::new(FakeCodexProbeSource::new(CodexProbe {
            version: Some("codex-cli test".to_owned()),
            login_status: AuthStatus::NeedsLogin,
        }));
        let mut controller = ReasoningController::load_with_probe_source(
            directory.path().join(SETTINGS_FILE_NAME),
            store,
            source,
        );
        let ticket = controller.begin_codex_probe_inner()?;
        let result = ticket
            .receiver
            .recv_timeout(std::time::Duration::from_secs(1))?;
        assert!(controller.finish_codex_probe_inner(result));
        assert_eq!(controller.codex_readiness(), CodexReadiness::NeedsLogin);
        assert!(matches!(
            controller.enable_codex_experimental_inner(),
            Err(ReasoningError::Unavailable(_))
        ));
        Ok(())
    }

    #[test]
    fn codex_consent_and_selection_do_not_depend_on_openai_keychain() -> TestResult {
        let directory = tempfile::tempdir()?;
        let store = Arc::new(FakeCredentials::default());
        let source = Arc::new(FakeCodexProbeSource::new(ready_probe()));
        let mut controller = ReasoningController::load_with_probe_source(
            directory.path().join(SETTINGS_FILE_NAME),
            store.clone(),
            source,
        );
        let ticket = controller.begin_codex_probe_inner()?;
        let result = ticket
            .receiver
            .recv_timeout(std::time::Duration::from_secs(1))?;
        assert!(controller.finish_codex_probe_inner(result));
        *store.failure.lock().map_err(lock_error)? = Some(ProviderError::CredentialStore(
            "OpenAI Keychain denied".to_owned(),
        ));

        controller.enable_codex_experimental_inner()?;
        controller.select_role_inner(Role::Summarizer, Some(CODEX_CLI_BACKEND_ID))?;
        assert_eq!(
            controller
                .selected_id(Role::Summarizer)
                .map(BackendId::as_str),
            Some(CODEX_CLI_BACKEND_ID)
        );
        assert_eq!(
            controller.openai_readiness(),
            &OpenAiReadiness::CredentialStore("OpenAI Keychain denied".to_owned())
        );
        Ok(())
    }

    #[test]
    fn reload_probe_restores_codex_when_openai_keychain_is_unavailable() -> TestResult {
        let directory = tempfile::tempdir()?;
        let settings_path = directory.path().join(SETTINGS_FILE_NAME);
        let store = Arc::new(FakeCredentials::default());
        let source = Arc::new(FakeCodexProbeSource::new(ready_probe()));
        let mut controller = ReasoningController::load_with_probe_source(
            settings_path.clone(),
            store.clone(),
            source,
        );
        let ticket = controller.begin_codex_probe_inner()?;
        let result = ticket
            .receiver
            .recv_timeout(std::time::Duration::from_secs(1))?;
        assert!(controller.finish_codex_probe_inner(result));
        controller.enable_codex_experimental_inner()?;
        controller.select_role_inner(Role::Summarizer, Some(CODEX_CLI_BACKEND_ID))?;
        drop(controller);

        *store.failure.lock().map_err(lock_error)? = Some(ProviderError::CredentialStore(
            "OpenAI Keychain denied".to_owned(),
        ));
        let source = Arc::new(FakeCodexProbeSource::new(ready_probe()));
        let mut reloaded =
            ReasoningController::load_with_probe_source(settings_path, store, source);
        assert!(reloaded.resolve(Role::Summarizer).is_err());
        let ticket = reloaded.begin_codex_probe_inner()?;
        let result = ticket
            .receiver
            .recv_timeout(std::time::Duration::from_secs(1))?;
        assert!(reloaded.finish_codex_probe_inner(result));
        assert!(reloaded.resolve(Role::Summarizer)?.is_some());
        assert_eq!(
            reloaded.openai_readiness(),
            &OpenAiReadiness::CredentialStore("OpenAI Keychain denied".to_owned())
        );
        Ok(())
    }

    #[test]
    fn stale_codex_probe_result_cannot_replace_current_attempt() -> TestResult {
        let directory = tempfile::tempdir()?;
        let mut controller = ReasoningController::load_with_probe_source(
            directory.path().join(SETTINGS_FILE_NAME),
            Arc::new(FakeCredentials::default()),
            Arc::new(FakeCodexProbeSource::new(ready_probe())),
        );
        let ticket = controller.begin_codex_probe_inner()?;
        assert!(!controller.finish_codex_probe_inner(CodexProbeResult {
            generation: ticket.generation().saturating_add(1),
            probe: ready_probe(),
        }));
        assert_eq!(controller.codex_readiness(), CodexReadiness::Checking);
        let result = ticket
            .receiver
            .recv_timeout(std::time::Duration::from_secs(1))?;
        assert!(controller.finish_codex_probe_inner(result));
        Ok(())
    }

    #[test]
    fn openai_credential_flow_uses_injected_store_and_keeps_errors_distinct() -> TestResult {
        let (_directory, store, mut controller) = fixture()?;
        controller.store_openai_key_inner(SecretString::from("fake-key".to_owned()))?;
        assert_eq!(controller.openai_readiness(), &OpenAiReadiness::Stored);
        controller.delete_openai_key_inner()?;
        assert_eq!(controller.openai_readiness(), &OpenAiReadiness::NoKey);
        *store.failure.lock().map_err(lock_error)? =
            Some(ProviderError::CredentialStore("denied".to_owned()));
        assert!(
            controller
                .store_openai_key_inner(SecretString::from("unused".to_owned()))
                .is_err()
        );
        assert_eq!(
            controller.openai_readiness(),
            &OpenAiReadiness::CredentialStore("denied".to_owned())
        );
        Ok(())
    }

    #[test]
    fn model_switch_affects_new_resolves_but_not_pinned_backend() -> TestResult {
        let (_directory, _store, mut controller) = fixture()?;
        controller.store_openai_key_inner(SecretString::from("fake-key".to_owned()))?;
        controller.apply_to_all_inner(Some(OPENAI_RESPONSES_BACKEND_ID))?;
        let pinned = controller
            .resolve(Role::Watcher)?
            .ok_or("backend should resolve")?;
        controller.set_openai_model_inner("gpt-test-next".to_owned())?;
        let next = controller
            .resolve(Role::Watcher)?
            .ok_or("backend should resolve")?;
        assert_eq!(pinned.descriptor().model_id(), DEFAULT_OPENAI_MODEL);
        assert_eq!(next.descriptor().model_id(), "gpt-test-next");
        assert_ne!(pinned.cache_fingerprint(), next.cache_fingerprint());
        controller.select_role_inner(Role::Watcher, None)?;
        assert!(
            controller
                .resolve(Role::Watcher)
                .is_ok_and(|value| value.is_none())
        );
        assert_eq!(pinned.descriptor().model_id(), DEFAULT_OPENAI_MODEL);
        Ok(())
    }

    #[test]
    fn product_registry_has_no_legacy_adapter_choices() -> TestResult {
        let (_directory, _store, controller) = fixture()?;
        for legacy in ["anthropic", "google", "openrouter", "ollama"] {
            let id = BackendId::new(legacy)?;
            assert!(controller.descriptor(&id).is_none());
        }
        Ok(())
    }

    #[test]
    fn validation_failures_remain_actionably_distinct() {
        assert_eq!(
            OpenAiReadiness::from_provider_error(ProviderError::Auth),
            OpenAiReadiness::BadKey
        );
        assert_eq!(
            OpenAiReadiness::from_provider_error(ProviderError::Network("offline".to_owned())),
            OpenAiReadiness::NetworkDown
        );
        assert_eq!(
            OpenAiReadiness::from_provider_error(ProviderError::RateLimit { retry_after: None }),
            OpenAiReadiness::RateLimited
        );
    }

    #[test]
    fn stale_validation_result_cannot_overwrite_deleted_or_replaced_key() -> TestResult {
        let (_directory, _store, mut controller) = fixture()?;
        controller.store_openai_key_inner(SecretString::from("first-key".to_owned()))?;
        let (deleted_generation, deleted_token) = install_test_attempt(&mut controller);
        controller.delete_openai_key_inner()?;
        assert!(
            deleted_token.is_cancelled(),
            "deleting must cancel validation"
        );
        assert_eq!(controller.openai_readiness(), &OpenAiReadiness::NoKey);
        assert!(
            !controller.finish_openai_validation_inner(ValidationResult {
                generation: deleted_generation,
                outcome: ValidationOutcome::Completed(OpenAiReadiness::Ready),
            })
        );
        assert_eq!(controller.openai_readiness(), &OpenAiReadiness::NoKey);

        controller.store_openai_key_inner(SecretString::from("second-key".to_owned()))?;
        let (replaced_generation, replaced_token) = install_test_attempt(&mut controller);
        controller.store_openai_key_inner(SecretString::from("third-key".to_owned()))?;
        assert!(
            replaced_token.is_cancelled(),
            "replacement must cancel validation"
        );
        assert_eq!(controller.openai_readiness(), &OpenAiReadiness::Stored);
        assert!(
            !controller.finish_openai_validation_inner(ValidationResult {
                generation: replaced_generation,
                outcome: ValidationOutcome::Completed(OpenAiReadiness::BadKey),
            })
        );
        assert_eq!(controller.openai_readiness(), &OpenAiReadiness::Stored);
        Ok(())
    }

    #[test]
    fn selection_mutation_cancels_attempt_and_repeated_begin_is_rejected() -> TestResult {
        let (_directory, _store, mut controller) = fixture()?;
        let (_generation, cancellation) = install_test_attempt(&mut controller);
        assert!(matches!(
            controller.begin_openai_validation_inner(),
            Err(ReasoningError::ValidationInProgress)
        ));
        controller.apply_to_all_inner(None)?;
        assert!(cancellation.is_cancelled());
        assert!(controller.validation_attempt.is_none());
        Ok(())
    }

    #[test]
    fn validation_ticket_drop_cancels_owned_attempt_token() {
        let cancellation = CancellationToken::new();
        let (_sender, receiver) = mpsc::sync_channel(1);
        let ticket = ValidationTicket {
            generation: 9,
            cancellation: cancellation.clone(),
            receiver,
        };
        drop(ticket);
        assert!(cancellation.is_cancelled());
    }

    struct PendingProvider;

    impl CompletionProvider for PendingProvider {
        fn stream(
            &self,
            _request: CompletionRequest,
            _cancellation: CancellationToken,
        ) -> sotto_core::BoxFuture<
            '_,
            Result<
                sotto_core::BoxStream<'static, Result<sotto_core::Delta, ProviderError>>,
                ProviderError,
            >,
        > {
            Box::pin(async {
                Ok(Box::pin(stream::pending())
                    as sotto_core::BoxStream<
                        'static,
                        Result<sotto_core::Delta, ProviderError>,
                    >)
            })
        }

        fn model_id(&self) -> &str {
            "pending-model"
        }
    }

    #[test]
    fn validation_wait_is_cancelled_without_a_fixed_timeout() -> TestResult {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let outcome = runtime.block_on(async move {
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                cancel.cancel();
            });
            validate_openai(
                Arc::new(PendingProvider),
                "pending-model".to_owned(),
                cancellation,
            )
            .await
        });
        assert_eq!(outcome, ValidationOutcome::Cancelled);
        Ok(())
    }

    #[test]
    fn invalidated_worker_is_tracked_until_reaped() -> TestResult {
        let (_directory, _store, mut controller) = fixture()?;
        let cancellation = CancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let (done_sender, done_receiver) = mpsc::sync_channel(1);
        controller.validation_generation = 1;
        controller.validation_attempt = Some(ValidationAttempt {
            generation: 1,
            cancellation,
            prior_readiness: controller.openai_readiness.clone(),
            worker: std::thread::spawn(move || {
                while !worker_cancellation.is_cancelled() {
                    std::thread::yield_now();
                }
                let _ = done_sender.send(());
            }),
        });
        controller.openai_readiness = OpenAiReadiness::Validating;
        assert!(controller.invalidate_validation());
        done_receiver.recv_timeout(std::time::Duration::from_secs(1))?;
        for _ in 0..1_000 {
            controller.reap_finished_validation_workers();
            if controller.retired_validation_workers.is_empty() {
                break;
            }
            std::thread::yield_now();
        }
        assert!(
            controller.retired_validation_workers.is_empty(),
            "a cancelled validation worker should be reaped after it exits"
        );
        Ok(())
    }

    struct NotificationObserver {
        _subscription: Subscription,
    }

    #[test]
    fn controller_mutations_notify_observers_but_stale_results_do_not() -> TestResult {
        let mut cx = TestAppContext::single();
        let directory = tempfile::tempdir()?;
        let store = Arc::new(FakeCredentials::default());
        let controller =
            cx.new(|_| ReasoningController::load(directory.path().join(SETTINGS_FILE_NAME), store));
        let notifications = Arc::new(AtomicUsize::new(0));
        let observed = notifications.clone();
        let _observer = cx.new(|cx| NotificationObserver {
            _subscription: cx.observe(&controller, move |_, _, _| {
                observed.fetch_add(1, Ordering::SeqCst);
            }),
        });

        cx.update(|cx| {
            controller.update(cx, |controller, cx| {
                controller.set_openai_model("observer-model".to_owned(), cx)
            })
        })?;
        cx.run_until_parked();
        assert_eq!(notifications.load(Ordering::SeqCst), 1);

        let accepted = cx.update(|cx| {
            controller.update(cx, |controller, cx| {
                controller.finish_openai_validation(
                    ValidationResult {
                        generation: 99,
                        outcome: ValidationOutcome::Completed(OpenAiReadiness::Ready),
                    },
                    cx,
                )
            })
        });
        cx.run_until_parked();
        assert!(!accepted);
        assert_eq!(notifications.load(Ordering::SeqCst), 1);
        Ok(())
    }
}
