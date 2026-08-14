use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// Stable application-owned identity for one configured MCP server.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServerId(String);

impl ServerId {
    pub fn new(value: impl Into<String>) -> Result<Self, ContextGrantError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'-' | b'_')
            })
        {
            return Err(ContextGrantError::InvalidServerId);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ServerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ServerId")
            .field(&redacted_fingerprint(&self.0))
            .finish()
    }
}

impl fmt::Display for ServerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&redacted_fingerprint(&self.0))
    }
}

/// Exact resource identifier selected by the user.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceUri(String);

impl ResourceUri {
    pub fn new(value: impl Into<String>) -> Result<Self, ContextGrantError> {
        let value = value.into();
        if value.is_empty() || value.len() > 4_096 || value.contains('\0') {
            return Err(ContextGrantError::InvalidResourceUri);
        }
        let parsed = url::Url::parse(&value).map_err(|_| ContextGrantError::InvalidResourceUri)?;
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(ContextGrantError::UnsafeResourceUri);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ResourceUri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ResourceUri")
            .field(&redacted_fingerprint(&self.0))
            .finish()
    }
}

impl fmt::Display for ResourceUri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&redacted_fingerprint(&self.0))
    }
}

/// Whether a configured server starts locally or is contacted over the network.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    StreamableHttp,
}

/// Credential-free information safe to display and persist.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServerDescriptor {
    pub id: ServerId,
    pub display_name: String,
    pub transport: TransportKind,
}

impl fmt::Debug for ServerDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerDescriptor")
            .field("id", &self.id)
            .field("display_name", &"<redacted>")
            .field("transport", &self.transport)
            .finish()
    }
}

/// One resource advertised by an explicitly contacted server.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResourceDescriptor {
    pub server_id: ServerId,
    pub uri: ResourceUri,
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub mime_type: Option<String>,
    pub size: Option<u64>,
    pub last_modified: Option<String>,
}

impl fmt::Debug for ResourceDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceDescriptor")
            .field("server_id", &self.server_id)
            .field("uri", &self.uri)
            .field("name", &"<redacted>")
            .field("title", &self.title.as_ref().map(|_| "<redacted>"))
            .field(
                "description",
                &self.description.as_ref().map(|_| "<redacted>"),
            )
            .field("mime_type", &self.mime_type.as_ref().map(|_| "<redacted>"))
            .field("size", &self.size)
            .field(
                "last_modified",
                &self.last_modified.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// A separately gated future policy for sending meeting-derived search text.
///
/// T039 performs resource list/read only, so neither mode currently transmits a
/// query. Carrying the choice now prevents later search support from being
/// smuggled into an ordinary resource grant.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingQueryDisclosure {
    #[default]
    None,
    Redacted,
}

/// Cache/run identity over the exact connection, resource, and disclosure snapshot.
#[derive(Clone, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GrantRunFingerprint(String);

impl GrantRunFingerprint {
    pub(crate) fn from_digest(digest: String) -> Self {
        Self(format!("mcp-grant-v1-{digest}"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn from_persisted(value: String) -> ContextResult<Self> {
        let Some(digest) = value.strip_prefix("mcp-grant-v1-") else {
            return Err(ContextError::InvalidBundle);
        };
        if !is_sha256_hex(digest) {
            return Err(ContextError::InvalidBundle);
        }
        Ok(Self(value))
    }
}

impl fmt::Debug for GrantRunFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GrantRunFingerprint(<redacted>)")
    }
}

/// Explicit server/resource selection for one meeting or post-call operation.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResourceSelection {
    pub server_id: ServerId,
    pub uri: ResourceUri,
}

impl fmt::Debug for ResourceSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceSelection")
            .field("server_id", &self.server_id)
            .field("uri", &self.uri)
            .finish()
    }
}

/// No servers and no meeting-query disclosure is the ordinary default.
#[derive(Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionContextGrant {
    selections: BTreeMap<ServerId, Vec<ResourceUri>>,
    query_disclosures: BTreeSet<ServerId>,
}

impl fmt::Debug for SessionContextGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionContextGrant")
            .field("server_count", &self.selections.len())
            .field(
                "resource_count",
                &self.selections.values().map(Vec::len).sum::<usize>(),
            )
            .field(
                "query_disclosure_server_count",
                &self.query_disclosures.len(),
            )
            .finish()
    }
}

impl SessionContextGrant {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow_resource(
        &mut self,
        server_id: ServerId,
        uri: ResourceUri,
    ) -> Result<(), ContextGrantError> {
        let resources = self.selections.entry(server_id).or_default();
        match resources.binary_search(&uri) {
            Ok(_) => Err(ContextGrantError::DuplicateSelection),
            Err(index) => {
                resources.insert(index, uri);
                Ok(())
            }
        }
    }

    pub fn set_query_disclosure(&mut self, server_id: ServerId, value: MeetingQueryDisclosure) {
        match value {
            MeetingQueryDisclosure::None => {
                self.query_disclosures.remove(&server_id);
            }
            MeetingQueryDisclosure::Redacted => {
                self.query_disclosures.insert(server_id);
            }
        }
    }

    #[must_use]
    pub fn query_disclosure(&self, server_id: &ServerId) -> MeetingQueryDisclosure {
        if self.query_disclosures.contains(server_id) {
            MeetingQueryDisclosure::Redacted
        } else {
            MeetingQueryDisclosure::None
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.selections.is_empty()
    }

    #[must_use]
    pub fn has_run_inputs(&self) -> bool {
        !self.selections.is_empty() || !self.query_disclosures.is_empty()
    }

    pub(crate) fn selections(&self) -> impl Iterator<Item = (&ServerId, &[ResourceUri])> {
        self.selections
            .iter()
            .map(|(server, resources)| (server, resources.as_slice()))
    }

    pub(crate) fn query_disclosures(&self) -> impl Iterator<Item = &ServerId> {
        self.query_disclosures.iter()
    }

    #[must_use]
    pub fn selected_resources(&self) -> Vec<ResourceSelection> {
        self.selections()
            .flat_map(|(server_id, resources)| {
                resources.iter().cloned().map(|uri| ResourceSelection {
                    server_id: server_id.clone(),
                    uri,
                })
            })
            .collect()
    }
}

/// Hard limits applied before any evidence can enter a reasoning prompt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextBudget {
    pub max_servers: usize,
    pub max_selected_resources: usize,
    pub max_catalog_pages: usize,
    pub max_catalog_resources: usize,
    pub max_resource_bytes: usize,
    pub max_bundle_bytes: usize,
    pub max_estimated_tokens: usize,
    pub max_transport_message_bytes: usize,
    pub call_timeout: Duration,
}

impl Default for ContextBudget {
    fn default() -> Self {
        Self {
            max_servers: 4,
            max_selected_resources: 12,
            max_catalog_pages: 8,
            max_catalog_resources: 1_000,
            max_resource_bytes: 128 * 1_024,
            max_bundle_bytes: 512 * 1_024,
            max_estimated_tokens: 96 * 1_024,
            max_transport_message_bytes: 2 * 1_024 * 1_024,
            call_timeout: Duration::from_secs(15),
        }
    }
}

impl ContextBudget {
    pub(crate) fn validate(self) -> Result<Self, ContextError> {
        if self.max_servers == 0
            || self.max_selected_resources == 0
            || self.max_catalog_pages == 0
            || self.max_catalog_resources == 0
            || self.max_resource_bytes == 0
            || self.max_bundle_bytes == 0
            || self.max_estimated_tokens == 0
            || self.max_transport_message_bytes == 0
            || self.call_timeout.is_zero()
        {
            return Err(ContextError::InvalidBudget);
        }
        Ok(self)
    }
}

/// Sotto-owned cancellation value; consumers do not depend on an SDK token.
#[derive(Clone, Default)]
pub struct ContextCancellation(CancellationToken);

impl ContextCancellation {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.cancel();
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    pub(crate) async fn cancelled(&self) {
        self.0.cancelled().await;
    }
}

impl fmt::Debug for ContextCancellation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContextCancellation")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

/// Opaque id the model may cite. It is computed by Sotto, never accepted from a server.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EvidenceId(String);

impl EvidenceId {
    pub(crate) fn from_digest(digest: &str) -> Self {
        Self(format!("mcp-evidence-v1-{digest}"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Exact provenance retained with every included excerpt.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceReceipt {
    pub server_id: ServerId,
    pub resource_uri: ResourceUri,
    pub content_sha256: String,
    pub retrieved_at_unix_ms: u64,
    pub original_bytes: usize,
    pub included_bytes: usize,
    pub truncated: bool,
    pub last_modified: Option<String>,
}

impl fmt::Debug for SourceReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceReceipt")
            .field("server_id", &self.server_id)
            .field("resource_uri", &self.resource_uri)
            .field("content_sha256", &self.content_sha256)
            .field("retrieved_at_unix_ms", &self.retrieved_at_unix_ms)
            .field("original_bytes", &self.original_bytes)
            .field("included_bytes", &self.included_bytes)
            .field("truncated", &self.truncated)
            .field(
                "last_modified",
                &self.last_modified.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// Text safe to pass only as labelled, untrusted evidence.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextExcerpt {
    pub evidence_id: EvidenceId,
    pub title: String,
    pub text: String,
    pub receipt: SourceReceipt,
}

impl fmt::Debug for ContextExcerpt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContextExcerpt")
            .field("evidence_id", &self.evidence_id)
            .field("title", &"<redacted>")
            .field("text_bytes", &self.text.len())
            .field("receipt", &self.receipt)
            .finish()
    }
}

/// Deterministically ordered context plus a cache identity that excludes wall-clock retrieval time.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContextBundle {
    excerpts: Vec<ContextExcerpt>,
    digest: String,
    integrity_digest: String,
    estimated_tokens: usize,
}

impl ContextBundle {
    pub(crate) fn new(
        excerpts: Vec<ContextExcerpt>,
        digest: String,
        estimated_tokens: usize,
    ) -> Self {
        let integrity_digest = bundle_integrity_digest(&excerpts, &digest, estimated_tokens);
        Self {
            excerpts,
            digest,
            integrity_digest,
            estimated_tokens,
        }
    }

    #[must_use]
    pub fn empty() -> Self {
        Self::new(
            Vec::new(),
            crate::transport::sha256_hex(b"sotto-mcp-bundle-v1"),
            0,
        )
    }

    #[must_use]
    pub fn excerpts(&self) -> &[ContextExcerpt] {
        &self.excerpts
    }

    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    #[must_use]
    pub fn integrity_digest(&self) -> &str {
        &self.integrity_digest
    }

    #[must_use]
    pub const fn estimated_tokens(&self) -> usize {
        self.estimated_tokens
    }

    /// Recomputes every portable identity before durable evidence is admitted on replay.
    pub fn validate_integrity(&self) -> ContextResult<()> {
        let mut evidence_ids = BTreeSet::new();
        let mut resources = BTreeSet::new();
        let mut previous_resource = None;
        let mut estimated_tokens = 0_usize;
        for excerpt in &self.excerpts {
            if ServerId::new(excerpt.receipt.server_id.as_str().to_owned()).is_err()
                || ResourceUri::new(excerpt.receipt.resource_uri.as_str().to_owned()).is_err()
            {
                return Err(ContextError::InvalidBundle);
            }
            let resource = (
                excerpt.receipt.server_id.clone(),
                excerpt.receipt.resource_uri.clone(),
            );
            if !evidence_ids.insert(excerpt.evidence_id.clone())
                || !resources.insert(resource.clone())
                || previous_resource
                    .as_ref()
                    .is_some_and(|previous| previous >= &resource)
            {
                return Err(ContextError::InvalidBundle);
            }
            previous_resource = Some(resource);
            if excerpt.text.len() != excerpt.receipt.included_bytes
                || excerpt.receipt.included_bytes > excerpt.receipt.original_bytes
                || excerpt.receipt.truncated
                    != (excerpt.receipt.included_bytes < excerpt.receipt.original_bytes)
                || !is_sha256_hex(&excerpt.receipt.content_sha256)
            {
                return Err(ContextError::InvalidBundle);
            }
            if !excerpt.receipt.truncated
                && crate::transport::sha256_hex(excerpt.text.as_bytes())
                    != excerpt.receipt.content_sha256
            {
                return Err(ContextError::InvalidBundle);
            }
            let expected_evidence = EvidenceId::from_digest(&evidence_digest(
                &excerpt.receipt.server_id,
                &excerpt.receipt.resource_uri,
                &excerpt.receipt.content_sha256,
            ));
            if excerpt.evidence_id != expected_evidence {
                return Err(ContextError::InvalidBundle);
            }
            estimated_tokens = estimated_tokens.saturating_add(estimate_tokens(excerpt.text.len()));
        }
        if !is_sha256_hex(&self.digest)
            || !is_sha256_hex(&self.integrity_digest)
            || estimated_tokens != self.estimated_tokens
            || bundle_digest(&self.excerpts) != self.digest
            || bundle_integrity_digest(&self.excerpts, &self.digest, self.estimated_tokens)
                != self.integrity_digest
        {
            return Err(ContextError::InvalidBundle);
        }
        Ok(())
    }
}

impl fmt::Debug for ContextBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContextBundle")
            .field("excerpt_count", &self.excerpts.len())
            .field("digest", &self.digest)
            .field("integrity_digest", &self.integrity_digest)
            .field("estimated_tokens", &self.estimated_tokens)
            .finish()
    }
}

pub type ContextResult<T> = Result<T, ContextError>;

/// Configuration errors contain no submitted values so secrets cannot echo through UI/logs.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ContextGrantError {
    #[error("MCP server id must be 1..=128 lowercase ASCII identifier characters")]
    InvalidServerId,
    #[error("MCP resource URI must be 1..=4096 bytes and contain no NUL")]
    InvalidResourceUri,
    #[error("MCP resource URI must not contain user information, a query, or a fragment")]
    UnsafeResourceUri,
    #[error("resource was selected more than once")]
    DuplicateSelection,
}

fn redacted_fingerprint(value: &str) -> String {
    let digest = crate::transport::sha256_hex(value.as_bytes());
    format!("<redacted:{}>", &digest[..12])
}

/// Runtime failures identify a server and opaque resource hash, never resource text or credentials.
#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error("invalid zero-valued MCP context budget")]
    InvalidBudget,
    #[error("MCP server {0} is not registered")]
    UnknownServer(ServerId),
    #[error("MCP server {0} is already registered")]
    DuplicateServer(ServerId),
    #[error("MCP grant exceeds the {dimension} budget ({actual} > {limit})")]
    BudgetExceeded {
        dimension: &'static str,
        actual: usize,
        limit: usize,
    },
    #[error("MCP server {0} does not advertise resource support")]
    ResourcesUnsupported(ServerId),
    #[error("selected MCP resource {resource_hash} is not advertised by server {server}")]
    UnknownResource {
        server: ServerId,
        resource_hash: String,
    },
    #[error("MCP resource {resource_hash} returned binary content")]
    BinaryResource { resource_hash: String },
    #[error("MCP resource {resource_hash} declared unsupported content type")]
    UnsupportedContentType { resource_hash: String },
    #[error("MCP resource {resource_hash} changed identity while being read")]
    ResourceChanged { resource_hash: String },
    #[error("MCP resource catalog exceeded its pagination budget")]
    CatalogPaginationExceeded,
    #[error("MCP context operation was cancelled")]
    Cancelled,
    #[error("MCP server {0} exceeded its operation timeout")]
    TimedOut(ServerId),
    #[error("MCP transport for server {0} failed")]
    Transport(ServerId),
    #[error("MCP connection configuration is invalid")]
    InvalidConnection,
    #[error("MCP remote endpoint must use HTTPS except for a loopback host")]
    InsecureEndpoint,
    #[error("MCP remote endpoint must not contain credentials, query, or fragment")]
    UnsafeEndpoint,
    #[error("MCP resource requested interactive input, which the read-only client rejects")]
    InteractiveRequestRejected,
    #[error("persisted MCP evidence bundle failed integrity validation")]
    InvalidBundle,
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

pub(crate) const fn estimate_tokens(bytes: usize) -> usize {
    bytes.saturating_add(3) / 4
}

pub(crate) fn evidence_digest(
    server: &ServerId,
    uri: &ResourceUri,
    content_digest: &str,
) -> String {
    let material = format!(
        "sotto-mcp-evidence-v1:{}:{}:{}:{}:{}:{}",
        server.as_str().len(),
        server.as_str(),
        uri.as_str().len(),
        uri.as_str(),
        content_digest.len(),
        content_digest
    );
    crate::transport::sha256_hex(material.as_bytes())
}

pub(crate) fn bundle_digest(excerpts: &[ContextExcerpt]) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(b"sotto-mcp-bundle-v1");
    for excerpt in excerpts {
        update_length_prefixed(&mut hasher, excerpt.evidence_id.as_str().as_bytes());
        update_length_prefixed(&mut hasher, excerpt.text.as_bytes());
        hasher.update([u8::from(excerpt.receipt.truncated)]);
    }
    hex::encode(hasher.finalize())
}

fn bundle_integrity_digest(
    excerpts: &[ContextExcerpt],
    digest: &str,
    estimated_tokens: usize,
) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(b"sotto-mcp-bundle-integrity-v1");
    update_length_prefixed(&mut hasher, digest.as_bytes());
    hasher.update(estimated_tokens.to_le_bytes());
    for excerpt in excerpts {
        update_length_prefixed(&mut hasher, excerpt.evidence_id.as_str().as_bytes());
        update_length_prefixed(&mut hasher, excerpt.title.as_bytes());
        update_length_prefixed(&mut hasher, excerpt.text.as_bytes());
        update_length_prefixed(&mut hasher, excerpt.receipt.server_id.as_str().as_bytes());
        update_length_prefixed(
            &mut hasher,
            excerpt.receipt.resource_uri.as_str().as_bytes(),
        );
        update_length_prefixed(&mut hasher, excerpt.receipt.content_sha256.as_bytes());
        hasher.update(excerpt.receipt.retrieved_at_unix_ms.to_le_bytes());
        hasher.update(excerpt.receipt.original_bytes.to_le_bytes());
        hasher.update(excerpt.receipt.included_bytes.to_le_bytes());
        hasher.update([u8::from(excerpt.receipt.truncated)]);
        match &excerpt.receipt.last_modified {
            Some(last_modified) => {
                hasher.update([1]);
                update_length_prefixed(&mut hasher, last_modified.as_bytes());
            }
            None => hasher.update([0]),
        }
    }
    hex::encode(hasher.finalize())
}

fn update_length_prefixed(hasher: &mut sha2::Sha256, value: &[u8]) {
    use sha2::Digest;

    hasher.update(value.len().to_le_bytes());
    hasher.update(value);
}
