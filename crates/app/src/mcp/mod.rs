//! HTTP-only MCP source configuration and explicit per-meeting grants.

mod persistence;

use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread::JoinHandle,
};

use gpui::Context;
use mcp::{
    BearerCredential, ContextBudget, ContextCancellation, GrantRunFingerprint, HttpEndpoint,
    McpBroker, MeetingQueryDisclosure, ResourceCatalog, ResourceDescriptor, ResourceUri,
    RmcpResourceTransport, ServerConnection, ServerDescriptor, ServerId, SessionContextGrant,
    TransportKind,
};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use sotto_core::SessionId;

use persistence::{PersistedGrant, PersistedMcpSettings, PersistedResource, PersistedServer};

const SETTINGS_VERSION: u32 = 1;
const KEYCHAIN_SERVICE: &str = "dev.sotto.mcp";

#[derive(Clone, Eq, PartialEq)]
pub enum CredentialReadiness {
    None,
    Stored,
    Error(String),
}

impl fmt::Debug for CredentialReadiness {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::None => "CredentialReadiness::None",
            Self::Stored => "CredentialReadiness::Stored",
            Self::Error(_) => "CredentialReadiness::Error(<redacted>)",
        })
    }
}

impl CredentialReadiness {
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::None => "No bearer token stored.",
            Self::Stored => "Bearer token stored in Keychain.",
            Self::Error(_) => "Keychain readiness unavailable.",
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub enum ConnectionHealth {
    NotChecked,
    Checking,
    Available { resources: usize },
    Unavailable(String),
}

impl fmt::Debug for ConnectionHealth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotChecked => formatter.write_str("ConnectionHealth::NotChecked"),
            Self::Checking => formatter.write_str("ConnectionHealth::Checking"),
            Self::Available { resources } => formatter
                .debug_struct("ConnectionHealth::Available")
                .field("resources", resources)
                .finish(),
            Self::Unavailable(_) => {
                formatter.write_str("ConnectionHealth::Unavailable(<redacted>)")
            }
        }
    }
}

impl ConnectionHealth {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::NotChecked => "Not checked; no network request has been made.".to_owned(),
            Self::Checking => "Checking remote HTTPS source…".to_owned(),
            Self::Available { resources } => {
                format!("Available; {resources} read-only resources discovered.")
            }
            Self::Unavailable(_) => "Remote source unavailable. Retry explicitly.".to_owned(),
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ConfiguredServer {
    pub id: ServerId,
    pub display_name: String,
    pub endpoint: HttpEndpoint,
    pub credential: CredentialReadiness,
    pub health: ConnectionHealth,
    pub resources: Vec<ResourceDescriptor>,
}

impl fmt::Debug for ConfiguredServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfiguredServer")
            .field("id", &self.id)
            .field("display_name", &"<redacted>")
            .field("endpoint", &self.endpoint)
            .field("credential", &self.credential.label())
            .field("health", &self.health.label())
            .field("resource_count", &self.resources.len())
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum GrantReceiptState {
    NotRetrieved,
}

#[derive(Clone, Eq, PartialEq)]
pub struct SessionGrantView {
    pub session_id: SessionId,
    pub grant: SessionContextGrant,
    pub fingerprint: Option<GrantRunFingerprint>,
    pub receipts: GrantReceiptState,
}

#[derive(Clone)]
pub struct FrozenGrounding {
    pub session_id: SessionId,
    pub grant: SessionContextGrant,
    pub fingerprint: Option<GrantRunFingerprint>,
    pub source: Arc<dyn mcp::ContextSource>,
}

impl fmt::Debug for SessionGrantView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionGrantView")
            .field("session_id", &self.session_id)
            .field("grant", &self.grant)
            .field("fingerprint", &self.fingerprint)
            .field("receipts", &self.receipts)
            .finish()
    }
}

pub enum McpUiError {
    Persistence(String),
    Invalid(String),
    Credential(String),
    Busy,
}

impl fmt::Debug for McpUiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Persistence(_) => "McpUiError::Persistence(<redacted>)",
            Self::Invalid(_) => "McpUiError::Invalid(<redacted>)",
            Self::Credential(_) => "McpUiError::Credential(<redacted>)",
            Self::Busy => "McpUiError::Busy",
        })
    }
}

impl fmt::Display for McpUiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Persistence(_) => formatter.write_str(
                "MCP settings could not be read or saved. Check local file permissions.",
            ),
            Self::Invalid(_) => formatter
                .write_str("The MCP source configuration is invalid or no longer available."),
            Self::Credential(_) => {
                formatter.write_str("The MCP credential could not be read or saved in Keychain.")
            }
            Self::Busy => formatter.write_str("An MCP connection check is already running."),
        }
    }
}

impl std::error::Error for McpUiError {}

pub trait McpCredentialStore: Send + Sync {
    fn store(
        &self,
        server_id: &ServerId,
        endpoint: &HttpEndpoint,
        secret: &SecretString,
    ) -> Result<(), McpUiError>;
    fn load(
        &self,
        server_id: &ServerId,
        endpoint: &HttpEndpoint,
    ) -> Result<Option<SecretString>, McpUiError>;
    fn delete(&self, server_id: &ServerId, endpoint: &HttpEndpoint) -> Result<(), McpUiError>;
}

pub struct KeychainMcpCredentialStore;

impl McpCredentialStore for KeychainMcpCredentialStore {
    fn store(
        &self,
        server_id: &ServerId,
        endpoint: &HttpEndpoint,
        secret: &SecretString,
    ) -> Result<(), McpUiError> {
        keyring_entry(server_id, endpoint)?
            .set_password(secret.expose_secret())
            .map_err(|error| McpUiError::Credential(error.to_string()))
    }

    fn load(
        &self,
        server_id: &ServerId,
        endpoint: &HttpEndpoint,
    ) -> Result<Option<SecretString>, McpUiError> {
        match keyring_entry(server_id, endpoint)?.get_password() {
            Ok(secret) => Ok(Some(SecretString::from(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(McpUiError::Credential(error.to_string())),
        }
    }

    fn delete(&self, server_id: &ServerId, endpoint: &HttpEndpoint) -> Result<(), McpUiError> {
        match keyring_entry(server_id, endpoint)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(McpUiError::Credential(error.to_string())),
        }
    }
}

trait McpSettingsStore: Send + Sync {
    fn load(&self, path: &Path) -> Result<Option<PersistedMcpSettings>, McpUiError>;
    fn save(&self, path: &Path, settings: &PersistedMcpSettings) -> Result<(), McpUiError>;
}

struct FileMcpSettingsStore;

impl McpSettingsStore for FileMcpSettingsStore {
    fn load(&self, path: &Path) -> Result<Option<PersistedMcpSettings>, McpUiError> {
        persistence::load(path)
    }

    fn save(&self, path: &Path, settings: &PersistedMcpSettings) -> Result<(), McpUiError> {
        persistence::save(path, settings)
    }
}

fn keyring_entry(
    server_id: &ServerId,
    endpoint: &HttpEndpoint,
) -> Result<keyring::Entry, McpUiError> {
    let mut hasher = Sha256::new();
    hasher.update(b"sotto-mcp-key-v1\0");
    hasher.update(server_id.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(endpoint.as_str().as_bytes());
    let account = format!("{}@{}", server_id.as_str(), hex::encode(hasher.finalize()));
    keyring::Entry::new(KEYCHAIN_SERVICE, &account)
        .map_err(|error| McpUiError::Credential(error.to_string()))
}

enum WorkerKind {
    Discover,
}

enum WorkerPayload {
    Catalog(ResourceCatalog),
}

struct WorkerResult {
    generation: u64,
    server_id: Option<ServerId>,
    result: Result<WorkerPayload, String>,
}

struct PendingWorker {
    generation: u64,
    kind: WorkerKind,
    cancellation: ContextCancellation,
    receiver: mpsc::Receiver<WorkerResult>,
    handle: JoinHandle<()>,
}

pub struct McpController {
    settings_path: Option<PathBuf>,
    settings: Arc<dyn McpSettingsStore>,
    credentials: Arc<dyn McpCredentialStore>,
    servers: BTreeMap<ServerId, ConfiguredServer>,
    grants: BTreeMap<SessionId, SessionGrantView>,
    selected_session: Option<SessionId>,
    status_message: Option<String>,
    generation: u64,
    pending: Option<PendingWorker>,
    retired: Vec<JoinHandle<()>>,
}

impl McpController {
    #[must_use]
    pub fn load_default() -> Self {
        let path = application_settings_path().ok();
        Self::load(path, Arc::new(KeychainMcpCredentialStore))
    }

    #[must_use]
    pub fn load(path: Option<PathBuf>, credentials: Arc<dyn McpCredentialStore>) -> Self {
        Self::load_with_settings(path, credentials, Arc::new(FileMcpSettingsStore))
    }

    fn load_with_settings(
        path: Option<PathBuf>,
        credentials: Arc<dyn McpCredentialStore>,
        settings: Arc<dyn McpSettingsStore>,
    ) -> Self {
        let mut controller = Self {
            settings_path: path,
            settings,
            credentials,
            servers: BTreeMap::new(),
            grants: BTreeMap::new(),
            selected_session: None,
            status_message: None,
            generation: 0,
            pending: None,
            retired: Vec::new(),
        };
        controller.load_persisted();
        controller
    }

    #[must_use]
    pub fn servers(&self) -> Vec<ConfiguredServer> {
        self.servers.values().cloned().collect()
    }

    #[must_use]
    pub fn selected_grant(&self) -> Option<SessionGrantView> {
        self.selected_session
            .and_then(|session_id| self.grants.get(&session_id).cloned())
    }

    pub fn freeze_selected_grounding(&self) -> Result<FrozenGrounding, McpUiError> {
        let view = self.selected_grant().unwrap_or_else(|| SessionGrantView {
            session_id: SessionId::new(0),
            grant: SessionContextGrant::new(),
            fingerprint: None,
            receipts: GrantReceiptState::NotRetrieved,
        });
        let mut broker = McpBroker::new();
        let mut selected_servers: std::collections::BTreeSet<_> = view
            .grant
            .selected_resources()
            .into_iter()
            .map(|selection| selection.server_id)
            .collect();
        selected_servers.extend(
            self.servers
                .keys()
                .filter(|server_id| {
                    view.grant.query_disclosure(server_id) == MeetingQueryDisclosure::Redacted
                })
                .cloned(),
        );
        for server_id in selected_servers {
            let server = self.servers.get(&server_id).cloned().ok_or_else(|| {
                McpUiError::Invalid("A selected MCP source is no longer configured.".to_owned())
            })?;
            let credential = self.credentials.load(&server_id, &server.endpoint)?;
            broker
                .register(Arc::new(build_transport(server, credential)?))
                .map_err(|error| McpUiError::Invalid(error.to_string()))?;
        }
        if view.grant.has_run_inputs() {
            let actual = broker
                .run_fingerprint(&view.grant)
                .map_err(|error| McpUiError::Invalid(error.to_string()))?;
            if view.fingerprint.as_ref() != Some(&actual) {
                return Err(McpUiError::Invalid(
                    "The source grant changed before generation.".to_owned(),
                ));
            }
        }
        Ok(FrozenGrounding {
            session_id: view.session_id,
            grant: view.grant,
            fingerprint: view.fingerprint,
            source: Arc::new(broker),
        })
    }

    #[must_use]
    pub fn status_message(&self) -> Option<&str> {
        self.status_message.as_deref()
    }

    pub fn select_session(&mut self, session_id: SessionId, cx: &mut Context<Self>) {
        self.cancel_pending();
        self.selected_session = Some(session_id);
        self.grants
            .entry(session_id)
            .or_insert_with(|| SessionGrantView {
                session_id,
                grant: SessionContextGrant::default(),
                fingerprint: None,
                receipts: GrantReceiptState::NotRetrieved,
            });
        cx.notify();
    }

    pub fn configure_remote(
        &mut self,
        id: &str,
        display_name: &str,
        endpoint: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), McpUiError> {
        let id =
            ServerId::new(id.to_owned()).map_err(|error| McpUiError::Invalid(error.to_string()))?;
        let endpoint = HttpEndpoint::new_remote(endpoint)
            .map_err(|error| McpUiError::Invalid(error.to_string()))?;
        if display_name.trim().is_empty() {
            return Err(McpUiError::Invalid("Enter a source name.".to_owned()));
        }
        self.cancel_pending();
        let original_servers = self.servers.clone();
        let original_grants = self.grants.clone();
        let replaced_endpoint = self
            .servers
            .get(&id)
            .filter(|previous| previous.endpoint != endpoint)
            .map(|previous| previous.endpoint.clone());
        let replaced_secret = replaced_endpoint
            .as_ref()
            .map(|previous| self.credentials.load(&id, previous))
            .transpose()?
            .flatten();
        if replaced_endpoint.is_some() {
            self.remove_server_from_grants(&id)?;
        }
        let credential = self.credential_readiness(&id, &endpoint);
        self.servers.insert(
            id.clone(),
            ConfiguredServer {
                id: id.clone(),
                display_name: display_name.trim().to_owned(),
                endpoint,
                credential,
                health: ConnectionHealth::NotChecked,
                resources: Vec::new(),
            },
        );
        if let Some(previous) = replaced_endpoint.as_ref()
            && let Err(error) = self.credentials.delete(&id, previous)
        {
            self.servers = original_servers;
            self.grants = original_grants;
            return Err(error);
        }
        if let Err(error) = self.persist() {
            self.servers = original_servers;
            self.grants = original_grants;
            if let (Some(previous), Some(secret)) =
                (replaced_endpoint.as_ref(), replaced_secret.as_ref())
            {
                self.credentials.store(&id, previous, secret)?;
            }
            return Err(error);
        }
        cx.notify();
        Ok(())
    }

    pub fn store_bearer(
        &mut self,
        server_id: &ServerId,
        secret: SecretString,
        cx: &mut Context<Self>,
    ) -> Result<(), McpUiError> {
        let server = self
            .servers
            .get_mut(server_id)
            .ok_or_else(|| McpUiError::Invalid("Unknown MCP source.".to_owned()))?;
        self.credentials
            .store(server_id, &server.endpoint, &secret)?;
        server.credential = CredentialReadiness::Stored;
        server.health = ConnectionHealth::NotChecked;
        cx.notify();
        Ok(())
    }

    pub fn delete_bearer(
        &mut self,
        server_id: &ServerId,
        cx: &mut Context<Self>,
    ) -> Result<(), McpUiError> {
        if let Some(server) = self.servers.get_mut(server_id) {
            self.credentials.delete(server_id, &server.endpoint)?;
            server.credential = CredentialReadiness::None;
            server.health = ConnectionHealth::NotChecked;
        }
        cx.notify();
        Ok(())
    }

    pub fn begin_discovery(
        &mut self,
        server_id: ServerId,
        cx: &mut Context<Self>,
    ) -> Result<(), McpUiError> {
        if self.pending.is_some() {
            return Err(McpUiError::Busy);
        }
        let server = self
            .servers
            .get(&server_id)
            .cloned()
            .ok_or_else(|| McpUiError::Invalid("Unknown MCP source.".to_owned()))?;
        let credential = self.credentials.load(&server_id, &server.endpoint)?;
        self.generation = self.generation.saturating_add(1);
        let generation = self.generation;
        let cancellation = ContextCancellation::new();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker_server = server_id.clone();
        let handle = std::thread::Builder::new()
            .name("sotto-mcp-discovery".to_owned())
            .spawn(move || {
                let result = discover(server, credential, worker_cancellation)
                    .map(WorkerPayload::Catalog)
                    .map_err(|error| error.to_string());
                let _ = sender.send(WorkerResult {
                    generation,
                    server_id: Some(worker_server),
                    result,
                });
            })
            .map_err(|error| McpUiError::Invalid(format!("Could not start MCP check: {error}")))?;
        if let Some(server) = self.servers.get_mut(&server_id) {
            server.health = ConnectionHealth::Checking;
        }
        self.pending = Some(PendingWorker {
            generation,
            kind: WorkerKind::Discover,
            cancellation,
            receiver,
            handle,
        });
        cx.notify();
        Ok(())
    }

    pub fn toggle_resource(
        &mut self,
        server_id: ServerId,
        uri: ResourceUri,
        cx: &mut Context<Self>,
    ) -> Result<(), McpUiError> {
        let server = self
            .servers
            .get(&server_id)
            .ok_or_else(|| McpUiError::Invalid("Configure this MCP source first.".to_owned()))?;
        if !server.resources.iter().any(|resource| resource.uri == uri) {
            return Err(McpUiError::Invalid(
                "Discover this source and select an advertised resource.".to_owned(),
            ));
        }
        let session_id = self
            .selected_session
            .ok_or_else(|| McpUiError::Invalid("Select a completed meeting first.".to_owned()))?;
        let view = self
            .grants
            .entry(session_id)
            .or_insert_with(|| SessionGrantView {
                session_id,
                grant: SessionContextGrant::default(),
                fingerprint: None,
                receipts: GrantReceiptState::NotRetrieved,
            });
        let selected = view
            .grant
            .selected_resources()
            .iter()
            .any(|selection| selection.server_id == server_id && selection.uri == uri);
        let mut replacement = SessionContextGrant::new();
        for selection in view.grant.selected_resources() {
            if !(selected && selection.server_id == server_id && selection.uri == uri) {
                replacement
                    .allow_resource(selection.server_id, selection.uri)
                    .map_err(|error| McpUiError::Invalid(error.to_string()))?;
            }
        }
        if !selected {
            replacement
                .allow_resource(server_id.clone(), uri)
                .map_err(|error| McpUiError::Invalid(error.to_string()))?;
        }
        for server in self.servers.keys() {
            replacement.set_query_disclosure(server.clone(), view.grant.query_disclosure(server));
        }
        view.grant = replacement;
        self.refresh_fingerprint(session_id);
        self.persist()?;
        cx.notify();
        Ok(())
    }

    pub fn set_query_disclosure(
        &mut self,
        server_id: ServerId,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), McpUiError> {
        if !self.servers.contains_key(&server_id) {
            return Err(McpUiError::Invalid(
                "Configure this MCP source first.".to_owned(),
            ));
        }
        let session_id = self
            .selected_session
            .ok_or_else(|| McpUiError::Invalid("Select a completed meeting first.".to_owned()))?;
        let view = self
            .grants
            .entry(session_id)
            .or_insert_with(|| SessionGrantView {
                session_id,
                grant: SessionContextGrant::default(),
                fingerprint: None,
                receipts: GrantReceiptState::NotRetrieved,
            });
        view.grant.set_query_disclosure(
            server_id,
            if enabled {
                MeetingQueryDisclosure::Redacted
            } else {
                MeetingQueryDisclosure::None
            },
        );
        self.refresh_fingerprint(session_id);
        self.persist()?;
        cx.notify();
        Ok(())
    }

    pub fn remove_source(
        &mut self,
        server_id: &ServerId,
        cx: &mut Context<Self>,
    ) -> Result<(), McpUiError> {
        let Some(server) = self.servers.get(server_id).cloned() else {
            return Err(McpUiError::Invalid("Unknown MCP source.".to_owned()));
        };
        self.cancel_pending();
        let original_servers = self.servers.clone();
        let original_grants = self.grants.clone();
        let original_secret = self.credentials.load(server_id, &server.endpoint)?;
        self.servers.remove(server_id);
        self.remove_server_from_grants(server_id)?;
        if let Err(error) = self.credentials.delete(server_id, &server.endpoint) {
            self.servers = original_servers;
            self.grants = original_grants;
            return Err(error);
        }
        if let Err(error) = self.persist() {
            self.servers = original_servers;
            self.grants = original_grants;
            if let Some(secret) = original_secret.as_ref() {
                self.credentials
                    .store(server_id, &server.endpoint, secret)?;
            }
            return Err(error);
        }
        cx.notify();
        Ok(())
    }

    pub fn poll(&mut self, cx: &mut Context<Self>) -> bool {
        self.reap();
        let Some(pending) = self.pending.take() else {
            return false;
        };
        let result = match pending.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => {
                self.pending = Some(pending);
                return false;
            }
            Err(mpsc::TryRecvError::Disconnected) => WorkerResult {
                generation: pending.generation,
                server_id: None,
                result: Err("MCP worker stopped unexpectedly.".to_owned()),
            },
        };
        self.retired.push(pending.handle);
        if result.generation != self.generation {
            self.reap();
            return false;
        }
        match (pending.kind, result.server_id, result.result) {
            (WorkerKind::Discover, Some(server_id), Ok(WorkerPayload::Catalog(catalog))) => {
                if let Some(server) = self.servers.get_mut(&server_id) {
                    server.health = ConnectionHealth::Available {
                        resources: catalog.resources.len(),
                    };
                    server.resources = catalog.resources;
                }
            }
            (WorkerKind::Discover, Some(server_id), Err(error)) => {
                if let Some(server) = self.servers.get_mut(&server_id) {
                    server.health = ConnectionHealth::Unavailable(error);
                }
            }
            _ => self.status_message = Some("MCP worker returned an invalid result.".to_owned()),
        }
        self.reap();
        cx.notify();
        true
    }

    fn credential_readiness(
        &self,
        server_id: &ServerId,
        endpoint: &HttpEndpoint,
    ) -> CredentialReadiness {
        match self.credentials.load(server_id, endpoint) {
            Ok(Some(_)) => CredentialReadiness::Stored,
            Ok(None) => CredentialReadiness::None,
            Err(error) => CredentialReadiness::Error(error.to_string()),
        }
    }

    fn refresh_fingerprint(&mut self, session_id: SessionId) {
        let fingerprint = self.grants.get(&session_id).and_then(|view| {
            let has_grant_input = !view.grant.selected_resources().is_empty()
                || self.servers.keys().any(|server| {
                    view.grant.query_disclosure(server) == MeetingQueryDisclosure::Redacted
                });
            if !has_grant_input {
                return None;
            }
            build_fingerprint_broker(&self.servers)
                .ok()
                .and_then(|broker| broker.run_fingerprint(&view.grant).ok())
        });
        if let Some(view) = self.grants.get_mut(&session_id) {
            view.fingerprint = fingerprint;
            view.receipts = GrantReceiptState::NotRetrieved;
        }
    }

    fn remove_server_from_grants(&mut self, server_id: &ServerId) -> Result<(), McpUiError> {
        for view in self.grants.values_mut() {
            let mut replacement = SessionContextGrant::new();
            for selection in view.grant.selected_resources() {
                if &selection.server_id != server_id {
                    replacement
                        .allow_resource(selection.server_id, selection.uri)
                        .map_err(|error| McpUiError::Invalid(error.to_string()))?;
                }
            }
            for configured in self.servers.keys() {
                if configured != server_id {
                    replacement.set_query_disclosure(
                        configured.clone(),
                        view.grant.query_disclosure(configured),
                    );
                }
            }
            view.grant = replacement;
            view.fingerprint = None;
            view.receipts = GrantReceiptState::NotRetrieved;
        }
        Ok(())
    }

    fn cancel_pending(&mut self) {
        self.generation = self.generation.saturating_add(1);
        if let Some(pending) = self.pending.take() {
            pending.cancellation.cancel();
            self.retired.push(pending.handle);
        }
        self.reap();
    }

    fn reap(&mut self) {
        let mut running = Vec::new();
        for worker in self.retired.drain(..) {
            if worker.is_finished() {
                let _ = worker.join();
            } else {
                running.push(worker);
            }
        }
        self.retired = running;
    }

    fn load_persisted(&mut self) {
        let Some(path) = self.settings_path.as_deref() else {
            self.status_message = Some("MCP settings path is unavailable.".to_owned());
            return;
        };
        let persisted = match self.settings.load(path) {
            Ok(Some(value)) if value.version == SETTINGS_VERSION => value,
            Ok(Some(_)) => {
                self.status_message = Some(
                    "MCP settings used an unsupported version; sources remain off.".to_owned(),
                );
                return;
            }
            Ok(None) => return,
            Err(error) => {
                self.status_message = Some(error.to_string());
                return;
            }
        };
        for server in persisted.servers {
            let Ok(id) = ServerId::new(server.id) else {
                continue;
            };
            let Ok(endpoint) = HttpEndpoint::new_remote(&server.endpoint) else {
                continue;
            };
            let credential = self.credential_readiness(&id, &endpoint);
            self.servers.insert(
                id.clone(),
                ConfiguredServer {
                    id,
                    display_name: server.display_name,
                    endpoint,
                    credential,
                    health: ConnectionHealth::NotChecked,
                    resources: Vec::new(),
                },
            );
        }
        for persisted in persisted.grants {
            let session_id = SessionId::new(persisted.session_id);
            let mut grant = SessionContextGrant::new();
            for resource in persisted.resources {
                if let (Ok(server_id), Ok(uri)) = (
                    ServerId::new(resource.server_id),
                    ResourceUri::new(resource.uri),
                ) {
                    let _ = grant.allow_resource(server_id, uri);
                }
            }
            for server in persisted.query_disclosure_servers {
                if let Ok(server_id) = ServerId::new(server) {
                    grant.set_query_disclosure(server_id, MeetingQueryDisclosure::Redacted);
                }
            }
            self.grants.insert(
                session_id,
                SessionGrantView {
                    session_id,
                    grant,
                    fingerprint: None,
                    receipts: GrantReceiptState::NotRetrieved,
                },
            );
            self.refresh_fingerprint(session_id);
        }
    }

    fn persist(&self) -> Result<(), McpUiError> {
        let Some(path) = self.settings_path.as_deref() else {
            return Err(McpUiError::Persistence(
                "settings path unavailable".to_owned(),
            ));
        };
        let servers = self
            .servers
            .values()
            .map(|server| PersistedServer {
                id: server.id.as_str().to_owned(),
                display_name: server.display_name.clone(),
                endpoint: server.endpoint.as_str().to_owned(),
            })
            .collect();
        let grants = self
            .grants
            .values()
            .map(|view| PersistedGrant {
                session_id: view.session_id.get(),
                resources: view
                    .grant
                    .selected_resources()
                    .into_iter()
                    .map(|selection| PersistedResource {
                        server_id: selection.server_id.as_str().to_owned(),
                        uri: selection.uri.as_str().to_owned(),
                    })
                    .collect(),
                query_disclosure_servers: self
                    .servers
                    .keys()
                    .filter(|server| {
                        view.grant.query_disclosure(server) == MeetingQueryDisclosure::Redacted
                    })
                    .map(|server| server.as_str().to_owned())
                    .collect(),
            })
            .collect();
        self.settings.save(
            path,
            &PersistedMcpSettings {
                version: SETTINGS_VERSION,
                servers,
                grants,
            },
        )
    }
}

fn build_fingerprint_broker(
    servers: &BTreeMap<ServerId, ConfiguredServer>,
) -> Result<McpBroker, McpUiError> {
    let mut broker = McpBroker::new();
    for server in servers.values() {
        broker
            .register(Arc::new(build_transport(server.clone(), None)?))
            .map_err(|error| McpUiError::Invalid(error.to_string()))?;
    }
    Ok(broker)
}

impl Drop for McpController {
    fn drop(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.cancellation.cancel();
        }
    }
}

fn build_transport(
    server: ConfiguredServer,
    credential: Option<SecretString>,
) -> Result<RmcpResourceTransport, McpUiError> {
    let descriptor = ServerDescriptor {
        id: server.id,
        display_name: server.display_name,
        transport: TransportKind::StreamableHttp,
    };
    let connection = ServerConnection::StreamableHttp(server.endpoint);
    match credential {
        Some(secret) => RmcpResourceTransport::new_authenticated(
            descriptor,
            connection,
            BearerCredential::new(secret),
        ),
        None => RmcpResourceTransport::new(descriptor, connection),
    }
    .map_err(|error| McpUiError::Invalid(error.to_string()))
}

fn discover(
    server: ConfiguredServer,
    credential: Option<SecretString>,
    cancellation: ContextCancellation,
) -> Result<ResourceCatalog, McpUiError> {
    let id = server.id.clone();
    let transport = Arc::new(build_transport(server, credential)?);
    let mut broker = McpBroker::new();
    broker
        .register(transport)
        .map_err(|error| McpUiError::Invalid(error.to_string()))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| McpUiError::Invalid(error.to_string()))?;
    runtime
        .block_on(broker.discover_resources(&id, ContextBudget::default(), cancellation))
        .map_err(|error| McpUiError::Invalid(error.to_string()))
}

fn application_settings_path() -> Result<PathBuf, McpUiError> {
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| McpUiError::Persistence("could not locate home directory".to_owned()))?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Sotto")
        .join("mcp-settings.json"))
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic, reason = "test setup failures must stop the regression")]
    use std::{
        collections::BTreeMap,
        path::Path,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    use gpui::AppContext;
    use secrecy::{ExposeSecret, SecretString};

    use super::{
        ConfiguredServer, ConnectionHealth, CredentialReadiness, FileMcpSettingsStore,
        McpController, McpCredentialStore, McpSettingsStore, McpUiError, PersistedMcpSettings,
    };
    use mcp::{HttpEndpoint, ResourceDescriptor, ResourceUri, ServerId};
    use sotto_core::SessionId;

    #[derive(Default)]
    struct FakeCredentials(Mutex<BTreeMap<String, SecretString>>);

    impl FakeCredentials {
        fn contains(&self, server_id: &ServerId, endpoint: &HttpEndpoint) -> bool {
            self.0
                .lock()
                .unwrap_or_else(|_| panic!("credential lock poisoned"))
                .contains_key(&format!("{}@{}", server_id.as_str(), endpoint.as_str()))
        }
    }

    impl McpCredentialStore for FakeCredentials {
        fn store(
            &self,
            server_id: &ServerId,
            endpoint: &HttpEndpoint,
            secret: &SecretString,
        ) -> Result<(), McpUiError> {
            self.0
                .lock()
                .map_err(|_| McpUiError::Credential("poisoned".to_owned()))?
                .insert(
                    format!("{}@{}", server_id.as_str(), endpoint.as_str()),
                    SecretString::from(secret.expose_secret().to_owned()),
                );
            Ok(())
        }

        fn load(
            &self,
            server_id: &ServerId,
            endpoint: &HttpEndpoint,
        ) -> Result<Option<SecretString>, McpUiError> {
            Ok(self
                .0
                .lock()
                .map_err(|_| McpUiError::Credential("poisoned".to_owned()))?
                .get(&format!("{}@{}", server_id.as_str(), endpoint.as_str()))
                .map(|value| SecretString::from(value.expose_secret().to_owned())))
        }

        fn delete(&self, server_id: &ServerId, endpoint: &HttpEndpoint) -> Result<(), McpUiError> {
            self.0
                .lock()
                .map_err(|_| McpUiError::Credential("poisoned".to_owned()))?
                .remove(&format!("{}@{}", server_id.as_str(), endpoint.as_str()));
            Ok(())
        }
    }

    struct ToggleSettingsStore {
        fail_save: AtomicBool,
    }

    impl ToggleSettingsStore {
        fn new() -> Self {
            Self {
                fail_save: AtomicBool::new(false),
            }
        }

        fn set_fail_save(&self, fail: bool) {
            self.fail_save.store(fail, Ordering::SeqCst);
        }
    }

    impl McpSettingsStore for ToggleSettingsStore {
        fn load(&self, path: &Path) -> Result<Option<PersistedMcpSettings>, McpUiError> {
            FileMcpSettingsStore.load(path)
        }

        fn save(&self, path: &Path, settings: &PersistedMcpSettings) -> Result<(), McpUiError> {
            if self.fail_save.load(Ordering::SeqCst) {
                Err(McpUiError::Persistence("injected failure".to_owned()))
            } else {
                FileMcpSettingsStore.save(path, settings)
            }
        }
    }

    fn controller() -> (tempfile::TempDir, Arc<FakeCredentials>, McpController) {
        let directory = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir: {error}"));
        let credentials = Arc::new(FakeCredentials::default());
        let controller =
            McpController::load(Some(directory.path().join("mcp.json")), credentials.clone());
        (directory, credentials, controller)
    }

    fn advertise_resource(
        controller: &mut McpController,
        server_id: &ServerId,
        resource_uri: &ResourceUri,
    ) {
        if let Some(server) = controller.servers.get_mut(server_id) {
            server.resources.push(ResourceDescriptor {
                server_id: server_id.clone(),
                uri: resource_uri.clone(),
                name: "Plan".to_owned(),
                title: None,
                description: None,
                mime_type: Some("text/plain".to_owned()),
                size: None,
                last_modified: None,
            });
        }
    }

    #[gpui::test]
    fn resources_default_off_and_endpoint_replacement_clears_old_approval(
        cx: &mut gpui::TestAppContext,
    ) {
        let (_directory, credentials, controller) = controller();
        let entity = cx.new(|_| controller);
        let session_id = SessionId::new(40);
        let server_id = ServerId::new("project.docs").unwrap_or_else(|error| panic!("id: {error}"));
        let resource_uri =
            ResourceUri::new("docs://project/plan").unwrap_or_else(|error| panic!("uri: {error}"));
        entity.update(cx, |controller, cx| {
            controller
                .configure_remote(
                    "project.docs",
                    "Project docs",
                    "https://one.example/mcp",
                    cx,
                )
                .unwrap_or_else(|error| panic!("configure: {error}"));
            controller.select_session(session_id, cx);
            assert!(
                controller
                    .selected_grant()
                    .is_some_and(|view| view.grant.is_empty())
            );
            controller
                .store_bearer(&server_id, SecretString::from("old-token"), cx)
                .unwrap_or_else(|error| panic!("token: {error}"));
            advertise_resource(controller, &server_id, &resource_uri);
            controller
                .toggle_resource(server_id.clone(), resource_uri.clone(), cx)
                .unwrap_or_else(|error| panic!("grant: {error}"));
            controller
                .set_query_disclosure(server_id.clone(), true, cx)
                .unwrap_or_else(|error| panic!("disclosure: {error}"));
            assert!(
                controller
                    .selected_grant()
                    .is_some_and(|view| view.fingerprint.is_some())
            );
            controller
                .configure_remote(
                    "project.docs",
                    "Project docs",
                    "https://two.example/mcp",
                    cx,
                )
                .unwrap_or_else(|error| panic!("replace: {error}"));
            let view = controller
                .selected_grant()
                .unwrap_or_else(|| panic!("grant missing"));
            assert!(view.grant.is_empty());
            assert_eq!(
                view.grant.query_disclosure(&server_id),
                mcp::MeetingQueryDisclosure::None
            );
            assert!(view.fingerprint.is_none());
        });
        let old_endpoint = HttpEndpoint::new("https://one.example/mcp")
            .unwrap_or_else(|error| panic!("endpoint: {error}"));
        assert!(!credentials.contains(&server_id, &old_endpoint));
    }

    #[gpui::test]
    fn persisted_identifiers_endpoints_and_grants_roundtrip_without_bearer(
        cx: &mut gpui::TestAppContext,
    ) {
        let (directory, credentials, controller) = controller();
        let settings_path = directory.path().join("mcp.json");
        let entity = cx.new(|_| controller);
        let session_id = SessionId::new(41);
        let server_id = ServerId::new("project.docs").unwrap_or_else(|error| panic!("id: {error}"));
        let resource_uri =
            ResourceUri::new("docs://project/plan").unwrap_or_else(|error| panic!("uri: {error}"));
        let endpoint = HttpEndpoint::new("https://docs.example/mcp")
            .unwrap_or_else(|error| panic!("endpoint: {error}"));
        let canary = "bearer-token-must-not-persist";
        entity.update(cx, |controller, cx| {
            controller
                .configure_remote("project.docs", "Project docs", endpoint.as_str(), cx)
                .unwrap_or_else(|error| panic!("configure: {error}"));
            controller
                .store_bearer(&server_id, SecretString::from(canary), cx)
                .unwrap_or_else(|error| panic!("token: {error}"));
            controller.select_session(session_id, cx);
            advertise_resource(controller, &server_id, &resource_uri);
            controller
                .toggle_resource(server_id.clone(), resource_uri.clone(), cx)
                .unwrap_or_else(|error| panic!("grant: {error}"));
            controller
                .set_query_disclosure(server_id.clone(), true, cx)
                .unwrap_or_else(|error| panic!("disclosure: {error}"));
        });

        let json = std::fs::read_to_string(&settings_path)
            .unwrap_or_else(|error| panic!("read persisted settings: {error}"));
        assert!(json.contains("project.docs"));
        assert!(json.contains("https://docs.example/mcp"));
        assert!(json.contains("docs://project/plan"));
        assert!(!json.contains(canary));
        assert!(!json.contains("bearer"));

        let reloaded = McpController::load(Some(settings_path), credentials);
        let reloaded = cx.new(|_| reloaded);
        reloaded.update(cx, |controller, cx| {
            controller.select_session(session_id, cx);
            let server = controller
                .servers()
                .into_iter()
                .find(|server| server.id == server_id)
                .unwrap_or_else(|| panic!("server missing after roundtrip"));
            assert_eq!(server.endpoint, endpoint);
            let grant = controller
                .selected_grant()
                .unwrap_or_else(|| panic!("grant missing after roundtrip"));
            assert!(grant.grant.selected_resources().iter().any(|selection| {
                selection.server_id == server_id && selection.uri == resource_uri
            }));
            assert_eq!(
                grant.grant.query_disclosure(&server_id),
                mcp::MeetingQueryDisclosure::Redacted
            );
        });
    }

    #[gpui::test]
    fn source_removal_deletes_exact_key_and_cleans_future_grant(cx: &mut gpui::TestAppContext) {
        let (directory, credentials, controller) = controller();
        let settings_path = directory.path().join("mcp.json");
        let entity = cx.new(|_| controller);
        let session_id = SessionId::new(42);
        let server_id = ServerId::new("project.docs").unwrap_or_else(|error| panic!("id: {error}"));
        let resource_uri =
            ResourceUri::new("docs://project/plan").unwrap_or_else(|error| panic!("uri: {error}"));
        let endpoint = HttpEndpoint::new("https://docs.example/mcp")
            .unwrap_or_else(|error| panic!("endpoint: {error}"));
        entity.update(cx, |controller, cx| {
            controller
                .configure_remote("project.docs", "Project docs", endpoint.as_str(), cx)
                .unwrap_or_else(|error| panic!("configure: {error}"));
            controller
                .store_bearer(&server_id, SecretString::from("remove-me"), cx)
                .unwrap_or_else(|error| panic!("token: {error}"));
            controller.select_session(session_id, cx);
            advertise_resource(controller, &server_id, &resource_uri);
            controller
                .toggle_resource(server_id.clone(), resource_uri.clone(), cx)
                .unwrap_or_else(|error| panic!("grant: {error}"));
            controller
                .set_query_disclosure(server_id.clone(), true, cx)
                .unwrap_or_else(|error| panic!("disclosure: {error}"));
            controller
                .remove_source(&server_id, cx)
                .unwrap_or_else(|error| panic!("remove: {error}"));
            assert!(controller.servers().is_empty());
            let grant = controller
                .selected_grant()
                .unwrap_or_else(|| panic!("grant missing"));
            assert!(grant.grant.is_empty());
            assert_eq!(
                grant.grant.query_disclosure(&server_id),
                mcp::MeetingQueryDisclosure::None
            );
            assert!(grant.fingerprint.is_none());
        });
        assert!(!credentials.contains(&server_id, &endpoint));

        let reloaded = McpController::load(Some(settings_path), credentials);
        assert!(reloaded.servers().is_empty());
        let persisted = reloaded
            .grants
            .get(&session_id)
            .unwrap_or_else(|| panic!("persisted grant missing"));
        assert!(persisted.grant.is_empty());
        assert!(persisted.fingerprint.is_none());
    }

    #[gpui::test]
    fn endpoint_replacement_persistence_failure_rolls_back_memory_and_keychain(
        cx: &mut gpui::TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir: {error}"));
        let credentials = Arc::new(FakeCredentials::default());
        let settings = Arc::new(ToggleSettingsStore::new());
        let controller = McpController::load_with_settings(
            Some(directory.path().join("mcp.json")),
            credentials.clone(),
            settings.clone(),
        );
        let entity = cx.new(|_| controller);
        let server_id = ServerId::new("project.docs").unwrap_or_else(|error| panic!("id: {error}"));
        let old_endpoint = HttpEndpoint::new("https://one.example/mcp")
            .unwrap_or_else(|error| panic!("endpoint: {error}"));
        let new_endpoint = HttpEndpoint::new("https://two.example/mcp")
            .unwrap_or_else(|error| panic!("endpoint: {error}"));
        entity.update(cx, |controller, cx| {
            controller
                .configure_remote("project.docs", "Project docs", old_endpoint.as_str(), cx)
                .unwrap_or_else(|error| panic!("configure: {error}"));
            controller
                .store_bearer(&server_id, SecretString::from("keep-me"), cx)
                .unwrap_or_else(|error| panic!("token: {error}"));
        });
        settings.set_fail_save(true);
        entity.update(cx, |controller, cx| {
            assert!(matches!(
                controller.configure_remote(
                    "project.docs",
                    "Replacement",
                    new_endpoint.as_str(),
                    cx,
                ),
                Err(McpUiError::Persistence(_))
            ));
            let server = controller
                .servers()
                .into_iter()
                .next()
                .unwrap_or_else(|| panic!("rolled back server missing"));
            assert_eq!(server.endpoint, old_endpoint);
            assert_eq!(server.display_name, "Project docs");
        });
        assert!(credentials.contains(&server_id, &old_endpoint));
        assert!(!credentials.contains(&server_id, &new_endpoint));
    }

    #[gpui::test]
    fn source_removal_persistence_failure_rolls_back_memory_grant_and_keychain(
        cx: &mut gpui::TestAppContext,
    ) {
        let directory = tempfile::tempdir().unwrap_or_else(|error| panic!("tempdir: {error}"));
        let credentials = Arc::new(FakeCredentials::default());
        let settings = Arc::new(ToggleSettingsStore::new());
        let controller = McpController::load_with_settings(
            Some(directory.path().join("mcp.json")),
            credentials.clone(),
            settings.clone(),
        );
        let entity = cx.new(|_| controller);
        let session_id = SessionId::new(43);
        let server_id = ServerId::new("project.docs").unwrap_or_else(|error| panic!("id: {error}"));
        let resource_uri =
            ResourceUri::new("docs://project/plan").unwrap_or_else(|error| panic!("uri: {error}"));
        let endpoint = HttpEndpoint::new("https://docs.example/mcp")
            .unwrap_or_else(|error| panic!("endpoint: {error}"));
        entity.update(cx, |controller, cx| {
            controller
                .configure_remote("project.docs", "Project docs", endpoint.as_str(), cx)
                .unwrap_or_else(|error| panic!("configure: {error}"));
            controller
                .store_bearer(&server_id, SecretString::from("keep-me"), cx)
                .unwrap_or_else(|error| panic!("token: {error}"));
            controller.select_session(session_id, cx);
            advertise_resource(controller, &server_id, &resource_uri);
            controller
                .toggle_resource(server_id.clone(), resource_uri.clone(), cx)
                .unwrap_or_else(|error| panic!("grant: {error}"));
            controller
                .set_query_disclosure(server_id.clone(), true, cx)
                .unwrap_or_else(|error| panic!("disclosure: {error}"));
        });
        settings.set_fail_save(true);
        entity.update(cx, |controller, cx| {
            assert!(matches!(
                controller.remove_source(&server_id, cx),
                Err(McpUiError::Persistence(_))
            ));
            assert_eq!(controller.servers().len(), 1);
            let grant = controller
                .selected_grant()
                .unwrap_or_else(|| panic!("rolled back grant missing"));
            assert!(grant.grant.selected_resources().iter().any(|selection| {
                selection.server_id == server_id && selection.uri == resource_uri
            }));
            assert_eq!(
                grant.grant.query_disclosure(&server_id),
                mcp::MeetingQueryDisclosure::Redacted
            );
            assert!(grant.fingerprint.is_some());
        });
        assert!(credentials.contains(&server_id, &endpoint));
    }

    #[test]
    fn public_app_states_redact_server_controlled_values() {
        let canary = "tokenleakcanary";
        let readiness = CredentialReadiness::Error(canary.to_owned());
        let health = ConnectionHealth::Unavailable(canary.to_owned());
        let errors = [
            McpUiError::Persistence(canary.to_owned()),
            McpUiError::Invalid(canary.to_owned()),
            McpUiError::Credential(canary.to_owned()),
        ];
        let server = ConfiguredServer {
            id: ServerId::new("project.docs").unwrap_or_else(|error| panic!("id: {error}")),
            display_name: canary.to_owned(),
            endpoint: HttpEndpoint::new("https://mcp.example/test")
                .unwrap_or_else(|error| panic!("endpoint: {error}")),
            credential: readiness.clone(),
            health: health.clone(),
            resources: Vec::new(),
        };
        assert!(!format!("{server:?}").contains(canary));
        assert!(!format!("{readiness:?}").contains(canary));
        assert!(!format!("{health:?}").contains(canary));
        for error in errors {
            assert!(!format!("{error:?}").contains(canary));
            assert!(!error.to_string().contains(canary));
        }
    }
}
