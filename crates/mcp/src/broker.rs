use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};

use crate::{
    BoxContextFuture, ContextBudget, ContextBundle, ContextCancellation, ContextError,
    ContextExcerpt, ContextResult, EvidenceId, GrantRunFingerprint, RawResourceContent,
    ResourceCatalog, ResourceDescriptor, ResourceTransport, ResourceUri, ServerId,
    SessionContextGrant, SourceReceipt,
    transport::sha256_hex,
    types::{bundle_digest, estimate_tokens, evidence_digest},
};

/// Consumer-facing context resolver. Its only input is an explicit resource grant.
pub trait ContextSource: Send + Sync {
    fn resolve<'a>(
        &'a self,
        grant: &'a SessionContextGrant,
        budget: ContextBudget,
        cancellation: ContextCancellation,
    ) -> BoxContextFuture<'a, ContextBundle>;
}

/// Registry and policy boundary for application-controlled MCP resource evidence.
#[derive(Default)]
pub struct McpBroker {
    transports: BTreeMap<ServerId, Arc<dyn ResourceTransport>>,
}

impl McpBroker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, transport: Arc<dyn ResourceTransport>) -> ContextResult<()> {
        let id = transport.descriptor().id.clone();
        if self.transports.contains_key(&id) {
            return Err(ContextError::DuplicateServer(id));
        }
        self.transports.insert(id, transport);
        Ok(())
    }

    #[must_use]
    pub fn server_descriptors(&self) -> Vec<crate::ServerDescriptor> {
        self.transports
            .values()
            .map(|transport| transport.descriptor().clone())
            .collect()
    }

    /// Freezes the exact credential-free inputs that must partition a future reasoning run.
    pub fn run_fingerprint(
        &self,
        grant: &SessionContextGrant,
    ) -> ContextResult<GrantRunFingerprint> {
        let mut hasher = Sha256::new();
        hasher.update(b"sotto-mcp-grant-v1\0");
        for (server_id, resources) in grant.selections() {
            let transport = self
                .transports
                .get(server_id)
                .ok_or_else(|| ContextError::UnknownServer(server_id.clone()))?;
            let endpoint = transport
                .connection_identity()
                .ok_or(ContextError::InvalidConnection)?;
            hash_field(&mut hasher, server_id.as_str());
            hash_field(&mut hasher, endpoint);
            for uri in resources {
                hash_field(&mut hasher, uri.as_str());
            }
            hash_field(
                &mut hasher,
                match grant.query_disclosure(server_id) {
                    crate::MeetingQueryDisclosure::None => "query:none",
                    crate::MeetingQueryDisclosure::Redacted => "query:redacted",
                },
            );
        }
        for server_id in grant.query_disclosures() {
            if !grant
                .selections()
                .any(|(selected, _)| selected == server_id)
            {
                let transport = self
                    .transports
                    .get(server_id)
                    .ok_or_else(|| ContextError::UnknownServer(server_id.clone()))?;
                let endpoint = transport
                    .connection_identity()
                    .ok_or(ContextError::InvalidConnection)?;
                hash_field(&mut hasher, server_id.as_str());
                hash_field(&mut hasher, endpoint);
                hash_field(&mut hasher, "query:redacted");
            }
        }
        Ok(GrantRunFingerprint::from_digest(hex::encode(
            hasher.finalize(),
        )))
    }

    /// Lists one server only after its id is supplied explicitly by the caller.
    pub async fn discover_resources(
        &self,
        server_id: &ServerId,
        budget: ContextBudget,
        cancellation: ContextCancellation,
    ) -> ContextResult<ResourceCatalog> {
        let budget = budget.validate()?;
        let transport = self
            .transports
            .get(server_id)
            .ok_or_else(|| ContextError::UnknownServer(server_id.clone()))?;
        let catalog = call_with_policy(
            server_id,
            budget,
            cancellation.clone(),
            transport.list_resources(
                budget.max_catalog_pages,
                budget.max_catalog_resources,
                budget.max_transport_message_bytes,
                cancellation,
            ),
        )
        .await?;
        if &catalog.server.id != server_id {
            return Err(ContextError::Transport(server_id.clone()));
        }
        Ok(catalog)
    }

    async fn resolve_inner(
        &self,
        grant: &SessionContextGrant,
        budget: ContextBudget,
        cancellation: ContextCancellation,
    ) -> ContextResult<ContextBundle> {
        let budget = budget.validate()?;
        if grant.is_empty() {
            return Ok(ContextBundle::empty());
        }
        let server_count = grant.selections().count();
        enforce_budget("servers", server_count, budget.max_servers)?;
        let selected_count = grant
            .selections()
            .map(|(_, resources)| resources.len())
            .sum::<usize>();
        enforce_budget(
            "selected resources",
            selected_count,
            budget.max_selected_resources,
        )?;

        let mut excerpts = Vec::with_capacity(selected_count);
        let mut remaining_bundle_bytes = budget.max_bundle_bytes;
        let mut remaining_tokens = budget.max_estimated_tokens;

        for (server_id, selected) in grant.selections() {
            if cancellation.is_cancelled() {
                return Err(ContextError::Cancelled);
            }
            let transport = self
                .transports
                .get(server_id)
                .ok_or_else(|| ContextError::UnknownServer(server_id.clone()))?;
            let catalog = call_with_policy(
                server_id,
                budget,
                cancellation.clone(),
                transport.list_resources(
                    budget.max_catalog_pages,
                    budget.max_catalog_resources,
                    budget.max_transport_message_bytes,
                    cancellation.clone(),
                ),
            )
            .await?;
            if catalog.server.id != *server_id {
                return Err(ContextError::Transport(server_id.clone()));
            }
            if !catalog.resources_supported {
                return Err(ContextError::ResourcesUnsupported(server_id.clone()));
            }
            let mut advertised = HashMap::new();
            for resource in catalog.resources {
                if resource.server_id != *server_id
                    || advertised.insert(resource.uri.clone(), resource).is_some()
                {
                    return Err(ContextError::Transport(server_id.clone()));
                }
            }

            for uri in selected {
                let resource_hash = sha256_hex(uri.as_str().as_bytes());
                let descriptor =
                    advertised
                        .get(uri)
                        .ok_or_else(|| ContextError::UnknownResource {
                            server: server_id.clone(),
                            resource_hash: resource_hash.clone(),
                        })?;
                if descriptor
                    .size
                    .is_some_and(|size| size > budget.max_transport_message_bytes as u64)
                {
                    return Err(ContextError::BudgetExceeded {
                        dimension: "declared transport bytes",
                        actual: usize::try_from(descriptor.size.unwrap_or(u64::MAX))
                            .unwrap_or(usize::MAX),
                        limit: budget.max_transport_message_bytes,
                    });
                }
                let contents = call_with_policy(
                    server_id,
                    budget,
                    cancellation.clone(),
                    transport.read_resource(
                        uri,
                        budget.max_transport_message_bytes,
                        cancellation.clone(),
                    ),
                )
                .await?;
                let normalized = normalize_resource(uri, descriptor, contents, &resource_hash)?;
                let original_bytes = normalized.len();
                enforce_budget(
                    "transport resource bytes",
                    original_bytes,
                    budget.max_transport_message_bytes,
                )?;
                let allowed_by_tokens = remaining_tokens.saturating_mul(4);
                let allowed = budget
                    .max_resource_bytes
                    .min(remaining_bundle_bytes)
                    .min(allowed_by_tokens);
                let (text, truncated) = truncate_utf8(&normalized, allowed);
                let included_bytes = text.len();
                let estimated_tokens = estimate_tokens(included_bytes);
                remaining_bundle_bytes = remaining_bundle_bytes.saturating_sub(included_bytes);
                remaining_tokens = remaining_tokens.saturating_sub(estimated_tokens);
                let content_sha256 = sha256_hex(normalized.as_bytes());
                let evidence_digest = evidence_digest(server_id, uri, &content_sha256);
                let receipt = SourceReceipt {
                    server_id: server_id.clone(),
                    resource_uri: uri.clone(),
                    content_sha256,
                    retrieved_at_unix_ms: now_unix_ms()?,
                    original_bytes,
                    included_bytes,
                    truncated,
                    last_modified: descriptor.last_modified.clone(),
                };
                excerpts.push(ContextExcerpt {
                    evidence_id: EvidenceId::from_digest(&evidence_digest),
                    title: descriptor
                        .title
                        .clone()
                        .unwrap_or_else(|| descriptor.name.clone()),
                    text,
                    receipt,
                });
            }
        }
        let estimated_tokens = excerpts
            .iter()
            .map(|excerpt| estimate_tokens(excerpt.text.len()))
            .sum();
        let digest = bundle_digest(&excerpts);
        Ok(ContextBundle::new(excerpts, digest, estimated_tokens))
    }
}

fn hash_field(hasher: &mut Sha256, value: &str) {
    hasher.update(value.len().to_le_bytes());
    hasher.update(value.as_bytes());
}

impl ContextSource for McpBroker {
    fn resolve<'a>(
        &'a self,
        grant: &'a SessionContextGrant,
        budget: ContextBudget,
        cancellation: ContextCancellation,
    ) -> BoxContextFuture<'a, ContextBundle> {
        Box::pin(self.resolve_inner(grant, budget, cancellation))
    }
}

async fn call_with_policy<T>(
    server_id: &ServerId,
    budget: ContextBudget,
    cancellation: ContextCancellation,
    operation: BoxContextFuture<'_, T>,
) -> ContextResult<T> {
    tokio::select! {
        () = cancellation.cancelled() => Err(ContextError::Cancelled),
        result = tokio::time::timeout(budget.call_timeout, operation) => {
            result.map_err(|_| ContextError::TimedOut(server_id.clone()))?
        }
    }
}

fn normalize_resource(
    selected_uri: &ResourceUri,
    descriptor: &ResourceDescriptor,
    contents: Vec<RawResourceContent>,
    resource_hash: &str,
) -> ContextResult<String> {
    let mut text = String::new();
    for content in contents {
        match content {
            RawResourceContent::Text {
                uri,
                mime_type,
                text: part,
            } => {
                if &uri != selected_uri {
                    return Err(ContextError::ResourceChanged {
                        resource_hash: resource_hash.to_owned(),
                    });
                }
                let mime = mime_type.as_deref().or(descriptor.mime_type.as_deref());
                if mime.is_some_and(|value| !is_text_content_type(value)) {
                    return Err(ContextError::UnsupportedContentType {
                        resource_hash: resource_hash.to_owned(),
                    });
                }
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&part);
            }
            RawResourceContent::Blob { .. } => {
                return Err(ContextError::BinaryResource {
                    resource_hash: resource_hash.to_owned(),
                });
            }
        }
    }
    Ok(text)
}

fn is_text_content_type(value: &str) -> bool {
    let base = value
        .split_once(';')
        .map_or(value, |(content_type, _)| content_type)
        .trim()
        .to_ascii_lowercase();
    base.starts_with("text/")
        || matches!(
            base.as_str(),
            "application/json"
                | "application/ld+json"
                | "application/xml"
                | "application/yaml"
                | "application/x-yaml"
                | "application/toml"
        )
        || base.ends_with("+json")
        || base.ends_with("+xml")
}

fn truncate_utf8(input: &str, max_bytes: usize) -> (String, bool) {
    if input.len() <= max_bytes {
        return (input.to_owned(), false);
    }
    let mut boundary = max_bytes.min(input.len());
    while boundary > 0 && !input.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    (input[..boundary].to_owned(), true)
}

fn now_unix_ms() -> ContextResult<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ContextError::InvalidConnection)?
        .as_millis();
    u64::try_from(millis).map_err(|_| ContextError::InvalidConnection)
}

fn enforce_budget(dimension: &'static str, actual: usize, limit: usize) -> ContextResult<()> {
    if actual > limit {
        Err(ContextError::BudgetExceeded {
            dimension,
            actual,
            limit,
        })
    } else {
        Ok(())
    }
}
