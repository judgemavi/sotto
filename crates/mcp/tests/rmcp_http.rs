#![expect(
    clippy::tests_outside_test_module,
    reason = "integration test compiles only under cfg(test)"
)]

use std::{
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use mcp::{
    BearerCredential, ContextBudget, ContextCancellation, ContextError, ContextSource,
    HttpEndpoint, McpBroker, ResourceUri, RmcpResourceTransport, ServerConnection,
    ServerDescriptor, ServerId, SessionContextGrant, TransportKind,
};
use rmcp::{
    ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, ErrorData, InputRequiredResult,
        ListResourcesResult, PaginatedRequestParams, ReadResourceRequestParams,
        ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, ServerCapabilities,
        ServerInfo,
    },
    service::{RequestContext, RoleServer},
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use secrecy::SecretString;
use tokio_util::sync::CancellationToken;

type TestResult = Result<(), Box<dyn Error>>;

const RESOURCE_URI: &str = "docs://meeting/decision-log";
const RESOURCE_TEXT: &str = "Decision: ship the read-only context plane first.";

#[derive(Clone, Default)]
struct FixtureServer {
    list_calls: Arc<AtomicUsize>,
    read_calls: Arc<AtomicUsize>,
    tool_calls: Arc<AtomicUsize>,
    input_required: Arc<AtomicBool>,
}

impl ServerHandler for FixtureServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_resources().build())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        self.list_calls.fetch_add(1, Ordering::SeqCst);
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(RESOURCE_URI, "decision-log")
                .with_title("Meeting decision log")
                .with_mime_type("text/markdown"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        self.read_calls.fetch_add(1, Ordering::SeqCst);
        if request.uri != RESOURCE_URI {
            return Err(ErrorData::resource_not_found("not found", None));
        }
        if self.input_required.load(Ordering::SeqCst) {
            return Ok(InputRequiredResult::from_request_state("server-secret-state").into());
        }
        Ok(
            ReadResourceResult::new(vec![ResourceContents::text(RESOURCE_TEXT, RESOURCE_URI)])
                .into(),
        )
    }

    async fn call_tool(
        &self,
        _request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.tool_calls.fetch_add(1, Ordering::SeqCst);
        Err(ErrorData::internal_error(
            "tool surface must stay unreachable",
            None,
        ))
    }
}

#[tokio::test]
async fn official_http_sdk_lists_and_reads_without_tools() -> TestResult {
    let fixture = FixtureServer::default();
    let cancellation = CancellationToken::new();
    let fixture_for_service = fixture.clone();
    let service: StreamableHttpService<FixtureServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(fixture_for_service.clone()),
            Default::default(),
            StreamableHttpServerConfig::default()
                .with_legacy_session_mode(false)
                .with_json_response(false)
                .with_cancellation_token(cancellation.child_token()),
        );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server_cancellation = cancellation.clone();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { server_cancellation.cancelled_owned().await })
            .await;
    });

    let server_id = ServerId::new("fixture.http")?;
    let descriptor = ServerDescriptor {
        id: server_id.clone(),
        display_name: "Fixture HTTP".to_owned(),
        transport: TransportKind::StreamableHttp,
    };
    let endpoint = HttpEndpoint::new(&format!("http://{address}/mcp"))?;
    let transport =
        RmcpResourceTransport::new(descriptor, ServerConnection::StreamableHttp(endpoint))?;
    let mut broker = McpBroker::new();
    broker.register(Arc::new(transport))?;
    let mut grant = SessionContextGrant::new();
    grant.allow_resource(server_id, ResourceUri::new(RESOURCE_URI)?)?;

    let bundle = broker
        .resolve(&grant, ContextBudget::default(), ContextCancellation::new())
        .await?;

    assert_eq!(
        bundle
            .excerpts()
            .first()
            .map(|excerpt| excerpt.text.as_str()),
        Some(RESOURCE_TEXT),
        "official SDK transport should normalize exact resource text"
    );
    assert_eq!(
        fixture.list_calls.load(Ordering::SeqCst),
        1,
        "broker should issue exactly one resources/list request"
    );
    assert_eq!(
        fixture.read_calls.load(Ordering::SeqCst),
        1,
        "broker should issue exactly one resources/read request"
    );
    assert_eq!(
        fixture.tool_calls.load(Ordering::SeqCst),
        0,
        "resource resolution must never invoke tools"
    );

    cancellation.cancel();
    server.await?;
    Ok(())
}

#[tokio::test]
async fn official_http_sdk_rejects_input_required_without_follow_up() -> TestResult {
    let fixture = FixtureServer::default();
    fixture.input_required.store(true, Ordering::SeqCst);
    let cancellation = CancellationToken::new();
    let fixture_for_service = fixture.clone();
    let service: StreamableHttpService<FixtureServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(fixture_for_service.clone()),
            Default::default(),
            StreamableHttpServerConfig::default()
                .with_legacy_session_mode(false)
                .with_json_response(false)
                .with_cancellation_token(cancellation.child_token()),
        );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server_cancellation = cancellation.clone();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { server_cancellation.cancelled_owned().await })
            .await;
    });

    let server_id = ServerId::new("fixture.input")?;
    let descriptor = ServerDescriptor {
        id: server_id.clone(),
        display_name: "Input-required fixture".to_owned(),
        transport: TransportKind::StreamableHttp,
    };
    let endpoint = HttpEndpoint::new(&format!("http://{address}/mcp"))?;
    let transport =
        RmcpResourceTransport::new(descriptor, ServerConnection::StreamableHttp(endpoint))?;
    let mut broker = McpBroker::new();
    broker.register(Arc::new(transport))?;
    let mut grant = SessionContextGrant::new();
    grant.allow_resource(server_id, ResourceUri::new(RESOURCE_URI)?)?;

    let result = broker
        .resolve(&grant, ContextBudget::default(), ContextCancellation::new())
        .await;
    assert!(
        matches!(result, Err(ContextError::InteractiveRequestRejected)),
        "input-required must fail closed instead of eliciting or retrying"
    );
    assert_eq!(fixture.read_calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.tool_calls.load(Ordering::SeqCst), 0);

    cancellation.cancel();
    server.await?;
    Ok(())
}

#[tokio::test]
async fn official_http_sdk_bounds_protocol_messages_before_catalog_allocation() -> TestResult {
    let fixture = FixtureServer::default();
    let cancellation = CancellationToken::new();
    let fixture_for_service = fixture.clone();
    let service: StreamableHttpService<FixtureServer, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(fixture_for_service.clone()),
            Default::default(),
            StreamableHttpServerConfig::default()
                .with_legacy_session_mode(false)
                .with_json_response(false)
                .with_cancellation_token(cancellation.child_token()),
        );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server_cancellation = cancellation.clone();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { server_cancellation.cancelled_owned().await })
            .await;
    });

    let server_id = ServerId::new("fixture.bounded")?;
    let descriptor = ServerDescriptor {
        id: server_id.clone(),
        display_name: "Bounded fixture".to_owned(),
        transport: TransportKind::StreamableHttp,
    };
    let endpoint = HttpEndpoint::new(&format!("http://{address}/mcp"))?;
    let transport =
        RmcpResourceTransport::new(descriptor, ServerConnection::StreamableHttp(endpoint))?;
    let mut broker = McpBroker::new();
    broker.register(Arc::new(transport))?;

    let result = broker
        .discover_resources(
            &server_id,
            ContextBudget {
                max_transport_message_bytes: 64,
                ..ContextBudget::default()
            },
            ContextCancellation::new(),
        )
        .await;
    assert!(
        matches!(result, Err(ContextError::Transport(id)) if id == server_id),
        "oversized initialize protocol response must fail before catalog decoding"
    );
    assert_eq!(fixture.list_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.tool_calls.load(Ordering::SeqCst), 0);

    cancellation.cancel();
    server.await?;
    Ok(())
}

#[tokio::test]
async fn authenticated_transport_never_follows_redirects_or_forwards_bearer() -> TestResult {
    let target_calls = Arc::new(AtomicUsize::new(0));
    let target_saw_auth = Arc::new(AtomicBool::new(false));
    let calls = target_calls.clone();
    let saw_auth = target_saw_auth.clone();
    let target = axum::Router::new().fallback(move |headers: axum::http::HeaderMap| {
        let calls = calls.clone();
        let saw_auth = saw_auth.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            saw_auth.store(
                headers.contains_key(axum::http::header::AUTHORIZATION),
                Ordering::SeqCst,
            );
            axum::http::StatusCode::NO_CONTENT
        }
    });
    let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let target_address = target_listener.local_addr()?;
    let target_server = tokio::spawn(async move {
        let _ = axum::serve(target_listener, target).await;
    });

    let location = format!("http://{target_address}/stolen");
    let redirect = axum::Router::new().fallback(move || {
        let location = location.clone();
        async move { axum::response::Redirect::temporary(&location) }
    });
    let redirect_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let redirect_address = redirect_listener.local_addr()?;
    let redirect_server = tokio::spawn(async move {
        let _ = axum::serve(redirect_listener, redirect).await;
    });

    let server_id = ServerId::new("fixture.redirect")?;
    let descriptor = ServerDescriptor {
        id: server_id.clone(),
        display_name: "Redirect fixture".to_owned(),
        transport: TransportKind::StreamableHttp,
    };
    let endpoint = HttpEndpoint::new(&format!("http://{redirect_address}/mcp"))?;
    let transport = RmcpResourceTransport::new_authenticated(
        descriptor,
        ServerConnection::StreamableHttp(endpoint),
        BearerCredential::new(SecretString::from("redirect-canary".to_owned())),
    )?;
    let mut broker = McpBroker::new();
    broker.register(Arc::new(transport))?;

    let result = broker
        .discover_resources(
            &server_id,
            ContextBudget::default(),
            ContextCancellation::new(),
        )
        .await;

    assert!(matches!(result, Err(ContextError::Transport(id)) if id == server_id));
    assert_eq!(target_calls.load(Ordering::SeqCst), 0);
    assert!(!target_saw_auth.load(Ordering::SeqCst));
    redirect_server.abort();
    target_server.abort();
    Ok(())
}
