use std::{fmt, future::Future, pin::Pin};

use sha2::{Digest, Sha256};

use crate::{
    ContextCancellation, ContextResult, ResourceDescriptor, ResourceUri, ServerDescriptor,
};

pub type BoxContextFuture<'a, T> = Pin<Box<dyn Future<Output = ContextResult<T>> + Send + 'a>>;

/// Complete bounded catalog returned by one explicit discovery operation.
#[derive(Clone, Eq, PartialEq)]
pub struct ResourceCatalog {
    pub server: ServerDescriptor,
    pub resources_supported: bool,
    pub resources: Vec<ResourceDescriptor>,
    pub pages: usize,
}

impl fmt::Debug for ResourceCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResourceCatalog")
            .field("server", &self.server)
            .field("resources_supported", &self.resources_supported)
            .field("resource_count", &self.resources.len())
            .field("pages", &self.pages)
            .finish()
    }
}

/// One resource response before the broker applies content policy and prompt budgets.
#[derive(Clone, Eq, PartialEq)]
pub enum RawResourceContent {
    Text {
        uri: ResourceUri,
        mime_type: Option<String>,
        text: String,
    },
    Blob {
        uri: ResourceUri,
        mime_type: Option<String>,
        encoded_bytes: usize,
    },
}

impl fmt::Debug for RawResourceContent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text {
                uri,
                mime_type,
                text,
            } => formatter
                .debug_struct("Text")
                .field("uri", uri)
                .field("mime_type", &mime_type.as_ref().map(|_| "<redacted>"))
                .field("text_bytes", &text.len())
                .finish(),
            Self::Blob {
                uri,
                mime_type,
                encoded_bytes,
            } => formatter
                .debug_struct("Blob")
                .field("uri", uri)
                .field("mime_type", &mime_type.as_ref().map(|_| "<redacted>"))
                .field("encoded_bytes", encoded_bytes)
                .finish(),
        }
    }
}

/// Internal transport-neutral surface used by the broker and credential-free fakes.
///
/// Implementations may only discover and read resources. There is intentionally no
/// method capable of listing or invoking tools, prompts, sampling, roots, or subscriptions.
pub trait ResourceTransport: Send + Sync {
    fn descriptor(&self) -> &ServerDescriptor;

    /// Credential-free connection identity included in immutable run fingerprints.
    fn connection_identity(&self) -> Option<&str> {
        None
    }

    fn list_resources(
        &self,
        max_pages: usize,
        max_resources: usize,
        max_transport_message_bytes: usize,
        cancellation: ContextCancellation,
    ) -> BoxContextFuture<'_, ResourceCatalog>;

    fn read_resource<'a>(
        &'a self,
        uri: &'a ResourceUri,
        max_transport_message_bytes: usize,
        cancellation: ContextCancellation,
    ) -> BoxContextFuture<'a, Vec<RawResourceContent>>;
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}
