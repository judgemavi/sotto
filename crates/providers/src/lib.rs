//! BYOK adapters for streaming provider-neutral completions.
//!
//! `CompletionRequest::model` is authoritative for each call. [`Provider::model_id`]
//! reports only the configured default used by settings and [`Registry`]. A cancelled
//! call with no emitted text yields `ProviderError::Cancelled`; after text has been
//! emitted it ends with a final `StopReason::Aborted` delta (usage is preserved when
//! the upstream supplied it). A single `cache_boundary` is accepted; Anthropic maps
//! it to `cache_control`, while other providers deliberately ignore it.

#![deny(warnings)]

pub mod inspection;
mod parse;
mod reasoning;

pub mod anthropic;
pub mod backend;
pub mod codex;
pub mod google;
pub mod ollama;
pub mod openai;
pub mod openrouter;

use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use sotto_core::{
    BoxFuture, BoxStream, CancellationToken, CompletionProvider, CompletionRequest, Delta,
    ProviderError,
};

pub use backend::{
    AuthKind, AuthStatus, BackendCapabilities, BackendCapability, BackendContractError,
    BackendDescriptor, BackendFingerprint, BackendId, CODEX_CLI_BACKEND_ID,
    OPENAI_RESPONSES_BACKEND_ID, Registry, RegistryError, ResolvedBackend, Role,
};
pub use parse::{ProviderKind, Transport};
pub use reasoning::{ReasoningProvider, TextReasoningProvider, text_reasoning_provider};

/// A configured HTTP adapter. Secret material is intentionally absent from `Debug`.
pub struct Provider {
    kind: ProviderKind,
    default_model: String,
    api_key: Option<SecretString>,
    transport: Transport,
}

impl fmt::Debug for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Provider")
            .field("kind", &self.kind)
            .field("default_model", &self.default_model)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .finish_non_exhaustive()
    }
}

impl Provider {
    #[must_use]
    pub fn new(
        kind: ProviderKind,
        default_model: impl Into<String>,
        key: Option<SecretString>,
    ) -> Self {
        Self {
            transport: Transport::new(kind),
            kind,
            default_model: default_model.into(),
            api_key: key,
        }
    }

    /// Overrides the endpoint, primarily for deterministic mock-server tests.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.transport.base_url = base_url.into();
        self
    }

    /// Starts a completion and returns a handle that can abort it explicitly.
    pub fn start(&self, req: CompletionRequest) -> Result<CompletionCall, ProviderError> {
        let cancellation = CancellationToken::new();
        self.start_with_cancellation(req, cancellation)
    }

    fn start_with_cancellation(
        &self,
        req: CompletionRequest,
        cancellation: CancellationToken,
    ) -> Result<CompletionCall, ProviderError> {
        validate(&req)?;
        let transport = self.transport.clone();
        let key = self.api_key.clone();
        let stream_cancellation = cancellation.clone();
        let future = async move { Ok(transport.stream(req, key, stream_cancellation)) };
        Ok(CompletionCall {
            cancellation,
            future: Box::pin(future),
        })
    }
}

impl CompletionProvider for Provider {
    fn stream(
        &self,
        req: CompletionRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
    {
        Box::pin(async move {
            self.start_with_cancellation(req, cancellation)?
                .future
                .await
        })
    }

    fn model_id(&self) -> &str {
        &self.default_model
    }
}

pub struct CompletionCall {
    cancellation: CancellationToken,
    future:
        BoxFuture<'static, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>,
}

impl CompletionCall {
    pub fn abort(&self) {
        self.cancellation.cancel();
    }

    #[must_use]
    pub fn abort_handle(&self) -> CancellationHandle {
        CancellationHandle(self.cancellation.clone())
    }

    pub async fn stream(
        self,
    ) -> Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError> {
        self.future.await
    }
}

#[derive(Clone)]
pub struct CancellationHandle(CancellationToken);

impl CancellationHandle {
    pub fn abort(&self) {
        self.0.cancel();
    }
}

fn validate(req: &CompletionRequest) -> Result<(), ProviderError> {
    let boundaries = req
        .messages
        .iter()
        .filter(|message| message.cache_boundary)
        .count();
    if boundaries > 1 {
        return Err(ProviderError::InvalidRequest(
            "multiple cache boundaries".to_owned(),
        ));
    }
    Ok(())
}

fn entry(kind: ProviderKind) -> Result<keyring::Entry, ProviderError> {
    keyring::Entry::new("dev.sotto.llm", kind.key_name())
        .map_err(|error| ProviderError::CredentialStore(error.to_string()))
}

pub fn store_key(kind: ProviderKind, key: &SecretString) -> Result<(), ProviderError> {
    entry(kind)?
        .set_password(key.expose_secret())
        .map_err(|error| ProviderError::CredentialStore(error.to_string()))
}

pub fn load_key(kind: ProviderKind) -> Result<Option<SecretString>, ProviderError> {
    match entry(kind)?.get_password() {
        Ok(key) => Ok(Some(SecretString::from(key))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(ProviderError::CredentialStore(error.to_string())),
    }
}

pub fn delete_key(kind: ProviderKind) -> Result<(), ProviderError> {
    match entry(kind)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(ProviderError::CredentialStore(error.to_string())),
    }
}

pub fn has_key(kind: ProviderKind) -> Result<bool, ProviderError> {
    load_key(kind).map(|key| key.is_some())
}

#[cfg(test)]
mod tests {
    use super::{Provider, ProviderKind, validate};
    use futures_util::StreamExt;
    use secrecy::SecretString;
    use sotto_core::{
        CancellationToken, CompletionMessage, CompletionProvider, CompletionRequest, MessageRole,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn request() -> CompletionRequest {
        CompletionRequest {
            model: "requested-model".to_owned(),
            system: None,
            messages: vec![],
            max_tokens: None,
            temperature: None,
            stop: vec![],
        }
    }

    #[test]
    fn provider_debug_redacts_key() {
        let provider = Provider::new(
            ProviderKind::Anthropic,
            "default",
            Some(SecretString::from("super-secret".to_owned())),
        );
        let debug = format!("{provider:?}");
        assert!(
            !debug.contains("super-secret"),
            "provider Debug must redact keys"
        );
    }

    #[test]
    fn rejects_multiple_cache_boundaries() {
        let mut req = request();
        req.messages = ["one", "two"]
            .into_iter()
            .map(|content| CompletionMessage {
                role: MessageRole::User,
                content: content.to_owned(),
                cache_boundary: true,
            })
            .collect();
        assert!(
            validate(&req).is_err(),
            "multiple cache boundaries must be rejected"
        );
    }

    #[tokio::test]
    async fn abort_mid_stream_closes_connection_and_emits_aborted_delta()
    -> Result<(), Box<dyn std::error::Error>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (closed_sender, closed_receiver) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            let mut request_bytes = [0_u8; 4096];
            let _count = socket.read(&mut request_bytes).await?;
            socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/x-ndjson\r\ntransfer-encoding: chunked\r\n\r\n").await?;
            let chunk = b"{\"message\":{\"content\":\"hello\"},\"done\":false}\n";
            socket
                .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                .await?;
            socket.write_all(chunk).await?;
            socket.write_all(b"\r\n").await?;
            socket.flush().await?;
            let mut byte = [0_u8; 1];
            let closed =
                tokio::time::timeout(std::time::Duration::from_secs(2), socket.read(&mut byte))
                    .await
                    .map(|result| result.map(|count| count == 0))
                    .unwrap_or(Ok(false))?;
            let _sent = closed_sender.send(closed);
            Ok::<(), std::io::Error>(())
        });

        let provider: std::sync::Arc<dyn CompletionProvider> = std::sync::Arc::new(
            Provider::new(ProviderKind::Ollama, "local", None)
                .with_base_url(format!("http://{address}/api/chat")),
        );
        let cancellation = CancellationToken::new();
        let mut stream = provider.stream(request(), cancellation.clone()).await?;
        let first = stream
            .next()
            .await
            .ok_or("mock stream ended before text")??;
        assert_eq!(first.text, "hello", "mock must deliver initial text");
        cancellation.cancel();
        let final_delta = stream
            .next()
            .await
            .ok_or("cancelled stream must terminate explicitly")??;
        assert_eq!(
            final_delta.stop_reason,
            Some(sotto_core::StopReason::Aborted),
            "a call cancelled after output must emit Aborted"
        );
        drop(stream);
        assert!(
            closed_receiver.await?,
            "aborting must close the HTTP connection"
        );
        server.await??;
        Ok(())
    }

    async fn mock_text(
        kind: ProviderKind,
        response_line: &'static str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            let mut request_bytes = [0_u8; 4096];
            let _count = socket.read(&mut request_bytes).await?;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\ncontent-type: application/octet-stream\r\n\r\n{response_line}\n",
                response_line.len() + 1
            );
            socket.write_all(response.as_bytes()).await?;
            Ok::<(), std::io::Error>(())
        });
        let key = (kind != ProviderKind::Ollama).then(|| SecretString::from("mock-key".to_owned()));
        let provider =
            Provider::new(kind, "default", key).with_base_url(format!("http://{address}"));
        let mut stream = provider.start(request())?.stream().await?;
        let mut text = String::new();
        while let Some(delta) = stream.next().await {
            text.push_str(&delta?.text);
        }
        server.await??;
        Ok(text)
    }

    #[tokio::test]
    async fn all_five_adapters_stream_end_to_end_against_http_mocks()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixtures = [
            (
                ProviderKind::Anthropic,
                r#"data: {"type":"content_block_delta","delta":{"text":"A"}}"#,
                "A",
            ),
            (
                ProviderKind::OpenAi,
                r#"data: {"choices":[{"delta":{"content":"B"}}]}"#,
                "B",
            ),
            (
                ProviderKind::Google,
                r#"data: {"candidates":[{"content":{"parts":[{"text":"C"}]}}]}"#,
                "C",
            ),
            (
                ProviderKind::OpenRouter,
                r#"data: {"choices":[{"delta":{"content":"D"}}]}"#,
                "D",
            ),
            (
                ProviderKind::Ollama,
                r#"{"message":{"content":"E"},"done":false}"#,
                "E",
            ),
        ];
        for (kind, line, expected) in fixtures {
            assert_eq!(
                mock_text(kind, line).await?,
                expected,
                "{kind:?} must stream normalized text through its HTTP adapter"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn google_key_is_only_sent_in_a_header() -> Result<(), Box<dyn std::error::Error>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            let mut request_bytes = [0_u8; 4096];
            let count = socket.read(&mut request_bytes).await?;
            let request = String::from_utf8_lossy(&request_bytes[..count]);
            assert!(
                request.starts_with("POST /requested-model:streamGenerateContent?alt=sse "),
                "Gemini URL must contain only the non-secret streaming query"
            );
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("x-goog-api-key: url-secret\r\n"),
                "Gemini key must be carried in x-goog-api-key"
            );
            assert!(
                !request
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .contains("url-secret"),
                "Gemini key must never occur in the request target"
            );
            socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                .await?;
            Ok::<(), std::io::Error>(())
        });
        let provider = Provider::new(
            ProviderKind::Google,
            "default",
            Some(SecretString::from("url-secret".to_owned())),
        )
        .with_base_url(format!("http://{address}"));
        let mut stream = provider.start(request())?.stream().await?;
        while let Some(delta) = stream.next().await {
            let _delta = delta?;
        }
        server.await??;
        Ok(())
    }

    #[tokio::test]
    async fn rate_limit_preserves_retry_after() -> Result<(), Box<dyn std::error::Error>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            let mut request_bytes = [0_u8; 4096];
            let _count = socket.read(&mut request_bytes).await?;
            socket.write_all(b"HTTP/1.1 429 Too Many Requests\r\nretry-after: 7\r\ncontent-length: 0\r\n\r\n").await?;
            Ok::<(), std::io::Error>(())
        });
        let provider = Provider::new(
            ProviderKind::OpenAi,
            "default",
            Some(SecretString::from("mock-key".to_owned())),
        )
        .with_base_url(format!("http://{address}"));
        let mut stream = provider.start(request())?.stream().await?;
        let error = stream
            .next()
            .await
            .ok_or("rate-limit mock must emit an error")?
            .err()
            .ok_or("rate-limit response must not be a delta")?;
        assert_eq!(
            error,
            sotto_core::ProviderError::RateLimit {
                retry_after: Some(std::time::Duration::from_secs(7))
            },
            "retry-after must survive normalization"
        );
        server.await??;
        Ok(())
    }
}
