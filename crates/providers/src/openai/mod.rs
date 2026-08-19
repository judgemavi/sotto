//! Direct OpenAI Responses API connector.
//!
//! The community SDK is deliberately contained in this module. Callers see only
//! Sotto's provider-neutral completion and backend contracts. Requests are
//! stateless (`store: false`), expose no tools, and default to JSON mode because
//! the initial insight notes and Ask consumers all consume structured JSON.

mod credentials;

pub use credentials::{delete_api_key, has_api_key, load_api_key, store_api_key};

use std::fmt;

use async_openai::{
    Client,
    config::OpenAIConfig,
    error::OpenAIError,
    types::responses::{
        CreateResponse, EasyInputContent, EasyInputMessage, ImageDetail, InputContent,
        InputImageContent, InputItem, InputParam, InputTextContent, MessageType,
        ResponseFormatJsonSchema, ResponseStreamEvent, ResponseTextParam, Role as OpenAiRole,
        TextResponseFormatConfiguration, ToolChoiceOptions, ToolChoiceParam,
    },
};
use futures_util::StreamExt;
use screen::{AuthorizedReasoningImage, ScreenSelector};
use secrecy::{ExposeSecret, SecretString};
use sotto_core::{
    BoxFuture, BoxStream, CancellationToken, CompletionProvider, CompletionRequest, Delta,
    MessageRole, ProviderError, ReasoningOutput, ReasoningRequest, StopReason, Usage,
};

use crate::{
    AuthKind, AuthStatus, BackendCapabilities, BackendCapability, BackendContractError,
    BackendDescriptor, BackendId, OPENAI_RESPONSES_BACKEND_ID, validate,
};

const DEFAULT_API_BASE: &str = "https://api.openai.com/v1";
const CONNECTOR_REVISION: u32 = 3;

/// Sotto-owned output selection; no SDK type crosses the connector boundary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OutputFormat {
    /// Plain text, useful for a minimal credential check.
    Text,
    /// OpenAI JSON mode. Prompts must still instruct the model which JSON to emit.
    #[default]
    JsonObject,
}

/// Optional BYOK Responses connector. Secret material is never included in `Debug`.
#[derive(Clone)]
pub struct OpenAiProvider {
    default_model: String,
    api_key: Option<SecretString>,
    api_base: String,
    output_format: OutputFormat,
}

impl fmt::Debug for OpenAiProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiProvider")
            .field("default_model", &self.default_model)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("api_base", &self.api_base)
            .field("output_format", &self.output_format)
            .finish()
    }
}

impl OpenAiProvider {
    /// Creates a connector whose readiness is explicit when no Keychain key exists.
    #[must_use]
    pub fn new(default_model: impl Into<String>, api_key: Option<SecretString>) -> Self {
        Self {
            default_model: default_model.into(),
            api_key,
            api_base: DEFAULT_API_BASE.to_owned(),
            output_format: OutputFormat::default(),
        }
    }

    /// Loads the optional API key through Sotto's existing OS credential-store path.
    pub fn from_keychain(default_model: impl Into<String>) -> Result<Self, ProviderError> {
        Ok(Self::new(default_model, load_api_key()?))
    }

    /// Overrides the endpoint for deterministic mock transport tests.
    #[must_use]
    pub fn with_api_base(mut self, api_base: impl Into<String>) -> Self {
        self.api_base = api_base.into();
        self
    }

    /// Selects plain text or JSON mode without exposing an SDK request type.
    #[must_use]
    pub fn with_output_format(mut self, output_format: OutputFormat) -> Self {
        self.output_format = output_format;
        self
    }

    /// Product metadata used by settings, Notes/Ask provider pickers, and derived-view caches.
    pub fn descriptor(&self) -> Result<BackendDescriptor, BackendContractError> {
        BackendDescriptor::new(
            BackendId::new(OPENAI_RESPONSES_BACKEND_ID)?,
            "OpenAI API",
            self.default_model.clone(),
            CONNECTOR_REVISION,
            BackendCapabilities::new([
                BackendCapability::Streaming,
                BackendCapability::Cancellation,
                BackendCapability::JsonObjectOutput,
                BackendCapability::JsonSchemaOutput,
                BackendCapability::UsageReporting,
                BackendCapability::ImageInput,
                BackendCapability::MaxTokens,
                BackendCapability::Temperature,
            ]),
            AuthKind::ApiKey,
            if self.api_key.is_some() {
                AuthStatus::Ready
            } else {
                AuthStatus::NeedsApiKey
            },
        )
    }
}

/// Backwards-compatible constructor name for callers that already hold a secret.
#[must_use]
pub fn configured(model: impl Into<String>, key: SecretString) -> OpenAiProvider {
    OpenAiProvider::new(model, Some(key))
}

impl CompletionProvider for OpenAiProvider {
    fn stream(
        &self,
        request: CompletionRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
    {
        let request = match self.output_format {
            OutputFormat::Text => ReasoningRequest::text(request),
            OutputFormat::JsonObject => ReasoningRequest::json_object(request),
        };
        self.dispatch(request, None, cancellation)
    }

    fn stream_reasoning(
        &self,
        request: ReasoningRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
    {
        self.dispatch(request, None, cancellation)
    }

    fn model_id(&self) -> &str {
        &self.default_model
    }
}

impl crate::ReasoningProvider for OpenAiProvider {
    fn supports_advanced_capability(&self, capability: BackendCapability) -> bool {
        matches!(
            capability,
            BackendCapability::JsonSchemaOutput | BackendCapability::ImageInput
        )
    }

    fn stream_advanced_reasoning(
        &self,
        request: ReasoningRequest,
        image: Option<AuthorizedReasoningImage>,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
    {
        self.dispatch(request, image, cancellation)
    }
}

impl OpenAiProvider {
    fn dispatch(
        &self,
        request: ReasoningRequest,
        image: Option<AuthorizedReasoningImage>,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<BoxStream<'static, Result<Delta, ProviderError>>, ProviderError>>
    {
        Box::pin(async move {
            validate_openai_request(&request, &self.default_model)?;
            let api_key = self.api_key.clone().ok_or(ProviderError::Auth)?;
            let api_base = self.api_base.clone();
            let sdk_request = build_sdk_request(request, image)?;
            let config = OpenAIConfig::new()
                .with_api_key(api_key.expose_secret())
                .with_api_base(api_base);
            let client = Client::with_config(config);

            let normalized = async_stream::stream! {
                let responses = client.responses();
                let mut create_stream = Box::pin(responses.create_stream(sdk_request));
                let mut upstream = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => {
                        // Do not retain the in-flight response-header future across
                        // the yielded cancellation item.
                        drop(create_stream);
                        yield Err(ProviderError::Cancelled);
                        return;
                    }
                    result = &mut create_stream => match result {
                        Ok(stream) => stream,
                        Err(error) => {
                            yield Err(normalize_sdk_error(error));
                            return;
                        }
                    }
                };

                let mut emitted_text = false;
                let mut latest_usage = None;
                let mut refusal = String::new();
                loop {
                    let event = tokio::select! {
                        biased;
                        () = cancellation.cancelled() => {
                            // async-openai owns the SSE parser task. Dropping its receiver
                            // is the SDK-supported signal that closes the upstream response.
                            drop(upstream);
                            if emitted_text {
                                yield Ok(Delta {
                                    text: String::new(),
                                    is_final: true,
                                    usage: latest_usage,
                                    stop_reason: Some(StopReason::Aborted),
                                });
                            } else {
                                yield Err(ProviderError::Cancelled);
                            }
                            return;
                        }
                        event = upstream.next() => event,
                    };

                    let Some(event) = event else {
                        if refusal.is_empty() {
                            yield Err(ProviderError::Decode(
                                "OpenAI Responses stream ended before a terminal event".to_owned(),
                            ));
                        } else {
                            yield Err(refusal_error(&refusal));
                        }
                        return;
                    };
                    match event {
                        Ok(ResponseStreamEvent::ResponseOutputTextDelta(event)) => {
                            emitted_text |= !event.delta.is_empty();
                            yield Ok(Delta {
                                text: event.delta,
                                is_final: false,
                                usage: None,
                                stop_reason: None,
                            });
                        }
                        Ok(ResponseStreamEvent::ResponseRefusalDelta(event)) => {
                            refusal.push_str(&event.delta);
                        }
                        Ok(ResponseStreamEvent::ResponseRefusalDone(event)) => {
                            if refusal.is_empty() {
                                refusal = event.refusal;
                            }
                            yield Err(refusal_error(&refusal));
                            return;
                        }
                        Ok(ResponseStreamEvent::ResponseCompleted(event)) => {
                            if !refusal.is_empty() {
                                yield Err(refusal_error(&refusal));
                                return;
                            }
                            latest_usage = event.response.usage.as_ref().map(normalize_usage);
                            yield Ok(Delta {
                                text: String::new(),
                                is_final: true,
                                usage: latest_usage,
                                stop_reason: Some(StopReason::EndTurn),
                            });
                            return;
                        }
                        Ok(ResponseStreamEvent::ResponseIncomplete(event)) => {
                            latest_usage = event.response.usage.as_ref().map(normalize_usage);
                            let reason = event
                                .response
                                .incomplete_details
                                .as_ref()
                                .map(|details| details.reason.as_str());
                            if matches!(reason, Some("max_output_tokens")) {
                                yield Ok(Delta {
                                    text: String::new(),
                                    is_final: true,
                                    usage: latest_usage,
                                    stop_reason: Some(StopReason::MaxTokens),
                                });
                            } else if let Some(reason) = reason {
                                yield Err(normalize_response_code(
                                    Some(reason),
                                    "OpenAI response was incomplete".to_owned(),
                                    422,
                                ));
                            } else {
                                yield Err(ProviderError::Upstream {
                                    status: 422,
                                    message: "OpenAI response was incomplete".to_owned(),
                                });
                            }
                            return;
                        }
                        Ok(ResponseStreamEvent::ResponseFailed(event)) => {
                            let error = event.response.error.as_ref();
                            yield Err(normalize_response_code(
                                error.map(|error| error.code.as_str()),
                                error.map_or_else(
                                    || "OpenAI response failed".to_owned(),
                                    |error| error.message.clone(),
                                ),
                                500,
                            ));
                            return;
                        }
                        Ok(ResponseStreamEvent::ResponseError(event)) => {
                            yield Err(normalize_event_error(
                                event.code.as_deref(),
                                event.message,
                            ));
                            return;
                        }
                        Ok(_) => {}
                        Err(error) => {
                            yield Err(normalize_sdk_error(error));
                            return;
                        }
                    }
                }
            };
            Ok(Box::pin(normalized) as BoxStream<'static, Result<Delta, ProviderError>>)
        })
    }
}

fn validate_openai_request(
    request: &ReasoningRequest,
    configured_model: &str,
) -> Result<(), ProviderError> {
    request
        .validate()
        .map_err(|error| ProviderError::InvalidRequest(error.to_string()))?;
    validate(&request.completion)?;
    if request.completion.model.trim().is_empty() {
        return Err(ProviderError::InvalidRequest(
            "model must not be empty".to_owned(),
        ));
    }
    if request.completion.model != configured_model {
        return Err(ProviderError::InvalidRequest(format!(
            "request model {:?} does not match configured backend model {:?}",
            request.completion.model, configured_model
        )));
    }
    if !request.completion.stop.is_empty() {
        return Err(ProviderError::InvalidRequest(
            "OpenAI Responses does not expose stop sequences through this connector".to_owned(),
        ));
    }
    if let ReasoningOutput::JsonSchema(schema) = &request.output {
        let value: serde_json::Value =
            serde_json::from_str(schema.schema_json()).map_err(|_| {
                ProviderError::InvalidRequest("JSON Schema is not valid JSON".to_owned())
            })?;
        if !value.is_object() {
            return Err(ProviderError::InvalidRequest(
                "JSON Schema root must be an object".to_owned(),
            ));
        }
    }
    Ok(())
}

fn build_sdk_request(
    request: ReasoningRequest,
    image: Option<AuthorizedReasoningImage>,
) -> Result<CreateResponse, ProviderError> {
    let ReasoningRequest { completion, output } = request;
    let mut items = completion
        .messages
        .into_iter()
        .map(|message| {
            InputItem::EasyMessage(EasyInputMessage {
                r#type: MessageType::Message,
                role: match message.role {
                    MessageRole::System => OpenAiRole::System,
                    MessageRole::User => OpenAiRole::User,
                    MessageRole::Assistant => OpenAiRole::Assistant,
                },
                content: EasyInputContent::Text(message.content),
                phase: None,
            })
        })
        .collect::<Vec<_>>();
    if let Some(image) = image {
        items.push(image_item(image));
    }
    let format = match output {
        ReasoningOutput::Text => TextResponseFormatConfiguration::Text,
        ReasoningOutput::JsonObject => TextResponseFormatConfiguration::JsonObject,
        ReasoningOutput::JsonSchema(schema) => {
            let schema_json = serde_json::from_str(schema.schema_json()).map_err(|_| {
                ProviderError::InvalidRequest("JSON Schema is not valid JSON".to_owned())
            })?;
            TextResponseFormatConfiguration::JsonSchema(ResponseFormatJsonSchema {
                description: schema.description().map(str::to_owned),
                name: schema.name().to_owned(),
                schema: schema_json,
                strict: Some(true),
            })
        }
    };
    Ok(CreateResponse {
        input: InputParam::Items(items),
        instructions: completion.system,
        max_output_tokens: completion.max_tokens,
        model: Some(completion.model),
        store: Some(false),
        stream: Some(true),
        temperature: completion.temperature,
        text: Some(ResponseTextParam::from(format)),
        tool_choice: Some(ToolChoiceParam::from(ToolChoiceOptions::None)),
        tools: Some(Vec::new()),
        ..CreateResponse::default()
    })
}

fn image_item(image: AuthorizedReasoningImage) -> InputItem {
    let provenance = image.provenance();
    let requested = match provenance.requested() {
        ScreenSelector::Timestamp(value) => {
            format!("timestamp:{:.3}s", value.as_secs_f64())
        }
        ScreenSelector::Event(value) => format!("event:{}", value.get()),
    };
    let visible_to = provenance
        .visible_to()
        .map(|value| format!("{:.3}s", value.as_secs_f64()))
        .unwrap_or_else(|| "session-end-unknown".to_owned());
    let evidence = format!(
        "Sampled screen evidence: requested={requested} captured_at={:.3}s visible_interval=[{:.3}s,{visible_to}) snapshot_event={}",
        provenance.captured_at().as_secs_f64(),
        provenance.visible_from().as_secs_f64(),
        provenance.snapshot_event_id().get(),
    );
    let data_url = format!(
        "data:{};base64,{}",
        image.media_type().as_mime(),
        encode_base64(image.bytes())
    );
    InputItem::EasyMessage(EasyInputMessage {
        r#type: MessageType::Message,
        role: OpenAiRole::User,
        content: EasyInputContent::ContentList(vec![
            InputContent::InputText(InputTextContent { text: evidence }),
            InputContent::InputImage(InputImageContent {
                detail: ImageDetail::Auto,
                file_id: None,
                image_url: Some(data_url),
            }),
        ]),
        phase: None,
    })
}

fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        encoded.push(char::from(ALPHABET[usize::from(first >> 2)]));
        encoded.push(char::from(
            ALPHABET[usize::from(((first & 0x03) << 4) | (second >> 4))],
        ));
        encoded.push(if chunk.len() > 1 {
            char::from(ALPHABET[usize::from(((second & 0x0f) << 2) | (third >> 6))])
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            char::from(ALPHABET[usize::from(third & 0x3f)])
        } else {
            '='
        });
    }
    encoded
}

fn normalize_usage(usage: &async_openai::types::responses::ResponseUsage) -> Usage {
    Usage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.input_tokens_details.cached_tokens,
        cache_write_tokens: 0,
    }
}

fn refusal_error(refusal: &str) -> ProviderError {
    let _ = refusal;
    ProviderError::Upstream {
        status: 422,
        // Refusal content can echo sensitive prompt material. Keep it out of the
        // cross-crate error while still exposing a stable refusal classification.
        message: "OpenAI model refused the request".to_owned(),
    }
}

fn normalize_event_error(code: Option<&str>, message: String) -> ProviderError {
    normalize_response_code(code, message, 500)
}

fn normalize_response_code(
    code: Option<&str>,
    message: String,
    fallback_status: u16,
) -> ProviderError {
    match code {
        Some("rate_limit_exceeded" | "rate_limit_error") => {
            ProviderError::RateLimit { retry_after: None }
        }
        Some("context_length_exceeded") => ProviderError::ContextLengthExceeded {
            limit: None,
            requested: None,
        },
        Some("invalid_api_key" | "authentication_error" | "insufficient_permissions") => {
            ProviderError::Auth
        }
        Some("invalid_request_error" | "invalid_request") => ProviderError::InvalidRequest(message),
        _ => ProviderError::Upstream {
            status: fallback_status,
            message,
        },
    }
}

fn normalize_sdk_error(error: OpenAIError) -> ProviderError {
    match error {
        OpenAIError::ApiError(response) => {
            let status = response.status_code.as_u16();
            let code = response.api_error.code.as_deref();
            match status {
                401 | 403 => ProviderError::Auth,
                429 => ProviderError::RateLimit {
                    // async-openai 0.41.3 does not expose response headers on ApiError.
                    retry_after: None,
                },
                400 if code == Some("context_length_exceeded") => {
                    ProviderError::ContextLengthExceeded {
                        limit: None,
                        requested: None,
                    }
                }
                400 => ProviderError::InvalidRequest(response.api_error.message),
                _ => ProviderError::Upstream {
                    status,
                    message: response.api_error.message,
                },
            }
        }
        OpenAIError::Reqwest(error) => ProviderError::Network(error.to_string()),
        OpenAIError::JSONDeserialize(error, _) => ProviderError::Decode(error.to_string()),
        OpenAIError::StreamError(error) => ProviderError::Decode(error.to_string()),
        OpenAIError::InvalidArgument(message) => ProviderError::InvalidRequest(message),
        other => ProviderError::Network(other.to_string()),
    }
}

#[cfg(test)]
mod tests;
