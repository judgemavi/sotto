//! Product-facing reasoning backend identity and role selection.
//!
//! Transport implementations stay behind [`sotto_core::CompletionProvider`]. This
//! module adds only the provider-layer information needed to select, display, and
//! cache a connector without teaching `core` about Codex, OpenAI, authentication,
//! or subprocesses.

use std::{
    collections::BTreeSet,
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use screen::AuthorizedReasoningImage;
use sotto_core::{
    BoxFuture, BoxStream, CancellationToken, CompletionProvider, CompletionRequest, Delta,
    ProviderError, ReasoningOutput, ReasoningRequest,
};

use crate::{ReasoningProvider, text_reasoning_provider};

/// Stable v1 id for the user-installed Codex CLI connector.
pub const CODEX_CLI_BACKEND_ID: &str = "openai.codex-cli";
/// Stable v1 id for the direct OpenAI Responses API connector.
pub const OPENAI_RESPONSES_BACKEND_ID: &str = "openai.responses";

const CACHE_FINGERPRINT_VERSION: u32 = 1;

/// An open-ended, persisted connector id rather than a closed vendor enum.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BackendId(String);

impl BackendId {
    pub fn new(value: impl Into<String>) -> Result<Self, BackendContractError> {
        let value = value.into();
        if value.is_empty()
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'-' | b'_')
            })
        {
            return Err(BackendContractError::InvalidBackendId(value));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BackendId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Behavior a backend can normalize into Sotto's reasoning contract.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum BackendCapability {
    Streaming,
    Cancellation,
    /// The connector can ask the model for syntactically valid JSON objects.
    JsonObjectOutput,
    /// The connector can constrain output against a caller-supplied JSON Schema.
    JsonSchemaOutput,
    UsageReporting,
    ImageInput,
    ToolCalling,
    /// The connector can enforce the caller's maximum output-token count.
    MaxTokens,
    /// The connector can apply the caller's sampling temperature.
    Temperature,
}

/// A caller-set sampling control considered during backend normalization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SamplingControl {
    MaxTokens,
    Temperature,
    /// The caller asked for a syntactically guaranteed JSON object.
    ///
    /// Not a sampling knob, but it travels the same path: a control the caller states, a backend
    /// may be unable to honour, and whose loss must be visible rather than silent. Losing it means
    /// the reply is only prompt-shaped JSON, so the caller's parser becomes the sole guarantee.
    JsonObjectOutput,
}

impl fmt::Display for SamplingControl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MaxTokens => formatter.write_str("max_tokens"),
            Self::Temperature => formatter.write_str("temperature"),
            Self::JsonObjectOutput => formatter.write_str("json_object output"),
        }
    }
}

/// Whether a caller permits a backend to proceed without one requested control.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlRequirement {
    Required,
    DowngradeAllowed,
}

/// Explicit policy applied at the one backend-bound request preparation seam.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestNormalizationPolicy {
    pub max_tokens: ControlRequirement,
    pub temperature: ControlRequirement,
    pub json_object_output: ControlRequirement,
}

impl RequestNormalizationPolicy {
    /// Insight's bounded structured-output policy. Both losses are visible in the result log.
    ///
    /// `max_tokens` is tolerable for these explicit, cancellable subscription-backed calls even
    /// though losing it removes the requested latency ceiling; the visible downgrade is required.
    /// `temperature = 0` is a determinism preference; schema and citation validators remain
    /// authoritative when the backend lacks that knob.
    ///
    /// `json_object_output` may also downgrade. Insight parses every structured reply defensively —
    /// it strips code fences, accepts a bare result or an action envelope, and fails closed on a
    /// shape it does not recognise — so a backend that cannot guarantee JSON syntactically still
    /// produces either a valid parse or a clean error, never a silently wrong one.
    pub const INSIGHT: Self = Self {
        max_tokens: ControlRequirement::DowngradeAllowed,
        temperature: ControlRequirement::DowngradeAllowed,
        json_object_output: ControlRequirement::DowngradeAllowed,
    };

    /// No caller-set control may be discarded.
    pub const STRICT: Self = Self {
        max_tokens: ControlRequirement::Required,
        temperature: ControlRequirement::Required,
        json_object_output: ControlRequirement::Required,
    };
}

/// One control deliberately removed before dispatch, with the backend named for auditability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RequestNormalization {
    pub backend_id: BackendId,
    pub control: SamplingControl,
}

/// A downgrade attributed to one dispatch through a resolved backend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedRequestNormalization {
    pub dispatch_id: u64,
    pub normalization: RequestNormalization,
}

/// The prepared request and every caller-authorized downgrade applied to it.
#[derive(Clone, Debug)]
pub struct PreparedReasoningRequest {
    pub request: ReasoningRequest,
    pub normalizations: Vec<RequestNormalization>,
}

/// A small extensible set used by settings and capability gates.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BackendCapabilities(BTreeSet<BackendCapability>);

impl BackendCapabilities {
    #[must_use]
    pub fn new(capabilities: impl IntoIterator<Item = BackendCapability>) -> Self {
        Self(capabilities.into_iter().collect())
    }

    #[must_use]
    pub fn reasoning_baseline() -> Self {
        Self::new([
            BackendCapability::Streaming,
            BackendCapability::Cancellation,
        ])
    }

    #[must_use]
    pub fn contains(&self, capability: BackendCapability) -> bool {
        self.0.contains(&capability)
    }

    pub fn iter(&self) -> impl Iterator<Item = BackendCapability> + '_ {
        self.0.iter().copied()
    }

    fn missing_reasoning_baseline(&self) -> Option<BackendCapability> {
        Self::reasoning_baseline()
            .iter()
            .find(|capability| !self.contains(*capability))
    }
}

/// Who owns authentication for a connector.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AuthKind {
    /// The optional CLI owns its login and refresh lifecycle.
    CodexLogin,
    /// Sotto stores a user-supplied API key in the OS credential store.
    ApiKey,
    /// Reserved for future local connectors that require no credential.
    None,
}

/// Actionable readiness state. `Ready` is the only state that can be resolved.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AuthStatus {
    Ready,
    NeedsLogin,
    NeedsApiKey,
    Unavailable { reason: String },
    Failed { reason: String },
}

impl AuthStatus {
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    const fn is_compatible_with(&self, kind: AuthKind) -> bool {
        match kind {
            AuthKind::ApiKey => matches!(
                self,
                Self::Ready | Self::NeedsApiKey | Self::Unavailable { .. } | Self::Failed { .. }
            ),
            AuthKind::CodexLogin => matches!(
                self,
                Self::Ready | Self::NeedsLogin | Self::Unavailable { .. } | Self::Failed { .. }
            ),
            AuthKind::None => {
                matches!(
                    self,
                    Self::Ready | Self::Unavailable { .. } | Self::Failed { .. }
                )
            }
        }
    }
}

impl fmt::Display for AuthStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready => formatter.write_str("ready"),
            Self::NeedsLogin => formatter.write_str("login required"),
            Self::NeedsApiKey => formatter.write_str("API key required"),
            Self::Unavailable { reason } => write!(formatter, "unavailable: {reason}"),
            Self::Failed { reason } => write!(formatter, "authentication failed: {reason}"),
        }
    }
}

/// Cache identity for one connector/model/contract-revision combination.
///
/// Length-prefixing makes the persisted representation unambiguous without hashing
/// away the evidence needed to diagnose a cache miss.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct BackendFingerprint(String);

impl BackendFingerprint {
    fn new(backend_id: &BackendId, model_id: &str, connector_revision: u32) -> Self {
        Self(format!(
            "sotto-backend-v{CACHE_FINGERPRINT_VERSION}:{}:{}:{}:{}:r{connector_revision}",
            backend_id.as_str().len(),
            backend_id.as_str(),
            model_id.len(),
            model_id,
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BackendFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Immutable connector metadata pinned alongside every resolved call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendDescriptor {
    id: BackendId,
    display_name: String,
    model_id: String,
    capabilities: BackendCapabilities,
    auth_kind: AuthKind,
    auth_status: AuthStatus,
    fingerprint: BackendFingerprint,
}

impl BackendDescriptor {
    pub fn new(
        id: BackendId,
        display_name: impl Into<String>,
        model_id: impl Into<String>,
        connector_revision: u32,
        capabilities: BackendCapabilities,
        auth_kind: AuthKind,
        auth_status: AuthStatus,
    ) -> Result<Self, BackendContractError> {
        let display_name = display_name.into();
        if display_name.trim().is_empty() {
            return Err(BackendContractError::EmptyDisplayName);
        }
        let model_id = model_id.into();
        if model_id.trim().is_empty() {
            return Err(BackendContractError::EmptyModelId);
        }
        if let Some(capability) = capabilities.missing_reasoning_baseline() {
            return Err(BackendContractError::MissingCapability(capability));
        }
        if !auth_status.is_compatible_with(auth_kind) {
            return Err(BackendContractError::IncompatibleAuthStatus {
                kind: auth_kind,
                status: auth_status,
            });
        }
        let fingerprint = BackendFingerprint::new(&id, &model_id, connector_revision);
        Ok(Self {
            id,
            display_name,
            model_id,
            capabilities,
            auth_kind,
            auth_status,
            fingerprint,
        })
    }

    #[must_use]
    pub fn id(&self) -> &BackendId {
        &self.id
    }

    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    #[must_use]
    pub const fn capabilities(&self) -> &BackendCapabilities {
        &self.capabilities
    }

    #[must_use]
    pub const fn auth_kind(&self) -> AuthKind {
        self.auth_kind
    }

    #[must_use]
    pub const fn auth_status(&self) -> &AuthStatus {
        &self.auth_status
    }

    #[must_use]
    pub const fn fingerprint(&self) -> &BackendFingerprint {
        &self.fingerprint
    }

    /// Refreshes transient readiness without changing cache identity.
    pub fn with_auth_status(mut self, status: AuthStatus) -> Result<Self, BackendContractError> {
        if !status.is_compatible_with(self.auth_kind) {
            return Err(BackendContractError::IncompatibleAuthStatus {
                kind: self.auth_kind,
                status,
            });
        }
        self.auth_status = status;
        Ok(self)
    }
}

/// Runtime purpose assigned to a configured backend.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Role {
    Watcher,
    Suggester,
    Summarizer,
}

struct RegisteredBackend {
    descriptor: BackendDescriptor,
    provider: Arc<dyn ReasoningProvider>,
}

struct NormalizedReasoningProvider {
    descriptor: BackendDescriptor,
    inner: Arc<dyn ReasoningProvider>,
    observations: Arc<Mutex<Vec<ObservedRequestNormalization>>>,
    diagnostic_observations: Arc<Mutex<Vec<ObservedRequestNormalization>>>,
    next_dispatch_id: Arc<AtomicU64>,
}

impl NormalizedReasoningProvider {
    fn prepare(
        &self,
        request: ReasoningRequest,
    ) -> Result<PreparedReasoningRequest, ProviderError> {
        prepare_reasoning_request(
            &self.descriptor,
            request,
            RequestNormalizationPolicy::INSIGHT,
        )
    }

    fn record(&self, normalizations: &[RequestNormalization]) {
        let dispatch_id = self.next_dispatch_id.fetch_add(1, Ordering::Relaxed);
        let observed = normalizations
            .iter()
            .cloned()
            .map(|normalization| ObservedRequestNormalization {
                dispatch_id,
                normalization,
            })
            .collect::<Vec<_>>();
        if let Ok(mut observations) = self.observations.lock() {
            observations.extend_from_slice(&observed);
        }
        if let Ok(mut diagnostics) = self.diagnostic_observations.lock() {
            diagnostics.extend(observed);
        }
    }
}

impl CompletionProvider for NormalizedReasoningProvider {
    fn stream(
        &self,
        request: CompletionRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
    {
        let prepared = self.prepare(ReasoningRequest::text(request));
        Box::pin(async move {
            let prepared = prepared?;
            self.record(&prepared.normalizations);
            self.inner
                .stream(prepared.request.completion, cancellation)
                .await
        })
    }

    fn stream_reasoning(
        &self,
        request: ReasoningRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
    {
        let prepared = self.prepare(request);
        Box::pin(async move {
            let prepared = prepared?;
            self.record(&prepared.normalizations);
            self.inner
                .stream_reasoning(prepared.request, cancellation)
                .await
        })
    }

    fn model_id(&self) -> &str {
        self.inner.model_id()
    }
}

impl ReasoningProvider for NormalizedReasoningProvider {
    fn take_request_normalizations(&self) -> Vec<ObservedRequestNormalization> {
        self.observations.lock().map_or_else(
            |_| Vec::new(),
            |mut observations| std::mem::take(&mut *observations),
        )
    }

    fn supports_advanced_capability(&self, capability: BackendCapability) -> bool {
        self.inner.supports_advanced_capability(capability)
    }

    fn stream_advanced_reasoning(
        &self,
        request: ReasoningRequest,
        image: Option<AuthorizedReasoningImage>,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
    {
        let prepared = self.prepare(request);
        Box::pin(async move {
            let prepared = prepared?;
            self.record(&prepared.normalizations);
            self.inner
                .stream_advanced_reasoning(prepared.request, image, cancellation)
                .await
        })
    }
}

/// Prepares one request against the immutable capabilities pinned to a resolved call.
///
/// All backend-bound dispatch paths use this function. A missing required control fails before
/// connector I/O; an allowed downgrade is returned and must remain observable to the caller.
pub fn prepare_reasoning_request(
    descriptor: &BackendDescriptor,
    mut request: ReasoningRequest,
    policy: RequestNormalizationPolicy,
) -> Result<PreparedReasoningRequest, ProviderError> {
    let mut normalizations = Vec::new();
    normalize_control(
        descriptor,
        request.completion.max_tokens.is_some(),
        BackendCapability::MaxTokens,
        SamplingControl::MaxTokens,
        policy.max_tokens,
        &mut normalizations,
    )?;
    if normalizations
        .iter()
        .any(|item| item.control == SamplingControl::MaxTokens)
    {
        request.completion.max_tokens = None;
    }
    normalize_control(
        descriptor,
        request.completion.temperature.is_some(),
        BackendCapability::Temperature,
        SamplingControl::Temperature,
        policy.temperature,
        &mut normalizations,
    )?;
    if normalizations
        .iter()
        .any(|item| item.control == SamplingControl::Temperature)
    {
        request.completion.temperature = None;
    }
    normalize_control(
        descriptor,
        request.output == ReasoningOutput::JsonObject,
        BackendCapability::JsonObjectOutput,
        SamplingControl::JsonObjectOutput,
        policy.json_object_output,
        &mut normalizations,
    )?;
    if normalizations
        .iter()
        .any(|item| item.control == SamplingControl::JsonObjectOutput)
    {
        // Downgrading to text is what actually stops the connector asking for a guarantee it
        // cannot provide. Leaving `JsonObject` here would send the request on unchanged and the
        // recorded normalization would describe something that never happened.
        request.output = ReasoningOutput::Text;
    }
    Ok(PreparedReasoningRequest {
        request,
        normalizations,
    })
}

fn normalize_control(
    descriptor: &BackendDescriptor,
    requested: bool,
    capability: BackendCapability,
    control: SamplingControl,
    requirement: ControlRequirement,
    normalizations: &mut Vec<RequestNormalization>,
) -> Result<(), ProviderError> {
    if !requested || descriptor.capabilities().contains(capability) {
        return Ok(());
    }
    match requirement {
        ControlRequirement::DowngradeAllowed => {
            normalizations.push(RequestNormalization {
                backend_id: descriptor.id().clone(),
                control,
            });
            Ok(())
        }
        ControlRequirement::Required => Err(ProviderError::InvalidRequest(format!(
            "backend {} cannot honour required control {control}",
            descriptor.id()
        ))),
    }
}

/// A call-pinned provider and its immutable identity.
#[derive(Clone)]
pub struct ResolvedBackend {
    descriptor: BackendDescriptor,
    provider: Arc<dyn ReasoningProvider>,
    normalization_observations: Arc<Mutex<Vec<ObservedRequestNormalization>>>,
    next_dispatch_id: Arc<AtomicU64>,
}

impl ResolvedBackend {
    #[must_use]
    pub const fn descriptor(&self) -> &BackendDescriptor {
        &self.descriptor
    }

    #[must_use]
    pub fn provider(&self) -> Arc<dyn ReasoningProvider> {
        Arc::new(NormalizedReasoningProvider {
            descriptor: self.descriptor.clone(),
            inner: Arc::clone(&self.provider),
            observations: Arc::new(Mutex::new(Vec::new())),
            diagnostic_observations: Arc::clone(&self.normalization_observations),
            next_dispatch_id: Arc::clone(&self.next_dispatch_id),
        })
    }

    /// Returns the explicit control downgrades observed by calls through this resolved backend.
    ///
    /// The log is shared with clones of this resolution and remains available after the provider
    /// call completes. Poisoning is treated as an empty diagnostic rather than a call failure.
    #[must_use]
    pub fn normalization_observations(&self) -> Vec<ObservedRequestNormalization> {
        self.normalization_observations
            .lock()
            .map_or_else(|_| Vec::new(), |observations| observations.clone())
    }

    #[must_use]
    pub const fn cache_fingerprint(&self) -> &BackendFingerprint {
        self.descriptor.fingerprint()
    }
}

/// Registered connectors and independent role selections.
///
/// Resolving clones both descriptor and `Arc`, so later registration or selection
/// changes cannot retarget an in-flight call.
#[derive(Default)]
pub struct Registry {
    backends: HashMap<BackendId, RegisteredBackend>,
    selections: HashMap<Role, BackendId>,
}

impl Registry {
    pub fn register(
        &mut self,
        descriptor: BackendDescriptor,
        provider: Arc<dyn CompletionProvider>,
    ) -> Result<(), RegistryError> {
        if descriptor
            .capabilities()
            .contains(BackendCapability::ImageInput)
        {
            return Err(RegistryError::ReasoningProviderRequired(
                descriptor.id().clone(),
            ));
        }
        self.register_reasoning(descriptor, text_reasoning_provider(provider))
    }

    /// Registers a provider without erasing its screen-authorized reasoning seam.
    pub fn register_reasoning(
        &mut self,
        descriptor: BackendDescriptor,
        provider: Arc<dyn ReasoningProvider>,
    ) -> Result<(), RegistryError> {
        for capability in [
            BackendCapability::JsonSchemaOutput,
            BackendCapability::ImageInput,
        ] {
            if descriptor.capabilities().contains(capability)
                && !provider.supports_advanced_capability(capability)
            {
                return Err(RegistryError::UnsupportedCapability {
                    id: descriptor.id().clone(),
                    capability,
                });
            }
        }
        if descriptor.model_id() != provider.model_id() {
            return Err(RegistryError::ModelMismatch {
                descriptor: descriptor.model_id().to_owned(),
                provider: provider.model_id().to_owned(),
            });
        }
        let id = descriptor.id().clone();
        if self.backends.contains_key(&id) {
            return Err(RegistryError::DuplicateBackend(id));
        }
        self.backends.insert(
            id,
            RegisteredBackend {
                descriptor,
                provider,
            },
        );
        Ok(())
    }

    pub fn select(&mut self, role: Role, backend: Option<&BackendId>) -> Result<(), RegistryError> {
        match backend {
            Some(id) if self.backends.contains_key(id) => {
                self.selections.insert(role, id.clone());
            }
            Some(id) => return Err(RegistryError::UnknownBackend(id.clone())),
            None => {
                self.selections.remove(&role);
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn selected_id(&self, role: Role) -> Option<&BackendId> {
        self.selections.get(&role)
    }

    #[must_use]
    pub fn descriptor(&self, id: &BackendId) -> Option<&BackendDescriptor> {
        self.backends.get(id).map(|backend| &backend.descriptor)
    }

    pub fn descriptors(&self) -> impl Iterator<Item = &BackendDescriptor> {
        self.backends.values().map(|backend| &backend.descriptor)
    }

    /// Refreshes transient connector readiness without replacing its provider or
    /// changing cache identity. Already resolved calls retain their pinned snapshot.
    pub fn refresh_auth_status(
        &mut self,
        id: &BackendId,
        status: AuthStatus,
    ) -> Result<(), RegistryError> {
        let backend = self
            .backends
            .get_mut(id)
            .ok_or_else(|| RegistryError::UnknownBackend(id.clone()))?;
        if !status.is_compatible_with(backend.descriptor.auth_kind()) {
            return Err(RegistryError::IncompatibleAuthStatus {
                id: id.clone(),
                kind: backend.descriptor.auth_kind(),
                status,
            });
        }
        backend.descriptor.auth_status = status;
        Ok(())
    }

    pub fn resolve(&self, role: Role) -> Result<Option<ResolvedBackend>, RegistryError> {
        let Some(id) = self.selections.get(&role) else {
            return Ok(None);
        };
        let backend = self
            .backends
            .get(id)
            .ok_or_else(|| RegistryError::UnknownBackend(id.clone()))?;
        if !backend.descriptor.auth_status().is_ready() {
            return Err(RegistryError::BackendNotReady {
                id: id.clone(),
                status: backend.descriptor.auth_status().clone(),
            });
        }
        let normalization_observations = Arc::new(Mutex::new(Vec::new()));
        Ok(Some(ResolvedBackend {
            descriptor: backend.descriptor.clone(),
            provider: Arc::clone(&backend.provider),
            normalization_observations,
            next_dispatch_id: Arc::new(AtomicU64::new(1)),
        }))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendContractError {
    InvalidBackendId(String),
    EmptyDisplayName,
    EmptyModelId,
    MissingCapability(BackendCapability),
    IncompatibleAuthStatus { kind: AuthKind, status: AuthStatus },
}

impl fmt::Display for BackendContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBackendId(id) => write!(formatter, "invalid backend id {id:?}"),
            Self::EmptyDisplayName => formatter.write_str("backend display name must not be empty"),
            Self::EmptyModelId => formatter.write_str("backend model id must not be empty"),
            Self::MissingCapability(capability) => {
                write!(
                    formatter,
                    "backend is missing required capability {capability:?}"
                )
            }
            Self::IncompatibleAuthStatus { kind, status } => write!(
                formatter,
                "authentication status {status} is incompatible with {kind:?}"
            ),
        }
    }
}

impl std::error::Error for BackendContractError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistryError {
    DuplicateBackend(BackendId),
    UnknownBackend(BackendId),
    ReasoningProviderRequired(BackendId),
    UnsupportedCapability {
        id: BackendId,
        capability: BackendCapability,
    },
    ModelMismatch {
        descriptor: String,
        provider: String,
    },
    BackendNotReady {
        id: BackendId,
        status: AuthStatus,
    },
    IncompatibleAuthStatus {
        id: BackendId,
        kind: AuthKind,
        status: AuthStatus,
    },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateBackend(id) => write!(formatter, "backend {id} is already registered"),
            Self::UnknownBackend(id) => write!(formatter, "unknown backend {id}"),
            Self::ReasoningProviderRequired(id) => write!(
                formatter,
                "backend {id} advertises image input and requires advanced reasoning registration"
            ),
            Self::UnsupportedCapability { id, capability } => write!(
                formatter,
                "backend {id} advertises {capability:?}, but its provider does not implement it"
            ),
            Self::ModelMismatch {
                descriptor,
                provider,
            } => write!(
                formatter,
                "backend model mismatch: descriptor {descriptor:?}, provider {provider:?}"
            ),
            Self::BackendNotReady { id, status } => {
                write!(formatter, "backend {id} is not ready: {status}")
            }
            Self::IncompatibleAuthStatus { id, kind, status } => write!(
                formatter,
                "backend {id} cannot use authentication status {status} with {kind:?}"
            ),
        }
    }
}

impl std::error::Error for RegistryError {}

#[cfg(test)]
mod tests {
    use super::{
        AuthKind, AuthStatus, BackendCapabilities, BackendCapability, BackendContractError,
        BackendDescriptor, BackendId, ControlRequirement, Registry, RegistryError,
        RequestNormalizationPolicy, Role, SamplingControl, prepare_reasoning_request,
    };
    use crate::{ReasoningProvider, text_reasoning_provider};
    use futures_util::{StreamExt, stream};
    use screen::AuthorizedReasoningImage;
    use sotto_core::{
        BoxFuture, BoxStream, CancellationToken, CompletionProvider, CompletionRequest, Delta,
        ProviderError, ReasoningRequest, StopReason, Usage,
    };
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    struct FakeProvider {
        model: String,
    }

    impl CompletionProvider for FakeProvider {
        fn stream(
            &self,
            _request: CompletionRequest,
            cancellation: CancellationToken,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
        {
            Box::pin(async move {
                let output = stream::once(async move {
                    cancellation.cancelled().await;
                    Ok(Delta {
                        text: String::new(),
                        is_final: true,
                        usage: Some(Usage {
                            input_tokens: 3,
                            output_tokens: 1,
                            cache_read_tokens: 0,
                            cache_write_tokens: 0,
                        }),
                        stop_reason: Some(StopReason::Aborted),
                    })
                });
                Ok(Box::pin(output) as BoxStream<'static, Result<Delta, ProviderError>>)
            })
        }

        fn model_id(&self) -> &str {
            &self.model
        }
    }

    struct AdvancedFakeProvider {
        inner: FakeProvider,
        advanced_called: Arc<AtomicBool>,
    }

    struct CapturingProvider {
        model: String,
        observed: Arc<Mutex<Vec<CompletionRequest>>>,
    }

    impl CompletionProvider for CapturingProvider {
        fn stream(
            &self,
            request: CompletionRequest,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
        {
            if let Ok(mut observed) = self.observed.lock() {
                observed.push(request);
            }
            Box::pin(async {
                Ok(Box::pin(stream::empty()) as BoxStream<'static, Result<Delta, ProviderError>>)
            })
        }

        fn model_id(&self) -> &str {
            &self.model
        }
    }

    impl ReasoningProvider for CapturingProvider {}

    impl CompletionProvider for AdvancedFakeProvider {
        fn stream(
            &self,
            request: CompletionRequest,
            cancellation: CancellationToken,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
        {
            self.inner.stream(request, cancellation)
        }

        fn model_id(&self) -> &str {
            self.inner.model_id()
        }
    }

    impl ReasoningProvider for AdvancedFakeProvider {
        fn supports_advanced_capability(&self, capability: BackendCapability) -> bool {
            capability == BackendCapability::ImageInput
        }

        fn stream_advanced_reasoning(
            &self,
            request: ReasoningRequest,
            _image: Option<AuthorizedReasoningImage>,
            cancellation: CancellationToken,
        ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
        {
            self.advanced_called.store(true, Ordering::Release);
            self.stream_reasoning(request, cancellation)
        }
    }

    fn descriptor(id: &str, model: &str) -> Result<BackendDescriptor, Box<dyn std::error::Error>> {
        Ok(BackendDescriptor::new(
            BackendId::new(id)?,
            id,
            model,
            1,
            BackendCapabilities::new([
                BackendCapability::Streaming,
                BackendCapability::Cancellation,
                BackendCapability::UsageReporting,
            ]),
            AuthKind::None,
            AuthStatus::Ready,
        )?)
    }

    fn provider(model: &str) -> Arc<dyn CompletionProvider> {
        Arc::new(FakeProvider {
            model: model.to_owned(),
        })
    }

    fn image_descriptor(
        id: &str,
        model: &str,
    ) -> Result<BackendDescriptor, Box<dyn std::error::Error>> {
        Ok(BackendDescriptor::new(
            BackendId::new(id)?,
            id,
            model,
            1,
            BackendCapabilities::new([
                BackendCapability::Streaming,
                BackendCapability::Cancellation,
                BackendCapability::ImageInput,
            ]),
            AuthKind::None,
            AuthStatus::Ready,
        )?)
    }

    #[test]
    fn baseline_does_not_conflate_json_object_and_json_schema_output() {
        let baseline = BackendCapabilities::reasoning_baseline();
        assert!(
            baseline.contains(BackendCapability::Streaming),
            "all reasoning backends must stream"
        );
        assert!(
            baseline.contains(BackendCapability::Cancellation),
            "all reasoning backends must support cancellation"
        );
        assert!(
            !baseline.contains(BackendCapability::JsonObjectOutput),
            "JSON object output is optional connector behavior"
        );
        assert!(
            !baseline.contains(BackendCapability::JsonSchemaOutput),
            "schema-constrained output must be advertised separately"
        );
        let json_object = BackendCapabilities::new([
            BackendCapability::Streaming,
            BackendCapability::Cancellation,
            BackendCapability::JsonObjectOutput,
        ]);
        assert!(
            json_object.contains(BackendCapability::JsonObjectOutput),
            "JSON object support must be independently discoverable"
        );
        assert!(
            !json_object.contains(BackendCapability::JsonSchemaOutput),
            "valid JSON alone must never imply schema conformance"
        );
    }

    #[test]
    fn fake_third_backend_registers_without_a_vendor_enum_change()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = Registry::default();
        let fake = descriptor("example.future-runtime", "same-model")?;
        let id = fake.id().clone();
        registry.register(fake, provider("same-model"))?;
        registry.select(Role::Summarizer, Some(&id))?;

        let resolved = registry
            .resolve(Role::Summarizer)?
            .ok_or("selected backend must resolve")?;
        assert_eq!(
            resolved.descriptor().id().as_str(),
            "example.future-runtime",
            "a new backend id must not require a closed enum arm"
        );
        Ok(())
    }

    #[tokio::test]
    async fn advanced_registration_survives_selection_and_resolution()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = Registry::default();
        let descriptor = image_descriptor("example.image-runtime", "vision-model")?;
        let id = descriptor.id().clone();
        let advanced_called = Arc::new(AtomicBool::new(false));
        let provider = Arc::new(AdvancedFakeProvider {
            inner: FakeProvider {
                model: "vision-model".to_owned(),
            },
            advanced_called: Arc::clone(&advanced_called),
        });
        registry.register_reasoning(descriptor, provider)?;
        registry.select(Role::Summarizer, Some(&id))?;
        let resolved = registry
            .resolve(Role::Summarizer)?
            .ok_or("advanced backend must resolve")?;
        let cancellation = CancellationToken::new();
        let mut output = resolved
            .provider()
            .stream_advanced_reasoning(
                ReasoningRequest::text(CompletionRequest {
                    model: "vision-model".to_owned(),
                    system: None,
                    messages: Vec::new(),
                    max_tokens: None,
                    temperature: None,
                    stop: Vec::new(),
                }),
                None,
                cancellation.clone(),
            )
            .await?;
        cancellation.cancel();
        let _ = output.next().await;
        assert!(
            advanced_called.load(Ordering::Acquire),
            "resolution must retain the advanced reasoning trait object"
        );
        Ok(())
    }

    #[test]
    fn text_registration_rejects_image_capability_before_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = Registry::default();
        let descriptor = image_descriptor("example.image-runtime", "vision-model")?;
        let id = descriptor.id().clone();
        assert_eq!(
            registry.register(descriptor, provider("vision-model")),
            Err(RegistryError::ReasoningProviderRequired(id.clone())),
            "image capability cannot be registered through the text-only adapter"
        );
        assert!(
            registry.descriptor(&id).is_none(),
            "failed registration must not partially mutate the registry"
        );
        Ok(())
    }

    #[test]
    fn advanced_registration_rejects_default_adapter_capability_overclaim()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = Registry::default();
        let descriptor = image_descriptor("example.default-image", "vision-model")?;
        let id = descriptor.id().clone();
        let adapter = text_reasoning_provider(provider("vision-model"));

        assert_eq!(
            registry.register_reasoning(descriptor, adapter),
            Err(RegistryError::UnsupportedCapability {
                id: id.clone(),
                capability: BackendCapability::ImageInput,
            }),
            "the advanced entry point must not turn a default image rejection into capability proof"
        );
        assert!(
            registry.descriptor(&id).is_none(),
            "capability overclaim must be rejected before registry mutation"
        );
        Ok(())
    }

    #[test]
    fn advanced_registration_checks_json_schema_independently()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = Registry::default();
        let descriptor = BackendDescriptor::new(
            BackendId::new("example.schema-runtime")?,
            "Schema runtime",
            "schema-model",
            1,
            BackendCapabilities::new([
                BackendCapability::Streaming,
                BackendCapability::Cancellation,
                BackendCapability::JsonSchemaOutput,
            ]),
            AuthKind::None,
            AuthStatus::Ready,
        )?;
        let id = descriptor.id().clone();

        assert_eq!(
            registry.register_reasoning(
                descriptor,
                text_reasoning_provider(provider("schema-model")),
            ),
            Err(RegistryError::UnsupportedCapability {
                id,
                capability: BackendCapability::JsonSchemaOutput,
            }),
            "schema metadata requires an implementation that explicitly reports schema dispatch"
        );
        Ok(())
    }

    #[test]
    fn descriptor_rejects_auth_status_from_a_different_auth_kind()
    -> Result<(), Box<dyn std::error::Error>> {
        let result = BackendDescriptor::new(
            BackendId::new("openai.responses")?,
            "OpenAI API",
            "model",
            1,
            BackendCapabilities::reasoning_baseline(),
            AuthKind::ApiKey,
            AuthStatus::NeedsLogin,
        );
        assert!(
            matches!(
                result,
                Err(BackendContractError::IncompatibleAuthStatus {
                    kind: AuthKind::ApiKey,
                    status: AuthStatus::NeedsLogin,
                })
            ),
            "API-key descriptors must not expose a CLI-login action"
        );
        let valid = BackendDescriptor::new(
            BackendId::new("openai.responses")?,
            "OpenAI API",
            "model",
            1,
            BackendCapabilities::reasoning_baseline(),
            AuthKind::ApiKey,
            AuthStatus::Ready,
        )?;
        assert!(
            matches!(
                valid.with_auth_status(AuthStatus::NeedsLogin),
                Err(BackendContractError::IncompatibleAuthStatus {
                    kind: AuthKind::ApiKey,
                    status: AuthStatus::NeedsLogin,
                })
            ),
            "descriptor status replacement must enforce the same authentication invariant"
        );
        Ok(())
    }

    #[test]
    fn auth_refresh_rejects_incoherent_state_without_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = Registry::default();
        let descriptor = BackendDescriptor::new(
            BackendId::new("openai.responses")?,
            "OpenAI API",
            "model",
            1,
            BackendCapabilities::reasoning_baseline(),
            AuthKind::ApiKey,
            AuthStatus::Ready,
        )?;
        let id = descriptor.id().clone();
        registry.register(descriptor, provider("model"))?;

        assert!(
            matches!(
                registry.refresh_auth_status(&id, AuthStatus::NeedsLogin),
                Err(RegistryError::IncompatibleAuthStatus {
                    kind: AuthKind::ApiKey,
                    status: AuthStatus::NeedsLogin,
                    ..
                })
            ),
            "readiness refresh must preserve the connector's authentication model"
        );
        assert_eq!(
            registry.descriptor(&id).map(BackendDescriptor::auth_status),
            Some(&AuthStatus::Ready),
            "a rejected readiness refresh must not mutate the registered descriptor"
        );
        Ok(())
    }

    #[test]
    fn role_and_cache_identity_include_backend_not_only_model()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = Registry::default();
        let codex = descriptor("openai.codex-cli", "shared-model")?;
        let responses = descriptor("openai.responses", "shared-model")?;
        let codex_id = codex.id().clone();
        let responses_id = responses.id().clone();
        assert_ne!(
            codex.fingerprint(),
            responses.fingerprint(),
            "different connector paths must never share a derived-view cache key"
        );
        registry.register(codex, provider("shared-model"))?;
        registry.register(responses, provider("shared-model"))?;
        registry.select(Role::Watcher, Some(&codex_id))?;
        registry.select(Role::Summarizer, Some(&responses_id))?;

        assert_eq!(
            registry.selected_id(Role::Watcher),
            Some(&codex_id),
            "watcher selection must remain independent"
        );
        assert_eq!(
            registry.selected_id(Role::Summarizer),
            Some(&responses_id),
            "summarizer selection must remain independent"
        );
        Ok(())
    }

    #[tokio::test]
    async fn resolved_call_stays_pinned_and_preserves_cancellation_and_usage()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = Registry::default();
        let first = descriptor("openai.codex-cli", "first")?;
        let second = descriptor("openai.responses", "second")?;
        let first_id = first.id().clone();
        let second_id = second.id().clone();
        registry.register(first, provider("first"))?;
        registry.register(second, provider("second"))?;
        registry.select(Role::Suggester, Some(&first_id))?;
        let pinned = registry
            .resolve(Role::Suggester)?
            .ok_or("first backend must resolve")?;
        registry.select(Role::Suggester, Some(&second_id))?;

        assert_eq!(
            pinned.provider().model_id(),
            "first",
            "settings changes must not retarget an already resolved call"
        );
        let cancellation = CancellationToken::new();
        let mut deltas = pinned
            .provider()
            .stream(
                CompletionRequest {
                    model: "first".to_owned(),
                    system: None,
                    messages: Vec::new(),
                    max_tokens: None,
                    temperature: None,
                    stop: Vec::new(),
                },
                cancellation.clone(),
            )
            .await?;
        cancellation.cancel();
        let final_delta = deltas
            .next()
            .await
            .ok_or("cancelled provider must emit a terminal delta")??;
        assert_eq!(
            final_delta.stop_reason,
            Some(StopReason::Aborted),
            "registry resolution must preserve cancellation semantics"
        );
        assert_eq!(
            final_delta.usage.map(|usage| usage.output_tokens),
            Some(1),
            "registry resolution must preserve provider usage"
        );
        Ok(())
    }

    #[test]
    fn no_selection_is_normal_but_unready_selection_is_actionable()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = Registry::default();
        assert!(
            registry.resolve(Role::Watcher)?.is_none(),
            "no reasoning must be an ordinary configuration"
        );
        let unavailable = BackendDescriptor::new(
            BackendId::new("openai.codex-cli")?,
            "Codex subscription",
            "model",
            1,
            BackendCapabilities::reasoning_baseline(),
            AuthKind::CodexLogin,
            AuthStatus::NeedsLogin,
        )?;
        let id = unavailable.id().clone();
        registry.register(unavailable, provider("model"))?;
        registry.select(Role::Watcher, Some(&id))?;
        assert!(
            matches!(
                registry.resolve(Role::Watcher),
                Err(RegistryError::BackendNotReady {
                    status: AuthStatus::NeedsLogin,
                    ..
                })
            ),
            "a selected but unauthenticated backend must not masquerade as disabled reasoning"
        );
        Ok(())
    }

    #[test]
    fn auth_refresh_preserves_fingerprint_and_already_pinned_calls()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = Registry::default();
        let codex = BackendDescriptor::new(
            BackendId::new("openai.codex-cli")?,
            "Codex subscription",
            "model",
            1,
            BackendCapabilities::reasoning_baseline(),
            AuthKind::CodexLogin,
            AuthStatus::Ready,
        )?;
        let id = codex.id().clone();
        let fingerprint = codex.fingerprint().clone();
        registry.register(codex, provider("model"))?;
        registry.select(Role::Summarizer, Some(&id))?;
        let pinned = registry
            .resolve(Role::Summarizer)?
            .ok_or("ready backend must resolve")?;

        registry.refresh_auth_status(&id, AuthStatus::NeedsLogin)?;

        assert_eq!(
            registry.descriptor(&id).map(BackendDescriptor::fingerprint),
            Some(&fingerprint),
            "a login-status refresh must not invalidate derived-view identity"
        );
        assert_eq!(
            pinned.descriptor().auth_status(),
            &AuthStatus::Ready,
            "a readiness refresh must not alter an already pinned call"
        );
        assert!(
            matches!(
                registry.resolve(Role::Summarizer),
                Err(RegistryError::BackendNotReady {
                    status: AuthStatus::NeedsLogin,
                    ..
                })
            ),
            "new calls must observe refreshed readiness"
        );
        Ok(())
    }

    #[test]
    fn required_unsupported_control_names_control_and_backend()
    -> Result<(), Box<dyn std::error::Error>> {
        let descriptor = descriptor("example.no-sampling", "model")?;
        let request = ReasoningRequest::json_object(CompletionRequest {
            model: "model".to_owned(),
            system: None,
            messages: Vec::new(),
            max_tokens: Some(200),
            temperature: Some(0.0),
            stop: Vec::new(),
        });
        let policy = RequestNormalizationPolicy {
            max_tokens: ControlRequirement::Required,
            temperature: ControlRequirement::DowngradeAllowed,
            json_object_output: ControlRequirement::DowngradeAllowed,
        };
        let error = match prepare_reasoning_request(&descriptor, request, policy) {
            Ok(_) => return Err("required unsupported max_tokens must fail".into()),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(
            message.contains("max_tokens"),
            "error must name the control: {message}"
        );
        assert!(
            message.contains("example.no-sampling"),
            "error must name the backend: {message}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn resolved_dispatch_normalizes_once_and_retains_observable_outcome()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = Registry::default();
        let descriptor = descriptor("example.no-sampling", "model")?;
        let id = descriptor.id().clone();
        let observed = Arc::new(Mutex::new(Vec::new()));
        registry.register_reasoning(
            descriptor,
            Arc::new(CapturingProvider {
                model: "model".to_owned(),
                observed: Arc::clone(&observed),
            }),
        )?;
        registry.select(Role::Summarizer, Some(&id))?;
        let resolved = registry
            .resolve(Role::Summarizer)?
            .ok_or("selected backend must resolve")?;
        let request = ReasoningRequest::json_object(CompletionRequest {
            model: "model".to_owned(),
            system: None,
            messages: Vec::new(),
            max_tokens: Some(4_096),
            temperature: Some(0.0),
            stop: Vec::new(),
        });
        let mut stream = resolved
            .provider()
            .stream_advanced_reasoning(request, None, CancellationToken::new())
            .await?;
        assert!(stream.next().await.is_none());

        let observed = observed.lock().map_err(|_| "capture poisoned")?;
        assert_eq!(
            observed.len(),
            1,
            "connector must receive exactly one request"
        );
        assert_eq!(observed[0].max_tokens, None);
        assert_eq!(observed[0].temperature, None);
        assert_eq!(
            resolved.normalization_observations(),
            vec![
                super::ObservedRequestNormalization {
                    dispatch_id: 1,
                    normalization: super::RequestNormalization {
                        backend_id: id.clone(),
                        control: SamplingControl::MaxTokens,
                    },
                },
                super::ObservedRequestNormalization {
                    dispatch_id: 1,
                    normalization: super::RequestNormalization {
                        backend_id: id.clone(),
                        control: SamplingControl::Temperature,
                    },
                },
                super::ObservedRequestNormalization {
                    dispatch_id: 1,
                    normalization: super::RequestNormalization {
                        backend_id: id,
                        control: SamplingControl::JsonObjectOutput,
                    },
                },
            ],
            "every tolerated loss must remain queryable on the resolved call"
        );
        Ok(())
    }
}
