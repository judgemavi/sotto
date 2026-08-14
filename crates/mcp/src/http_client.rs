use std::{collections::HashMap, sync::Arc};

use futures_util::{StreamExt, stream::BoxStream};
use http::{HeaderName, HeaderValue};
use reqwest::{Response, header::ACCEPT};
use rmcp::{
    model::ClientJsonRpcMessage,
    transport::streamable_http_client::{
        StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
    },
};
use sse_stream::{Error as SseError, Sse, SseStream};

const EVENT_STREAM_MIME_TYPE: &str = "text/event-stream";
const JSON_MIME_TYPE: &str = "application/json";
const HEADER_SESSION_ID: &str = "Mcp-Session-Id";
const HEADER_LAST_EVENT_ID: &str = "Last-Event-Id";
const DEFAULT_MAX_RESPONSE_BYTES: usize = 16 * 1_024 * 1_024;

/// Streamable HTTP backend that accepts only bounded SSE response bodies.
///
/// rmcp's built-in reqwest backend bounds SSE events but parses JSON response
/// bodies with `Response::json`, which has no allocation limit. T039 therefore
/// supplies this stricter backend: JSON responses are rejected before their
/// bodies are read and every accepted SSE response is capped before parsing.
#[derive(Clone)]
pub(crate) struct BoundedHttpClient(reqwest::Client);

impl BoundedHttpClient {
    pub(crate) fn new() -> Result<Self, reqwest::Error> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map(Self)
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum BoundedHttpError {
    #[error("HTTP request failed")]
    Request(#[source] reqwest::Error),
    #[error("HTTP response exceeded its configured byte budget")]
    ResponseTooLarge,
}

impl StreamableHttpClient for BoundedHttpClient {
    type Error = BoundedHttpError;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        self.post_message_with_max_sse_event_size(
            uri,
            message,
            session_id,
            auth_header,
            custom_headers,
            DEFAULT_MAX_RESPONSE_BYTES,
        )
        .await
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_response_bytes: usize,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let mut request = self
            .0
            .post(uri.as_ref())
            .header(ACCEPT, [EVENT_STREAM_MIME_TYPE, JSON_MIME_TYPE].join(", "))
            .json(&message);
        if let Some(session_id) = &session_id {
            request = request.header(HEADER_SESSION_ID, session_id.as_ref());
        }
        if let Some(token) = auth_header {
            request = request.bearer_auth(token);
        }
        request = apply_custom_headers(request, custom_headers)?;
        let response = request.send().await.map_err(request_error)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND && session_id.is_some() {
            return Err(StreamableHttpError::SessionExpired);
        }
        if matches!(
            response.status(),
            reqwest::StatusCode::ACCEPTED | reqwest::StatusCode::NO_CONTENT
        ) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        let response = response.error_for_status().map_err(request_error)?;
        ensure_sse(&response)?;
        let response_session_id = response
            .headers()
            .get(HEADER_SESSION_ID)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let stream = bounded_sse(response, max_response_bytes)?;
        Ok(StreamableHttpPostResponse::Sse(stream, response_session_id))
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        let mut request = self
            .0
            .delete(uri.as_ref())
            .header(HEADER_SESSION_ID, session_id.as_ref());
        if let Some(token) = auth_header {
            request = request.bearer_auth(token);
        }
        request = apply_custom_headers(request, custom_headers)?;
        let response = request.send().await.map_err(request_error)?;
        if response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
            return Ok(());
        }
        response.error_for_status().map_err(request_error)?;
        Ok(())
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        self.get_stream_with_max_sse_event_size(
            uri,
            session_id,
            last_event_id,
            auth_header,
            custom_headers,
            DEFAULT_MAX_RESPONSE_BYTES,
        )
        .await
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        max_response_bytes: usize,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        let mut request = self
            .0
            .get(uri.as_ref())
            .header(ACCEPT, EVENT_STREAM_MIME_TYPE);
        if let Some(session_id) = session_id {
            request = request.header(HEADER_SESSION_ID, session_id.as_ref());
        }
        if let Some(last_event_id) = last_event_id {
            request = request.header(HEADER_LAST_EVENT_ID, last_event_id);
        }
        if let Some(token) = auth_header {
            request = request.bearer_auth(token);
        }
        request = apply_custom_headers(request, custom_headers)?;
        let response = request.send().await.map_err(request_error)?;
        if response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        let response = response.error_for_status().map_err(request_error)?;
        ensure_sse(&response)?;
        bounded_sse(response, max_response_bytes)
    }
}

fn apply_custom_headers(
    mut request: reqwest::RequestBuilder,
    headers: HashMap<HeaderName, HeaderValue>,
) -> Result<reqwest::RequestBuilder, StreamableHttpError<BoundedHttpError>> {
    for (name, value) in headers {
        if ["accept", HEADER_SESSION_ID, HEADER_LAST_EVENT_ID]
            .iter()
            .any(|reserved| name.as_str().eq_ignore_ascii_case(reserved))
        {
            return Err(StreamableHttpError::ReservedHeaderConflict(
                name.to_string(),
            ));
        }
        request = request.header(name, value);
    }
    Ok(request)
}

fn ensure_sse(response: &Response) -> Result<(), StreamableHttpError<BoundedHttpError>> {
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
    if content_type.as_deref().is_some_and(|value| {
        value
            .as_bytes()
            .starts_with(EVENT_STREAM_MIME_TYPE.as_bytes())
    }) {
        Ok(())
    } else {
        Err(StreamableHttpError::UnexpectedContentType(content_type))
    }
}

fn bounded_sse(
    response: Response,
    limit: usize,
) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<BoundedHttpError>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(StreamableHttpError::Client(
            BoundedHttpError::ResponseTooLarge,
        ));
    }
    let mut received = 0_usize;
    let stream = response.bytes_stream().map(move |result| {
        let bytes = result.map_err(BoundedHttpError::Request)?;
        received = received.saturating_add(bytes.len());
        if received > limit {
            Err(BoundedHttpError::ResponseTooLarge)
        } else {
            Ok(bytes)
        }
    });
    Ok(SseStream::from_bytes_stream(stream).boxed())
}

fn request_error(error: reqwest::Error) -> StreamableHttpError<BoundedHttpError> {
    StreamableHttpError::Client(BoundedHttpError::Request(error))
}
