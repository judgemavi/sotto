use std::fmt;

use rmcp::{
    ClientLifecycleMode, ClientServiceExt,
    model::{
        ClientInfo, PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams,
        ReadResourceResponse, ResourceContents,
    },
    service::{RoleClient, RunningService},
    transport::streamable_http_client::{
        StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
    },
};
use secrecy::{ExposeSecret, SecretString};
use url::Url;

use crate::{
    BoxContextFuture, ContextCancellation, ContextError, RawResourceContent, ResourceCatalog,
    ResourceDescriptor, ResourceTransport, ResourceUri, ServerDescriptor, TransportKind,
    http_client::BoundedHttpClient,
};

/// Validated credential-free Streamable HTTP endpoint.
#[derive(Clone, Eq, PartialEq)]
pub struct HttpEndpoint(Url);

impl HttpEndpoint {
    pub fn new(value: &str) -> Result<Self, ContextError> {
        let url = Url::parse(value).map_err(|_| ContextError::InvalidConnection)?;
        let host = url.host_str().ok_or(ContextError::InvalidConnection)?;
        let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
            return Err(ContextError::InsecureEndpoint);
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ContextError::UnsafeEndpoint);
        }
        Ok(Self(url))
    }

    /// Product configuration accepts only remote HTTPS. Loopback HTTP remains a test seam.
    pub fn new_remote(value: &str) -> Result<Self, ContextError> {
        let endpoint = Self::new(value)?;
        if endpoint.0.scheme() != "https" {
            return Err(ContextError::InsecureEndpoint);
        }
        Ok(endpoint)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    #[must_use]
    pub fn host(&self) -> &str {
        self.0.host_str().unwrap_or("invalid-host")
    }
}

/// Runtime-only bearer value. It is never serializable and its diagnostics are always redacted.
#[derive(Clone)]
pub struct BearerCredential(SecretString);

impl BearerCredential {
    #[must_use]
    pub fn new(value: SecretString) -> Self {
        Self(value)
    }
}

impl fmt::Debug for BearerCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerCredential(<redacted>)")
    }
}

impl fmt::Debug for HttpEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("HttpEndpoint")
            .field(&format_args!("<redacted:{}>", self.0.scheme()))
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub enum ServerConnection {
    StreamableHttp(HttpEndpoint),
}

impl fmt::Debug for ServerConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StreamableHttp(endpoint) => formatter
                .debug_tuple("StreamableHttp")
                .field(endpoint)
                .finish(),
        }
    }
}

impl ServerConnection {
    const fn transport_kind(&self) -> TransportKind {
        match self {
            Self::StreamableHttp(_) => TransportKind::StreamableHttp,
        }
    }
}

/// Official-SDK transport hidden behind Sotto's resources-only trait.
pub struct RmcpResourceTransport {
    descriptor: ServerDescriptor,
    connection: ServerConnection,
    credential: Option<BearerCredential>,
}

impl RmcpResourceTransport {
    pub fn new(
        descriptor: ServerDescriptor,
        connection: ServerConnection,
    ) -> Result<Self, ContextError> {
        if descriptor.transport != connection.transport_kind() {
            return Err(ContextError::InvalidConnection);
        }
        Ok(Self {
            descriptor,
            connection,
            credential: None,
        })
    }

    pub fn new_authenticated(
        descriptor: ServerDescriptor,
        connection: ServerConnection,
        credential: BearerCredential,
    ) -> Result<Self, ContextError> {
        let mut transport = Self::new(descriptor, connection)?;
        transport.credential = Some(credential);
        Ok(transport)
    }

    async fn start(
        &self,
        max_transport_message_bytes: usize,
    ) -> Result<RunningService<RoleClient, ClientInfo>, ContextError> {
        let lifecycle = ClientLifecycleMode::Auto {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            legacy_version: Some(ProtocolVersion::V_2025_11_25),
        };
        let ServerConnection::StreamableHttp(endpoint) = &self.connection;
        let mut config = StreamableHttpClientTransportConfig::with_uri(endpoint.as_str())
            .max_sse_event_size(max_transport_message_bytes);
        if let Some(credential) = &self.credential {
            config = config.auth_header(credential.0.expose_secret());
        }
        let client = BoundedHttpClient::new()
            .map_err(|_| ContextError::Transport(self.descriptor.id.clone()))?;
        let transport = StreamableHttpClientTransport::with_client(client, config);
        ClientInfo::default()
            .serve_with_lifecycle(transport, lifecycle)
            .await
            .map_err(|_| ContextError::Transport(self.descriptor.id.clone()))
    }

    async fn list_inner(
        &self,
        max_pages: usize,
        max_resources: usize,
        max_transport_message_bytes: usize,
        cancellation: ContextCancellation,
    ) -> Result<ResourceCatalog, ContextError> {
        if cancellation.is_cancelled() {
            return Err(ContextError::Cancelled);
        }
        let service = self.start(max_transport_message_bytes).await?;
        let supported = service
            .peer_info()
            .is_some_and(|info| info.capabilities.resources.is_some());
        if !supported {
            let _ = service.cancel().await;
            return Ok(ResourceCatalog {
                server: self.descriptor.clone(),
                resources_supported: false,
                resources: Vec::new(),
                pages: 0,
            });
        }
        let mut cursor = None;
        let mut resources = Vec::new();
        let mut pages = 0_usize;
        loop {
            if cancellation.is_cancelled() {
                let _ = service.cancel().await;
                return Err(ContextError::Cancelled);
            }
            if pages == max_pages {
                let _ = service.cancel().await;
                return Err(ContextError::CatalogPaginationExceeded);
            }
            let params = cursor
                .take()
                .map(|value| PaginatedRequestParams::default().with_cursor(Some(value)));
            let page = service
                .list_resources(params)
                .await
                .map_err(|_| ContextError::Transport(self.descriptor.id.clone()))?;
            pages = pages.saturating_add(1);
            for resource in page.resources {
                if resources.len() == max_resources {
                    let _ = service.cancel().await;
                    return Err(ContextError::BudgetExceeded {
                        dimension: "catalog resources",
                        actual: resources.len().saturating_add(1),
                        limit: max_resources,
                    });
                }
                let uri = ResourceUri::new(resource.uri)
                    .map_err(|_| ContextError::Transport(self.descriptor.id.clone()))?;
                resources.push(ResourceDescriptor {
                    server_id: self.descriptor.id.clone(),
                    uri,
                    name: resource.name,
                    title: resource.title,
                    description: resource.description,
                    mime_type: resource.mime_type,
                    size: resource.size,
                    last_modified: resource.annotations.and_then(|value| value.last_modified),
                });
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        let _ = service.cancel().await;
        resources.sort_by(|left, right| left.uri.cmp(&right.uri));
        resources.dedup_by(|left, right| left.uri == right.uri);
        Ok(ResourceCatalog {
            server: self.descriptor.clone(),
            resources_supported: true,
            resources,
            pages,
        })
    }

    async fn read_inner(
        &self,
        uri: &ResourceUri,
        max_transport_message_bytes: usize,
        cancellation: ContextCancellation,
    ) -> Result<Vec<RawResourceContent>, ContextError> {
        if cancellation.is_cancelled() {
            return Err(ContextError::Cancelled);
        }
        let service = self.start(max_transport_message_bytes).await?;
        let response = service
            .read_resource_once(ReadResourceRequestParams::new(uri.as_str()))
            .await
            .map_err(|_| ContextError::Transport(self.descriptor.id.clone()))?;
        let result = match response {
            ReadResourceResponse::Complete(result) => result,
            ReadResourceResponse::InputRequired(_) => {
                let _ = service.cancel().await;
                return Err(ContextError::InteractiveRequestRejected);
            }
            _ => {
                let _ = service.cancel().await;
                return Err(ContextError::Transport(self.descriptor.id.clone()));
            }
        };
        let mut normalized = Vec::with_capacity(result.contents.len());
        for content in result.contents {
            match content {
                ResourceContents::TextResourceContents {
                    uri,
                    mime_type,
                    text,
                    ..
                } => normalized.push(RawResourceContent::Text {
                    uri: ResourceUri::new(uri)
                        .map_err(|_| ContextError::Transport(self.descriptor.id.clone()))?,
                    mime_type,
                    text,
                }),
                ResourceContents::BlobResourceContents {
                    uri,
                    mime_type,
                    blob,
                    ..
                } => normalized.push(RawResourceContent::Blob {
                    uri: ResourceUri::new(uri)
                        .map_err(|_| ContextError::Transport(self.descriptor.id.clone()))?,
                    mime_type,
                    encoded_bytes: blob.len(),
                }),
                _ => {
                    let _ = service.cancel().await;
                    return Err(ContextError::Transport(self.descriptor.id.clone()));
                }
            }
        }
        let _ = service.cancel().await;
        Ok(normalized)
    }
}

impl ResourceTransport for RmcpResourceTransport {
    fn descriptor(&self) -> &ServerDescriptor {
        &self.descriptor
    }

    fn connection_identity(&self) -> Option<&str> {
        match &self.connection {
            ServerConnection::StreamableHttp(endpoint) => Some(endpoint.as_str()),
        }
    }

    fn list_resources(
        &self,
        max_pages: usize,
        max_resources: usize,
        max_transport_message_bytes: usize,
        cancellation: ContextCancellation,
    ) -> BoxContextFuture<'_, ResourceCatalog> {
        Box::pin(self.list_inner(
            max_pages,
            max_resources,
            max_transport_message_bytes,
            cancellation,
        ))
    }

    fn read_resource<'a>(
        &'a self,
        uri: &'a ResourceUri,
        max_transport_message_bytes: usize,
        cancellation: ContextCancellation,
    ) -> BoxContextFuture<'a, Vec<RawResourceContent>> {
        Box::pin(self.read_inner(uri, max_transport_message_bytes, cancellation))
    }
}
