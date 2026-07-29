use std::{pin::pin, time::Duration};

use async_stream::stream;
use futures_util::StreamExt;
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use sotto_core::{
    BoxStream, CancellationToken, CompletionMessage, CompletionRequest, Delta, MessageRole,
    ProviderError, StopReason, Usage,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProviderKind {
    Anthropic,
    OpenAi,
    Google,
    OpenRouter,
    Ollama,
}

impl ProviderKind {
    pub(crate) const fn key_name(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Google => "google",
            Self::OpenRouter => "openrouter",
            Self::Ollama => "ollama",
        }
    }

    const fn default_url(self) -> &'static str {
        match self {
            Self::Anthropic => "https://api.anthropic.com/v1/messages",
            Self::OpenAi => "https://api.openai.com/v1/chat/completions",
            Self::Google => "https://generativelanguage.googleapis.com/v1beta/models",
            Self::OpenRouter => "https://openrouter.ai/api/v1/chat/completions",
            Self::Ollama => "http://127.0.0.1:11434/api/chat",
        }
    }
}

#[derive(Clone)]
pub struct Transport {
    pub(crate) base_url: String,
    kind: ProviderKind,
    client: Client,
}

impl Transport {
    pub(crate) fn new(kind: ProviderKind) -> Self {
        Self {
            base_url: kind.default_url().to_owned(),
            kind,
            client: Client::new(),
        }
    }

    pub(crate) fn stream(
        &self,
        req: CompletionRequest,
        key: Option<SecretString>,
        cancelled: CancellationToken,
    ) -> BoxStream<'static, Result<Delta, ProviderError>> {
        let this = self.clone();
        Box::pin(stream! {
            let response = tokio::select! {
                () = cancelled.cancelled() => { yield Err(ProviderError::Cancelled); return; }
                result = this.send(&req, key.as_ref()) => match result { Ok(response) => response, Err(error) => { yield Err(error); return; } }
            };
            let mut bytes = pin!(response.bytes_stream());
            let mut buffer = String::new();
            let mut emitted = false;
            let mut latest_usage = None;
            loop {
                let chunk = tokio::select! {
                    () = cancelled.cancelled() => {
                        if emitted { yield Ok(Delta { text: String::new(), is_final: true, usage: latest_usage, stop_reason: Some(StopReason::Aborted) }); }
                        else { yield Err(ProviderError::Cancelled); }
                        return;
                    }
                    chunk = bytes.next() => chunk,
                };
                let Some(chunk) = chunk else { break; };
                let chunk = match chunk { Ok(chunk) => chunk, Err(error) => { yield Err(ProviderError::Network(error.to_string())); return; } };
                let text = match std::str::from_utf8(&chunk) { Ok(text) => text, Err(error) => { yield Err(ProviderError::Decode(error.to_string())); return; } };
                buffer.push_str(text);
                while let Some(index) = buffer.find('\n') {
                    let line: String = buffer.drain(..=index).collect();
                    match parse_line(this.kind, line.trim()) {
                        Ok(Some(delta)) => {
                            emitted |= !delta.text.is_empty();
                            if delta.usage.is_some() { latest_usage.clone_from(&delta.usage); }
                            yield Ok(delta);
                        }
                        Ok(None) => {}
                        Err(error) => { yield Err(error); return; }
                    }
                }
            }
            if !buffer.trim().is_empty() {
                match parse_line(this.kind, buffer.trim()) { Ok(Some(delta)) => yield Ok(delta), Ok(None) => {}, Err(error) => yield Err(error) }
            }
        })
    }

    async fn send(
        &self,
        req: &CompletionRequest,
        key: Option<&SecretString>,
    ) -> Result<Response, ProviderError> {
        if self.kind != ProviderKind::Ollama && key.is_none() {
            return Err(ProviderError::Auth);
        }
        let mut request = self.client.post(self.url(req)).json(&body(self.kind, req));
        request = authenticate(request, self.kind, key);
        let response = request
            .send()
            .await
            .map_err(|error| ProviderError::Network(error.to_string()))?;
        map_status(response).await
    }

    fn url(&self, req: &CompletionRequest) -> String {
        if self.kind == ProviderKind::Google {
            format!(
                "{}/{}:streamGenerateContent?alt=sse",
                self.base_url.trim_end_matches('/'),
                req.model
            )
        } else {
            self.base_url.clone()
        }
    }
}

fn authenticate(
    request: RequestBuilder,
    kind: ProviderKind,
    key: Option<&SecretString>,
) -> RequestBuilder {
    let Some(key) = key else {
        return request;
    };
    match kind {
        ProviderKind::Anthropic => request
            .header("x-api-key", key.expose_secret())
            .header("anthropic-version", "2023-06-01"),
        ProviderKind::OpenAi | ProviderKind::OpenRouter => request.bearer_auth(key.expose_secret()),
        ProviderKind::Google => request.header("x-goog-api-key", key.expose_secret()),
        ProviderKind::Ollama => request,
    }
}

async fn map_status(response: Response) -> Result<Response, ProviderError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .map(Duration::from_secs);
    let message = response
        .text()
        .await
        .unwrap_or_else(|error| error.to_string());
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(ProviderError::Auth),
        StatusCode::TOO_MANY_REQUESTS => Err(ProviderError::RateLimit { retry_after }),
        _ if status.as_u16() == 400 && message.to_ascii_lowercase().contains("context") => {
            Err(ProviderError::ContextLengthExceeded {
                limit: None,
                requested: None,
            })
        }
        _ => Err(ProviderError::Upstream {
            status: status.as_u16(),
            message,
        }),
    }
}

fn body(kind: ProviderKind, req: &CompletionRequest) -> Value {
    match kind {
        ProviderKind::Anthropic => {
            json!({ "model": req.model, "system": req.system, "messages": req.messages.iter().map(anthropic_message).collect::<Vec<_>>(), "max_tokens": req.max_tokens.unwrap_or(1024), "temperature": req.temperature, "stop_sequences": req.stop, "stream": true })
        }
        ProviderKind::Google => {
            json!({ "system_instruction": req.system.as_ref().map(|text| json!({"parts":[{"text":text}]})), "contents": req.messages.iter().map(google_message).collect::<Vec<_>>(), "generationConfig": { "maxOutputTokens": req.max_tokens, "temperature": req.temperature, "stopSequences": req.stop } })
        }
        ProviderKind::Ollama => {
            json!({ "model": req.model, "messages": generic_messages(req), "stream": true, "options": { "temperature": req.temperature, "stop": req.stop, "num_predict": req.max_tokens } })
        }
        ProviderKind::OpenAi | ProviderKind::OpenRouter => {
            json!({ "model": req.model, "messages": generic_messages(req), "max_tokens": req.max_tokens, "temperature": req.temperature, "stop": req.stop, "stream": true, "stream_options": { "include_usage": true } })
        }
    }
}

fn generic_messages(req: &CompletionRequest) -> Vec<Value> {
    req.system
        .iter()
        .map(|content| json!({"role":"system", "content":content}))
        .chain(
            req.messages
                .iter()
                .map(|message| json!({"role":role(message.role), "content":message.content})),
        )
        .collect()
}
fn role(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}
fn anthropic_message(message: &CompletionMessage) -> Value {
    json!({ "role": role(message.role), "content": [{ "type":"text", "text":message.content, "cache_control": message.cache_boundary.then(|| json!({"type":"ephemeral"})) }] })
}
fn google_message(message: &CompletionMessage) -> Value {
    json!({ "role": if message.role == MessageRole::Assistant { "model" } else { "user" }, "parts":[{"text":message.content}] })
}

fn parse_line(kind: ProviderKind, line: &str) -> Result<Option<Delta>, ProviderError> {
    if line.is_empty() || line.starts_with(':') || line.starts_with("event:") {
        return Ok(None);
    }
    let data = line.strip_prefix("data:").map(str::trim).unwrap_or(line);
    if data == "[DONE]" {
        return Ok(Some(final_delta(None, StopReason::EndTurn)));
    }
    let value: Value =
        serde_json::from_str(data).map_err(|error| ProviderError::Decode(error.to_string()))?;
    match kind {
        ProviderKind::Anthropic => parse_anthropic(&value),
        ProviderKind::Google => parse_google(&value),
        ProviderKind::Ollama => parse_ollama(&value),
        ProviderKind::OpenAi | ProviderKind::OpenRouter => parse_openai(&value),
    }
}

fn parse_openai(value: &Value) -> Result<Option<Delta>, ProviderError> {
    let text = value
        .pointer("/choices/0/delta/content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let reason = value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str);
    let usage = value
        .get("usage")
        .filter(|usage| !usage.is_null())
        .map(usage_openai);
    if let Some(reason) = reason {
        return Ok(Some(final_delta(usage, stop_reason(reason))));
    }
    Ok((!text.is_empty() || usage.is_some()).then(|| Delta {
        text: text.to_owned(),
        is_final: usage.is_some(),
        usage,
        stop_reason: None,
    }))
}
fn parse_anthropic(value: &Value) -> Result<Option<Delta>, ProviderError> {
    match value.get("type").and_then(Value::as_str) {
        Some("content_block_delta") => Ok(value
            .pointer("/delta/text")
            .and_then(Value::as_str)
            .map(text_delta)),
        Some("message_delta") => Ok(Some(final_delta(
            value.get("usage").map(usage_anthropic),
            value
                .pointer("/delta/stop_reason")
                .and_then(Value::as_str)
                .map(stop_reason)
                .unwrap_or(StopReason::EndTurn),
        ))),
        Some("error") => Err(ProviderError::Upstream {
            status: 200,
            message: value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("provider stream error")
                .to_owned(),
        }),
        _ => Ok(None),
    }
}
fn parse_google(value: &Value) -> Result<Option<Delta>, ProviderError> {
    let text = value
        .pointer("/candidates/0/content/parts/0/text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let reason = value
        .pointer("/candidates/0/finishReason")
        .and_then(Value::as_str);
    let usage = value.get("usageMetadata").map(usage_google);
    Ok(if reason.is_some() {
        Some(Delta {
            text: text.to_owned(),
            is_final: true,
            usage,
            stop_reason: Some(stop_reason(reason.unwrap_or_default())),
        })
    } else if text.is_empty() {
        None
    } else {
        Some(text_delta(text))
    })
}
fn parse_ollama(value: &Value) -> Result<Option<Delta>, ProviderError> {
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(ProviderError::Upstream {
            status: 200,
            message: error.to_owned(),
        });
    }
    let text = value
        .pointer("/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if value.get("done").and_then(Value::as_bool).unwrap_or(false) {
        Ok(Some(Delta {
            text: text.to_owned(),
            is_final: true,
            usage: Some(Usage {
                input_tokens: u32_field(value, "prompt_eval_count"),
                output_tokens: u32_field(value, "eval_count"),
                ..Usage::default()
            }),
            stop_reason: Some(StopReason::EndTurn),
        }))
    } else {
        Ok((!text.is_empty()).then(|| text_delta(text)))
    }
}

fn text_delta(text: &str) -> Delta {
    Delta {
        text: text.to_owned(),
        is_final: false,
        usage: None,
        stop_reason: None,
    }
}
fn final_delta(usage: Option<Usage>, reason: StopReason) -> Delta {
    Delta {
        text: String::new(),
        is_final: true,
        usage,
        stop_reason: Some(reason),
    }
}
fn stop_reason(reason: &str) -> StopReason {
    match reason.to_ascii_lowercase().as_str() {
        "max_tokens" | "maxlength" => StopReason::MaxTokens,
        "stop_sequence" | "stop" => StopReason::StopSequence,
        _ => StopReason::EndTurn,
    }
}
fn u32_value(value: Option<&Value>) -> u32 {
    value
        .and_then(Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
        .unwrap_or_default()
}
fn u32_field(value: &Value, field: &str) -> u32 {
    u32_value(value.get(field))
}
fn usage_openai(value: &Value) -> Usage {
    Usage {
        input_tokens: u32_field(value, "prompt_tokens"),
        output_tokens: u32_field(value, "completion_tokens"),
        cache_read_tokens: u32_value(value.pointer("/prompt_tokens_details/cached_tokens")),
        cache_write_tokens: 0,
    }
}
fn usage_anthropic(value: &Value) -> Usage {
    Usage {
        input_tokens: u32_field(value, "input_tokens"),
        output_tokens: u32_field(value, "output_tokens"),
        cache_read_tokens: u32_field(value, "cache_read_input_tokens"),
        cache_write_tokens: u32_field(value, "cache_creation_input_tokens"),
    }
}
fn usage_google(value: &Value) -> Usage {
    Usage {
        input_tokens: u32_field(value, "promptTokenCount"),
        output_tokens: u32_field(value, "candidatesTokenCount"),
        cache_read_tokens: u32_field(value, "cachedContentTokenCount"),
        cache_write_tokens: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{ProviderKind, body, parse_line};
    use sotto_core::{CompletionMessage, CompletionRequest, MessageRole};

    fn request() -> CompletionRequest {
        CompletionRequest {
            model: "call-model".to_owned(),
            system: Some("system".to_owned()),
            messages: vec![CompletionMessage {
                role: MessageRole::User,
                content: "hello".to_owned(),
                cache_boundary: true,
            }],
            max_tokens: Some(50),
            temperature: Some(0.2),
            stop: vec!["stop".to_owned()],
        }
    }

    #[test]
    fn provider_bodies_use_request_model_and_anthropic_cache_boundary() {
        for kind in [
            ProviderKind::Anthropic,
            ProviderKind::OpenAi,
            ProviderKind::Google,
            ProviderKind::OpenRouter,
            ProviderKind::Ollama,
        ] {
            let value = body(kind, &request());
            if kind != ProviderKind::Google {
                assert_eq!(
                    value.get("model").and_then(serde_json::Value::as_str),
                    Some("call-model"),
                    "request model must be authoritative for {kind:?}"
                );
            }
        }
        let anthropic = body(ProviderKind::Anthropic, &request());
        assert_eq!(
            anthropic
                .pointer("/messages/0/content/0/cache_control/type")
                .and_then(serde_json::Value::as_str),
            Some("ephemeral"),
            "Anthropic must map the portable cache boundary"
        );
    }

    #[test]
    fn all_provider_stream_formats_normalize_text() -> Result<(), Box<dyn std::error::Error>> {
        let fixtures = [
            (
                ProviderKind::Anthropic,
                r#"data: {"type":"content_block_delta","delta":{"text":"A"}}"#,
            ),
            (
                ProviderKind::OpenAi,
                r#"data: {"choices":[{"delta":{"content":"B"}}]}"#,
            ),
            (
                ProviderKind::Google,
                r#"data: {"candidates":[{"content":{"parts":[{"text":"C"}]}}]}"#,
            ),
            (
                ProviderKind::OpenRouter,
                r#"data: {"choices":[{"delta":{"content":"D"}}]}"#,
            ),
            (
                ProviderKind::Ollama,
                r#"{"message":{"content":"E"},"done":false}"#,
            ),
        ];
        for (kind, fixture) in fixtures {
            let delta = parse_line(kind, fixture)?.ok_or("fixture must produce a delta")?;
            assert!(
                !delta.text.is_empty(),
                "{kind:?} fixture must normalize text"
            );
            assert!(!delta.is_final, "text chunk must remain partial");
        }
        Ok(())
    }

    #[test]
    fn malformed_stream_data_is_a_decode_error() {
        assert!(
            parse_line(ProviderKind::OpenAi, "data: not-json").is_err(),
            "malformed SSE must be rejected"
        );
    }
}
