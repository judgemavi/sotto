//! Provider-owned advanced dispatch seam for screen-authorized image evidence.

use std::sync::Arc;

use screen::AuthorizedReasoningImage;
use sotto_core::{
    BoxFuture, BoxStream, CancellationToken, CompletionProvider, CompletionRequest, Delta,
    ProviderError, ReasoningRequest,
};

use crate::{BackendCapability, backend::ObservedRequestNormalization};

/// Advanced reasoning transport without moving screen authority into `core`.
///
/// Implementations must reject an image they cannot map. The default does so before delegating
/// any request, which keeps text-only connectors and Codex honest without transport I/O.
pub trait ReasoningProvider: CompletionProvider {
    /// Drains request-control downgrades produced since the previous call.
    ///
    /// Text-only and directly constructed providers return no observations. Resolved backend
    /// wrappers override this so reasoning results can carry their own normalization evidence.
    fn take_request_normalizations(&self) -> Vec<ObservedRequestNormalization> {
        Vec::new()
    }

    /// Reports advanced dispatch shapes implemented by this exact trait object.
    ///
    /// Descriptor metadata is checked against this method before registry mutation, so merely
    /// choosing the advanced registration entry point cannot overclaim the default adapters.
    fn supports_advanced_capability(&self, _capability: BackendCapability) -> bool {
        false
    }

    fn stream_advanced_reasoning(
        &self,
        request: ReasoningRequest,
        image: Option<AuthorizedReasoningImage>,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
    {
        Box::pin(async move {
            if image.is_some() {
                return Err(ProviderError::InvalidRequest(
                    "provider does not support authorized screen image input".to_owned(),
                ));
            }
            self.stream_reasoning(request, cancellation).await
        })
    }
}

/// Source-compatible adapter for existing text-only `CompletionProvider` call sites.
pub struct TextReasoningProvider {
    inner: Arc<dyn CompletionProvider>,
}

impl CompletionProvider for TextReasoningProvider {
    fn stream(
        &self,
        request: CompletionRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
    {
        self.inner.stream(request, cancellation)
    }

    fn stream_reasoning(
        &self,
        request: ReasoningRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
    {
        self.inner.stream_reasoning(request, cancellation)
    }

    fn model_id(&self) -> &str {
        self.inner.model_id()
    }
}

impl ReasoningProvider for TextReasoningProvider {}

#[must_use]
pub fn text_reasoning_provider(
    provider: Arc<dyn CompletionProvider>,
) -> Arc<dyn ReasoningProvider> {
    Arc::new(TextReasoningProvider { inner: provider })
}
