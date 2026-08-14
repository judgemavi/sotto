use std::{sync::Arc, time::Duration};

use futures_util::StreamExt;
use screen::{
    AuthorizedReasoningImage, ImageInspectionPolicy, InspectScreenRequest, OcrEngine,
    RetainedScreenInspector, ScreenError, ScreenEvidence, ScreenInspectionSource, ScreenSelector,
};
use secrecy::SecretString;
use serde_json::{Value, json};
use sotto_core::{
    CancellationToken, CaptureTarget, CompletionMessage, CompletionProvider, CompletionRequest,
    EventPayload, FrameRef, JsonSchemaConstraint, MessageRole, ProviderError, ReasoningRequest,
    ScreenSnapshot, Session, SessionId, StopReason, TargetKind, TimelineBuilder, Usage,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};

use super::{OpenAiProvider, OutputFormat, normalize_sdk_error};
use crate::{
    AuthKind, AuthStatus, BackendCapabilities, BackendDescriptor, BackendId, CODEX_CLI_BACKEND_ID,
    Registry, Role,
};
use async_openai::error::{ApiError, ApiErrorResponse, OpenAIError};

fn request() -> CompletionRequest {
    CompletionRequest {
        model: "mock-model".to_owned(),
        system: Some("Return the requested JSON.".to_owned()),
        messages: vec![
            CompletionMessage {
                role: MessageRole::User,
                content: "Summarize event 7.".to_owned(),
                cache_boundary: false,
            },
            CompletionMessage {
                role: MessageRole::Assistant,
                content: "Previous turn.".to_owned(),
                cache_boundary: false,
            },
        ],
        max_tokens: Some(128),
        temperature: Some(0.0),
        stop: Vec::new(),
    }
}

fn provider(api_base: String) -> OpenAiProvider {
    OpenAiProvider::new(
        "mock-model",
        Some(SecretString::from("mock-secret".to_owned())),
    )
    .with_api_base(api_base)
}

struct NoOcr;

impl OcrEngine for NoOcr {
    fn recognize(&self, _frame: &screen::Frame) -> Result<String, ScreenError> {
        Err(ScreenError::Ocr(
            "OCR must not run for image transport".to_owned(),
        ))
    }
}

fn authorized_png()
-> Result<(tempfile::TempDir, AuthorizedReasoningImage), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("frame.png");
    std::fs::write(
        &path,
        [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3],
    )?;
    let session = Session::new(
        SessionId::new(31),
        CaptureTarget {
            bundle_id: Some("com.apple.Keynote".to_owned()),
            display_name: "Keynote".to_owned(),
            window_title: Some("Pricing".to_owned()),
            kind: TargetKind::Window,
            audio_scoped: true,
        },
        0,
    );
    let mut timeline = TimelineBuilder::new(session);
    timeline.append(
        Duration::from_secs(10),
        EventPayload::ScreenSnapshot(ScreenSnapshot {
            frame_ref: FrameRef::new(path.to_string_lossy()),
            ocr_text: String::new(),
            active_app: Some("Keynote".to_owned()),
            window_title: Some("Pricing".to_owned()),
            visible_from: Duration::from_secs(10),
            visible_to: Some(Duration::from_secs(20)),
        }),
    );
    let request = InspectScreenRequest {
        selector: ScreenSelector::Timestamp(Duration::from_secs(12)),
        evidence: ScreenEvidence::Image,
        reason: "ground the recap".to_owned(),
    };
    let mut inspection = RetainedScreenInspector::new(
        directory.path(),
        None::<NoOcr>,
        ImageInspectionPolicy::Allow,
    )
    .inspect(timeline.events(), &request);
    let image = inspection
        .take_authorized_image_for(&request)
        .ok_or("screen policy did not mint authorized image")?;
    Ok((directory, image))
}

fn text_delta(sequence_number: u64, delta: &str) -> Value {
    json!({
        "type": "response.output_text.delta",
        "sequence_number": sequence_number,
        "item_id": "msg_1",
        "output_index": 0,
        "content_index": 0,
        "delta": delta
    })
}

fn completed(sequence_number: u64, input: u32, output: u32, cached: u32) -> Value {
    json!({
        "type": "response.completed",
        "sequence_number": sequence_number,
        "response": {
            "created_at": 1,
            "completed_at": 2,
            "id": "resp_1",
            "model": "mock-model",
            "object": "response",
            "output": [],
            "status": "completed",
            "usage": {
                "input_tokens": input,
                "input_tokens_details": { "cached_tokens": cached },
                "output_tokens": output,
                "output_tokens_details": { "reasoning_tokens": 2 },
                "total_tokens": input + output
            }
        }
    })
}

fn sse_body(events: impl IntoIterator<Item = Value>) -> String {
    events.into_iter().fold(String::new(), |mut body, event| {
        body.push_str("data: ");
        body.push_str(&event.to_string());
        body.push_str("\n\n");
        body
    })
}

async fn read_request(
    socket: &mut TcpStream,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let count = socket.read(&mut chunk).await?;
        if count == 0 {
            return Err("connection closed before request headers".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        let mut chunk = [0_u8; 4096];
        let count = socket.read(&mut chunk).await?;
        if count == 0 {
            return Err("connection closed before request body".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(String::from_utf8(bytes)?)
}

async fn spawn_response(
    status: &str,
    content_type: &str,
    body: String,
) -> Result<(String, oneshot::Receiver<String>), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (request_sender, request_receiver) = oneshot::channel();
    let status = status.to_owned();
    let content_type = content_type.to_owned();
    tokio::spawn(async move {
        let result: Result<(), Box<dyn std::error::Error + Send + Sync>> = async {
            let (mut socket, _) = listener.accept().await?;
            let request = read_request(&mut socket).await?;
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await?;
            socket.shutdown().await?;
            let _ = request_sender.send(request);
            Ok(())
        }
        .await;
        assert!(result.is_ok(), "mock server failed: {result:?}");
    });
    Ok((format!("http://{address}"), request_receiver))
}

#[tokio::test]
async fn sdk_responses_stream_normalizes_text_json_mode_and_usage()
-> Result<(), Box<dyn std::error::Error>> {
    let body = sse_body([
        text_delta(1, "{\"summary\":"),
        text_delta(2, "\"clear\"}"),
        completed(3, 19, 7, 5),
    ]);
    let (api_base, request_receiver) = spawn_response("200 OK", "text/event-stream", body).await?;
    let provider = provider(api_base);
    let mut stream = provider.stream(request(), CancellationToken::new()).await?;
    let deltas = stream.by_ref().collect::<Vec<_>>().await;

    assert_eq!(deltas.len(), 3, "two text deltas and one terminal delta");
    assert_eq!(
        deltas[0].as_ref().map_err(Clone::clone)?.text,
        "{\"summary\":",
        "first SDK text event must remain incremental"
    );
    assert_eq!(
        deltas[1].as_ref().map_err(Clone::clone)?.text,
        "\"clear\"}",
        "second SDK text event must remain incremental"
    );
    assert_eq!(
        deltas[2].as_ref().map_err(Clone::clone)?,
        &sotto_core::Delta {
            text: String::new(),
            is_final: true,
            usage: Some(Usage {
                input_tokens: 19,
                output_tokens: 7,
                cache_read_tokens: 5,
                cache_write_tokens: 0,
            }),
            stop_reason: Some(StopReason::EndTurn),
        },
        "completed event must carry normalized usage and stop reason"
    );

    let raw_request = request_receiver.await?;
    assert!(
        raw_request.starts_with("POST /responses HTTP/1.1"),
        "product path must call the Responses endpoint"
    );
    let body_start = raw_request
        .find("\r\n\r\n")
        .ok_or("mock request omitted body separator")?;
    let request_json: Value = serde_json::from_str(&raw_request[body_start + 4..])?;
    assert_eq!(request_json["store"], false, "requests must be stateless");
    assert_eq!(
        request_json["stream"], true,
        "SDK streaming must be enabled"
    );
    assert!(
        request_json.get("stream_options").is_none(),
        "connector must preserve OpenAI's default stream obfuscation"
    );
    assert_eq!(
        request_json["text"]["format"]["type"], "json_object",
        "reasoning defaults to JSON object mode"
    );
    assert_eq!(
        request_json["tool_choice"], "none",
        "text-only reasoning must disable tool selection"
    );
    assert_eq!(
        request_json["tools"],
        json!([]),
        "text-only reasoning must expose no tools"
    );
    assert_eq!(
        request_json["input"][0]["role"], "user",
        "user role must survive SDK normalization"
    );
    assert_eq!(
        request_json["input"][1]["role"], "assistant",
        "assistant history must survive SDK normalization"
    );
    Ok(())
}

#[tokio::test]
async fn sdk_request_maps_strict_schema_and_consented_bounded_image()
-> Result<(), Box<dyn std::error::Error>> {
    let body = sse_body([completed(1, 12, 3, 0)]);
    let (api_base, request_receiver) = spawn_response("200 OK", "text/event-stream", body).await?;
    let schema = JsonSchemaConstraint::new(
        "screen_recap",
        Some("Grounded recap".to_owned()),
        r#"{"type":"object","properties":{"recap":{"type":"string"}},"required":["recap"],"additionalProperties":false}"#,
    )?;
    let (_directory, image) = authorized_png()?;
    let provider = provider(api_base);
    let descriptor = provider.descriptor()?;
    let id = descriptor.id().clone();
    let mut registry = Registry::default();
    registry.register_reasoning(descriptor, Arc::new(provider))?;
    registry.select(Role::Summarizer, Some(&id))?;
    let resolved = registry
        .resolve(Role::Summarizer)?
        .ok_or("OpenAI backend must resolve")?;
    let mut stream = resolved
        .provider()
        .stream_advanced_reasoning(
            ReasoningRequest::json_schema(request(), schema),
            Some(image),
            CancellationToken::new(),
        )
        .await?;
    while let Some(delta) = stream.next().await {
        delta?;
    }

    let raw_request = request_receiver.await?;
    let (_, body) = raw_request
        .split_once("\r\n\r\n")
        .ok_or("request body separator missing")?;
    let request_json: Value = serde_json::from_str(body)?;
    assert_eq!(request_json["text"]["format"]["type"], "json_schema");
    assert_eq!(request_json["text"]["format"]["name"], "screen_recap");
    assert_eq!(request_json["text"]["format"]["strict"], true);
    assert_eq!(
        request_json["text"]["format"]["schema"]["required"],
        json!(["recap"])
    );
    let content = request_json["input"]
        .as_array()
        .and_then(|input| input.last())
        .and_then(|message| message["content"].as_array())
        .ok_or("image content list missing")?;
    assert_eq!(content[0]["type"], "input_text");
    assert!(
        content[0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("snapshot_event=1")),
        "provenance must travel alongside the sampled image"
    );
    assert_eq!(content[1]["type"], "input_image");
    assert_eq!(
        content[1]["image_url"],
        "data:image/png;base64,iVBORw0KGgoBAgM="
    );
    assert!(
        !body.contains("file://") && !body.contains("/tmp/") && !body.contains("/private/"),
        "no local frame path may enter the request"
    );
    assert_eq!(request_json["tools"], json!([]));
    assert_eq!(request_json["tool_choice"], "none");
    assert_eq!(request_json["store"], false);
    Ok(())
}

#[tokio::test]
async fn malformed_schema_is_rejected_before_transport() -> Result<(), Box<dyn std::error::Error>> {
    let schema = JsonSchemaConstraint::new("bad_schema", None, "{not-json")?;
    let result = provider("http://127.0.0.1:1".to_owned())
        .stream_reasoning(
            ReasoningRequest::json_schema(request(), schema),
            CancellationToken::new(),
        )
        .await;
    assert!(matches!(result, Err(ProviderError::InvalidRequest(_))));
    Ok(())
}

#[tokio::test]
async fn plain_text_mode_is_explicit_and_does_not_leak_sdk_types()
-> Result<(), Box<dyn std::error::Error>> {
    let (api_base, request_receiver) = spawn_response(
        "200 OK",
        "text/event-stream",
        sse_body([text_delta(1, "OK"), completed(2, 1, 1, 0)]),
    )
    .await?;
    let provider = provider(api_base).with_output_format(OutputFormat::Text);
    let mut stream = provider.stream(request(), CancellationToken::new()).await?;
    while let Some(delta) = stream.next().await {
        delta?;
    }
    let raw_request = request_receiver.await?;
    let body_start = raw_request.find("\r\n\r\n").ok_or("missing body")?;
    let request_json: Value = serde_json::from_str(&raw_request[body_start + 4..])?;
    assert_eq!(
        request_json["text"]["format"]["type"], "text",
        "plain-text mode must be explicit on the SDK request"
    );
    Ok(())
}

#[tokio::test]
async fn refusal_is_normalized_without_echoing_refusal_content()
-> Result<(), Box<dyn std::error::Error>> {
    let private_refusal = "private prompt excerpt";
    let refusal = json!({
        "type": "response.refusal.done",
        "sequence_number": 1,
        "item_id": "msg_1",
        "output_index": 0,
        "content_index": 0,
        "refusal": private_refusal
    });
    let (api_base, _) = spawn_response("200 OK", "text/event-stream", sse_body([refusal])).await?;
    let mut stream = provider(api_base)
        .stream(request(), CancellationToken::new())
        .await?;
    let result = stream.next().await.ok_or("missing refusal")?;
    let Err(error) = result else {
        return Err("refusal was normalized as a successful delta".into());
    };
    assert_eq!(
        error,
        ProviderError::Upstream {
            status: 422,
            message: "OpenAI model refused the request".to_owned(),
        },
        "refusal must have a stable content-free classification"
    );
    assert!(
        !error.to_string().contains(private_refusal),
        "refusal error must not echo model or prompt-derived content"
    );
    Ok(())
}

#[tokio::test]
async fn refusal_delta_cannot_be_completed_as_success() -> Result<(), Box<dyn std::error::Error>> {
    let private_refusal = "private prompt excerpt";
    let refusal_delta = json!({
        "type": "response.refusal.delta",
        "sequence_number": 1,
        "item_id": "msg_1",
        "output_index": 0,
        "content_index": 0,
        "delta": private_refusal
    });
    let (api_base, _) = spawn_response(
        "200 OK",
        "text/event-stream",
        sse_body([refusal_delta, completed(2, 1, 1, 0)]),
    )
    .await?;
    let mut stream = provider(api_base)
        .stream(request(), CancellationToken::new())
        .await?;
    let result = stream.next().await.ok_or("missing terminal refusal")?;
    let Err(error) = result else {
        return Err("accumulated refusal content was normalized as success".into());
    };
    assert_eq!(
        error,
        ProviderError::Upstream {
            status: 422,
            message: "OpenAI model refused the request".to_owned(),
        },
        "accumulated refusal content must fail closed even if the stream skips refusal.done"
    );
    assert!(
        !error.to_string().contains(private_refusal),
        "refusal details must remain redacted"
    );
    Ok(())
}

#[tokio::test]
async fn rate_limit_is_normalized_without_credentials_in_ci()
-> Result<(), Box<dyn std::error::Error>> {
    let rate_limit = json!({
        "type": "error",
        "sequence_number": 1,
        "code": "rate_limit_exceeded",
        "message": "slow down",
        "param": null
    });
    let (api_base, _) =
        spawn_response("200 OK", "text/event-stream", sse_body([rate_limit])).await?;
    let mut stream = provider(api_base)
        .stream(request(), CancellationToken::new())
        .await?;
    assert_eq!(
        stream.next().await.ok_or("missing rate-limit error")?,
        Err(ProviderError::RateLimit { retry_after: None }),
        "stream error event must normalize rate limits"
    );
    Ok(())
}

#[test]
fn sdk_http_429_is_normalized_without_raw_wire_handling() {
    let error = OpenAIError::ApiError(ApiErrorResponse {
        status_code: reqwest::StatusCode::TOO_MANY_REQUESTS,
        api_error: ApiError {
            message: "slow down".to_owned(),
            r#type: Some("rate_limit_error".to_owned()),
            param: None,
            code: Some("rate_limit_exceeded".to_owned()),
        },
    });
    assert_eq!(
        normalize_sdk_error(error),
        ProviderError::RateLimit { retry_after: None },
        "SDK HTTP 429 must normalize without a raw response parser"
    );
}

#[tokio::test]
async fn failed_terminal_uses_machine_readable_error_code() -> Result<(), Box<dyn std::error::Error>>
{
    let failed = json!({
        "type": "response.failed",
        "sequence_number": 1,
        "response": {
            "created_at": 1,
            "id": "resp_failed",
            "model": "mock-model",
            "object": "response",
            "output": [],
            "status": "failed",
            "error": {
                "code": "context_length_exceeded",
                "message": "request exceeded the context window"
            }
        }
    });
    let (api_base, _) = spawn_response("200 OK", "text/event-stream", sse_body([failed])).await?;
    let mut stream = provider(api_base)
        .stream(request(), CancellationToken::new())
        .await?;
    assert_eq!(
        stream.next().await.ok_or("missing failed terminal")?,
        Err(ProviderError::ContextLengthExceeded {
            limit: None,
            requested: None,
        }),
        "response.failed must use its machine-readable code"
    );
    Ok(())
}

#[tokio::test]
async fn incomplete_terminal_normalizes_reason_and_max_token_stop()
-> Result<(), Box<dyn std::error::Error>> {
    let rate_limited = json!({
        "type": "response.incomplete",
        "sequence_number": 1,
        "response": {
            "created_at": 1,
            "id": "resp_incomplete_rate",
            "model": "mock-model",
            "object": "response",
            "output": [],
            "status": "incomplete",
            "incomplete_details": { "reason": "rate_limit_exceeded" }
        }
    });
    let (api_base, _) =
        spawn_response("200 OK", "text/event-stream", sse_body([rate_limited])).await?;
    let mut stream = provider(api_base)
        .stream(request(), CancellationToken::new())
        .await?;
    assert_eq!(
        stream.next().await.ok_or("missing incomplete terminal")?,
        Err(ProviderError::RateLimit { retry_after: None }),
        "response.incomplete must normalize its machine-readable reason"
    );

    let max_tokens = json!({
        "type": "response.incomplete",
        "sequence_number": 1,
        "response": {
            "created_at": 1,
            "id": "resp_incomplete_tokens",
            "model": "mock-model",
            "object": "response",
            "output": [],
            "status": "incomplete",
            "incomplete_details": { "reason": "max_output_tokens" },
            "usage": {
                "input_tokens": 11,
                "input_tokens_details": { "cached_tokens": 3 },
                "output_tokens": 9,
                "output_tokens_details": { "reasoning_tokens": 2 },
                "total_tokens": 20
            }
        }
    });
    let (api_base, _) =
        spawn_response("200 OK", "text/event-stream", sse_body([max_tokens])).await?;
    let mut stream = provider(api_base)
        .stream(request(), CancellationToken::new())
        .await?;
    assert_eq!(
        stream.next().await.ok_or("missing max-token terminal")?,
        Ok(sotto_core::Delta {
            text: String::new(),
            is_final: true,
            usage: Some(Usage {
                input_tokens: 11,
                output_tokens: 9,
                cache_read_tokens: 3,
                cache_write_tokens: 0,
            }),
            stop_reason: Some(StopReason::MaxTokens),
        }),
        "max_output_tokens must remain a successful terminal stop with usage"
    );
    Ok(())
}

#[tokio::test]
async fn malformed_event_is_a_decode_error() -> Result<(), Box<dyn std::error::Error>> {
    let (api_base, _) = spawn_response(
        "200 OK",
        "text/event-stream",
        "data: {not valid json}\n\n".to_owned(),
    )
    .await?;
    let mut stream = provider(api_base)
        .stream(request(), CancellationToken::new())
        .await?;
    assert!(
        matches!(
            stream.next().await.ok_or("missing decode error")?,
            Err(ProviderError::Decode(_))
        ),
        "malformed SDK event must become a decode error"
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_drops_sdk_stream_closes_transport_and_preserves_usage_shape()
-> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (closed_sender, closed_receiver) = oneshot::channel();
    tokio::spawn(async move {
        let result: Result<(), Box<dyn std::error::Error + Send + Sync>> = async {
            let (mut socket, _) = listener.accept().await?;
            let _request = read_request(&mut socket).await?;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n")
                .await?;
            socket
                .write_all(format!("data: {}\n\n", text_delta(1, "hello")).as_bytes())
                .await?;
            let mut byte = [0_u8; 1];
            let count = socket.read(&mut byte).await?;
            closed_sender
                .send(count == 0)
                .map_err(|_| "closed receiver dropped")?;
            Ok(())
        }
        .await;
        assert!(result.is_ok(), "cancellation mock failed: {result:?}");
    });

    let cancellation = CancellationToken::new();
    let mut stream = provider(format!("http://{address}"))
        .stream(request(), cancellation.clone())
        .await?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await?
            .ok_or("missing first delta")??
            .text,
        "hello",
        "first SDK delta must arrive before cancellation"
    );
    cancellation.cancel();
    let final_delta = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await?
        .ok_or("missing abort delta")??;
    assert!(
        final_delta.is_final,
        "post-text cancellation must be terminal"
    );
    assert_eq!(
        final_delta.stop_reason,
        Some(StopReason::Aborted),
        "post-text cancellation must use the aborted stop reason"
    );
    assert_eq!(
        final_delta.usage, None,
        "cancellation must not fabricate provider usage"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(2), closed_receiver).await??,
        "dropping the SDK receiver must close the in-flight HTTP response"
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_before_response_headers_is_cancelled_and_closes_transport()
-> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (accepted_sender, accepted_receiver) = oneshot::channel();
    let (closed_sender, closed_receiver) = oneshot::channel();
    tokio::spawn(async move {
        let result: Result<(), Box<dyn std::error::Error + Send + Sync>> = async {
            let (mut socket, _) = listener.accept().await?;
            let _request = read_request(&mut socket).await?;
            accepted_sender
                .send(())
                .map_err(|_| "accepted receiver dropped")?;
            let mut byte = [0_u8; 1];
            let count = socket.read(&mut byte).await?;
            closed_sender
                .send(count == 0)
                .map_err(|_| "closed receiver dropped")?;
            Ok(())
        }
        .await;
        assert!(
            result.is_ok(),
            "pre-header cancellation mock failed: {result:?}"
        );
    });

    let cancellation = CancellationToken::new();
    let mut stream = provider(format!("http://{address}"))
        .stream(request(), cancellation.clone())
        .await?;
    let next = stream.next();
    tokio::pin!(next);
    tokio::select! {
        result = accepted_receiver => result?,
        item = &mut next => return Err(format!("stream ended before cancellation: {item:?}").into()),
    }
    cancellation.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), &mut next)
            .await?
            .ok_or("missing cancellation error")?,
        Err(ProviderError::Cancelled),
        "pre-text cancellation must be an error, not a final delta"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(2), closed_receiver).await??,
        "cancelling response-header wait must close the HTTP request"
    );
    Ok(())
}

#[test]
fn descriptor_is_key_aware_and_reports_proven_request_shapes()
-> Result<(), Box<dyn std::error::Error>> {
    let missing = OpenAiProvider::new("mock-model", None).descriptor()?;
    assert_eq!(
        missing.auth_kind(),
        AuthKind::ApiKey,
        "direct OpenAI authentication is Sotto-owned BYOK"
    );
    assert_eq!(
        missing.auth_status(),
        &AuthStatus::NeedsApiKey,
        "missing Keychain value must remain an ordinary unavailable state"
    );
    assert!(
        missing
            .capabilities()
            .contains(crate::BackendCapability::ImageInput),
        "image capability is a transport property independent of current auth readiness"
    );
    assert!(
        !missing
            .capabilities()
            .contains(crate::BackendCapability::ToolCalling),
        "connector that exposes no tools cannot advertise tool calling"
    );

    let ready = provider("http://127.0.0.1:1".to_owned()).descriptor()?;
    assert_eq!(
        ready.auth_status(),
        &AuthStatus::Ready,
        "an explicitly supplied secret makes the connector ready"
    );
    assert!(
        ready
            .capabilities()
            .contains(crate::BackendCapability::JsonObjectOutput),
        "OpenAI currently exposes JSON object output"
    );
    assert!(
        ready
            .capabilities()
            .contains(crate::BackendCapability::JsonSchemaOutput),
        "strict schema mapping is covered by a Sotto-owned request-shape test"
    );
    assert!(
        ready
            .capabilities()
            .contains(crate::BackendCapability::MaxTokens),
        "OpenAI must keep applying the caller's output-token limit"
    );
    assert!(
        ready
            .capabilities()
            .contains(crate::BackendCapability::Temperature),
        "OpenAI must keep applying the caller's sampling temperature"
    );
    Ok(())
}

#[test]
fn backend_normalization_preserves_openai_sampling_controls()
-> Result<(), Box<dyn std::error::Error>> {
    let descriptor = provider("http://127.0.0.1:1".to_owned()).descriptor()?;
    let prepared = crate::backend::prepare_reasoning_request(
        &descriptor,
        ReasoningRequest::json_object(request()),
        crate::backend::RequestNormalizationPolicy::INSIGHT,
    )?;
    assert_eq!(prepared.request.completion.max_tokens, Some(128));
    assert_eq!(prepared.request.completion.temperature, Some(0.0));
    assert!(
        prepared.normalizations.is_empty(),
        "supported OpenAI controls must never be reported as downgraded"
    );
    Ok(())
}

#[tokio::test]
async fn rejects_per_call_model_mismatch_before_starting_transport()
-> Result<(), Box<dyn std::error::Error>> {
    let provider = provider("http://127.0.0.1:1".to_owned());
    let mut mismatched = request();
    mismatched.model = "different-model".to_owned();
    let result = provider.stream(mismatched, CancellationToken::new()).await;
    assert!(
        matches!(result, Err(ProviderError::InvalidRequest(_))),
        "a call cannot change the model represented by the backend fingerprint"
    );
    Ok(())
}

#[test]
fn openai_and_codex_same_model_have_distinct_cache_fingerprints()
-> Result<(), Box<dyn std::error::Error>> {
    let openai = provider("http://127.0.0.1:1".to_owned()).descriptor()?;
    assert!(
        openai.fingerprint().as_str().ends_with(":r3"),
        "opaque screen-authorized transport must use a new OpenAI connector fingerprint"
    );
    let codex = BackendDescriptor::new(
        BackendId::new(CODEX_CLI_BACKEND_ID)?,
        "Codex subscription",
        "mock-model",
        1,
        BackendCapabilities::reasoning_baseline(),
        AuthKind::CodexLogin,
        AuthStatus::Ready,
    )?;
    assert_ne!(
        openai.fingerprint(),
        codex.fingerprint(),
        "connector id must prevent cross-backend cache reuse"
    );
    Ok(())
}

#[test]
fn debug_redacts_api_key() {
    let provider = OpenAiProvider::new(
        "mock-model",
        Some(SecretString::from("do-not-print".to_owned())),
    );
    let debug = format!("{provider:?}");
    assert!(
        !debug.contains("do-not-print"),
        "provider Debug must redact the API key"
    );
}

#[tokio::test]
#[ignore = "requires an OpenAI API key stored in the OS Keychain and spends user tokens"]
async fn live_keychain_recap_cites_timestamped_fixture() -> Result<(), Box<dyn std::error::Error>> {
    let model = std::env::var("SOTTO_OPENAI_MODEL")
        .map_err(|_| "set SOTTO_OPENAI_MODEL to an enabled current Responses model")?;
    let provider = OpenAiProvider::from_keychain(model)?;
    if provider.descriptor()?.auth_status() != &AuthStatus::Ready {
        return Err("store the OpenAI key through Sotto before running this canary".into());
    }
    let mut live_request = request();
    live_request.model = provider.model_id().to_owned();
    live_request.system = Some(
        "Return JSON with recap and citations. Every citation must contain an event_id from the fixture."
            .to_owned(),
    );
    live_request.messages = vec![CompletionMessage {
        role: MessageRole::User,
        content: concat!(
            "[event:7 from:12.000s to:16.000s] [customer] ",
            "\"Security review is the blocker.\"\n",
            "[event:8 from:17.000s to:20.000s] [rep] ",
            "\"I will send the security packet tomorrow.\""
        )
        .to_owned(),
        cache_boundary: false,
    }];
    let mut stream = provider
        .stream(live_request, CancellationToken::new())
        .await?;
    let mut output = String::new();
    while let Some(delta) = stream.next().await {
        output.push_str(&delta?.text);
    }
    let recap: Value = serde_json::from_str(&output)?;
    let citations = recap["citations"]
        .as_array()
        .ok_or("live recap omitted citations")?;
    assert!(
        !citations.is_empty(),
        "live recap must cite fixture evidence"
    );
    assert!(
        citations
            .iter()
            .all(|citation| { matches!(citation["event_id"].as_u64(), Some(7 | 8)) }),
        "every live citation must point to a timestamped fixture event"
    );
    Ok(())
}
