use std::{
    collections::HashMap,
    error::Error,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use crate::{
    BearerCredential, BoxContextFuture, ContextBudget, ContextBundle, ContextCancellation,
    ContextError, ContextExcerpt, ContextSource, EvidenceId, HttpEndpoint, McpBroker,
    RawResourceContent, ResourceCatalog, ResourceDescriptor, ResourceSelection, ResourceTransport,
    ResourceUri, RmcpResourceTransport, ServerConnection, ServerDescriptor, ServerId,
    SessionContextGrant, SourceReceipt, TransportKind,
};
use rmcp::model::ClientInfo;

type TestResult = Result<(), Box<dyn Error>>;
type BrokerFixture = (McpBroker, Arc<FakeTransport>, ServerId, ResourceUri);

struct FakeTransport {
    descriptor: ServerDescriptor,
    resources: Vec<ResourceDescriptor>,
    reads: Mutex<HashMap<ResourceUri, Vec<RawResourceContent>>>,
    list_calls: AtomicUsize,
    read_calls: AtomicUsize,
    delay: Duration,
    resources_supported: bool,
}

impl FakeTransport {
    fn new(
        descriptor: ServerDescriptor,
        resources: Vec<ResourceDescriptor>,
        reads: HashMap<ResourceUri, Vec<RawResourceContent>>,
    ) -> Self {
        Self {
            descriptor,
            resources,
            reads: Mutex::new(reads),
            list_calls: AtomicUsize::new(0),
            read_calls: AtomicUsize::new(0),
            delay: Duration::ZERO,
            resources_supported: true,
        }
    }

    const fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    const fn without_resource_capability(mut self) -> Self {
        self.resources_supported = false;
        self
    }

    fn calls(&self) -> (usize, usize) {
        (
            self.list_calls.load(Ordering::SeqCst),
            self.read_calls.load(Ordering::SeqCst),
        )
    }
}

impl ResourceTransport for FakeTransport {
    fn descriptor(&self) -> &ServerDescriptor {
        &self.descriptor
    }

    fn list_resources(
        &self,
        _max_pages: usize,
        _max_resources: usize,
        _max_transport_message_bytes: usize,
        cancellation: ContextCancellation,
    ) -> BoxContextFuture<'_, ResourceCatalog> {
        self.list_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            tokio::select! {
                () = cancellation.cancelled() => Err(ContextError::Cancelled),
                () = tokio::time::sleep(self.delay) => Ok(ResourceCatalog {
                    server: self.descriptor.clone(),
                    resources_supported: self.resources_supported,
                    resources: self.resources.clone(),
                    pages: 1,
                }),
            }
        })
    }

    fn read_resource<'a>(
        &'a self,
        uri: &'a ResourceUri,
        _max_transport_message_bytes: usize,
        cancellation: ContextCancellation,
    ) -> BoxContextFuture<'a, Vec<RawResourceContent>> {
        self.read_calls.fetch_add(1, Ordering::SeqCst);
        let uri = uri.clone();
        Box::pin(async move {
            tokio::select! {
                () = cancellation.cancelled() => Err(ContextError::Cancelled),
                () = tokio::time::sleep(self.delay) => self.reads.lock()
                    .map_err(|_| ContextError::Transport(self.descriptor.id.clone()))?
                    .get(&uri)
                    .cloned()
                    .ok_or_else(|| ContextError::Transport(self.descriptor.id.clone())),
            }
        })
    }
}

fn server_id() -> Result<ServerId, crate::ContextGrantError> {
    ServerId::new("docs.primary")
}

fn uri(value: &str) -> Result<ResourceUri, crate::ContextGrantError> {
    ResourceUri::new(value)
}

fn descriptor(server: &ServerId, resource_uri: &ResourceUri) -> ResourceDescriptor {
    ResourceDescriptor {
        server_id: server.clone(),
        uri: resource_uri.clone(),
        name: "project-plan".to_owned(),
        title: Some("Project plan".to_owned()),
        description: Some("Current planning notes".to_owned()),
        mime_type: Some("text/markdown".to_owned()),
        size: None,
        last_modified: Some("2026-08-12T12:00:00Z".to_owned()),
    }
}

fn server_descriptor(server: &ServerId) -> ServerDescriptor {
    ServerDescriptor {
        id: server.clone(),
        display_name: "Project docs".to_owned(),
        transport: TransportKind::StreamableHttp,
    }
}

fn broker_with_text(text: &str) -> Result<BrokerFixture, Box<dyn Error>> {
    let server = server_id()?;
    let resource_uri = uri("docs://project/plan")?;
    let resource = descriptor(&server, &resource_uri);
    let reads = HashMap::from([(
        resource_uri.clone(),
        vec![RawResourceContent::Text {
            uri: resource_uri.clone(),
            mime_type: Some("text/markdown".to_owned()),
            text: text.to_owned(),
        }],
    )]);
    let transport = Arc::new(FakeTransport::new(
        server_descriptor(&server),
        vec![resource],
        reads,
    ));
    let mut broker = McpBroker::new();
    broker.register(transport.clone())?;
    Ok((broker, transport, server, resource_uri))
}

#[tokio::test]
async fn default_grant_contacts_no_server() -> TestResult {
    let (broker, transport, _, _) = broker_with_text("private meeting context")?;
    let bundle = broker
        .resolve(
            &SessionContextGrant::default(),
            ContextBudget::default(),
            ContextCancellation::new(),
        )
        .await?;

    assert!(
        bundle.excerpts().is_empty(),
        "default grant should contain no evidence"
    );
    assert_eq!(
        transport.calls(),
        (0, 0),
        "default grant must perform no MCP I/O"
    );
    assert!(
        !format!("{bundle:?}").contains("private meeting context"),
        "bundle Debug must not reveal source text"
    );
    Ok(())
}

#[tokio::test]
async fn explicit_resource_becomes_citable_provenance() -> TestResult {
    let text = "The migration decision is recorded in ADR-12.";
    let (broker, transport, server, resource_uri) = broker_with_text(text)?;
    let mut grant = SessionContextGrant::new();
    grant.allow_resource(server.clone(), resource_uri.clone())?;

    let first = broker
        .resolve(&grant, ContextBudget::default(), ContextCancellation::new())
        .await?;
    let second = broker
        .resolve(&grant, ContextBudget::default(), ContextCancellation::new())
        .await?;

    assert_eq!(
        transport.calls(),
        (2, 2),
        "each explicit resolution should list then read"
    );
    assert_eq!(
        first.digest(),
        second.digest(),
        "retrieval time must not change cache digest"
    );
    let excerpt = first.excerpts().first().ok_or("missing excerpt")?;
    assert_eq!(
        excerpt.text, text,
        "selected text should be preserved exactly"
    );
    assert_eq!(
        excerpt.receipt.server_id, server,
        "receipt must preserve server identity"
    );
    assert_eq!(
        excerpt.receipt.resource_uri, resource_uri,
        "receipt must preserve resource URI"
    );
    assert!(
        excerpt.evidence_id.as_str().starts_with("mcp-evidence-v1-"),
        "evidence id must be minted by Sotto"
    );
    assert!(
        !excerpt.receipt.truncated,
        "small evidence should not be truncated"
    );
    Ok(())
}

#[tokio::test]
async fn truncation_is_utf8_safe_and_explicit() -> TestResult {
    let (broker, _, server, resource_uri) = broker_with_text("éééé")?;
    let mut grant = SessionContextGrant::new();
    grant.allow_resource(server, resource_uri)?;
    let budget = ContextBudget {
        max_resource_bytes: 5,
        ..ContextBudget::default()
    };
    let bundle = broker
        .resolve(&grant, budget, ContextCancellation::new())
        .await?;
    let excerpt = bundle.excerpts().first().ok_or("missing excerpt")?;

    assert_eq!(
        excerpt.text, "éé",
        "truncation must stop on a UTF-8 boundary"
    );
    assert_eq!(
        excerpt.receipt.original_bytes, 8,
        "receipt must retain original size"
    );
    assert_eq!(
        excerpt.receipt.included_bytes, 4,
        "receipt must retain included size"
    );
    assert!(excerpt.receipt.truncated, "receipt must name truncation");
    Ok(())
}

#[tokio::test]
async fn binary_and_changed_resources_fail_closed() -> TestResult {
    let server = server_id()?;
    let selected = uri("docs://project/plan")?;
    let other = uri("docs://project/other")?;
    let resource = descriptor(&server, &selected);
    let cases = [
        vec![RawResourceContent::Blob {
            uri: selected.clone(),
            mime_type: Some("application/octet-stream".to_owned()),
            encoded_bytes: 24,
        }],
        vec![RawResourceContent::Text {
            uri: other,
            mime_type: Some("text/plain".to_owned()),
            text: "substituted".to_owned(),
        }],
    ];

    for contents in cases {
        let transport = Arc::new(FakeTransport::new(
            server_descriptor(&server),
            vec![resource.clone()],
            HashMap::from([(selected.clone(), contents)]),
        ));
        let mut broker = McpBroker::new();
        broker.register(transport)?;
        let mut grant = SessionContextGrant::new();
        grant.allow_resource(server.clone(), selected.clone())?;
        let result = broker
            .resolve(&grant, ContextBudget::default(), ContextCancellation::new())
            .await;
        assert!(
            matches!(
                result,
                Err(ContextError::BinaryResource { .. } | ContextError::ResourceChanged { .. })
            ),
            "binary or identity-changing content must fail closed"
        );
    }
    Ok(())
}

#[tokio::test]
async fn unsupported_server_and_unknown_resource_are_explicit() -> TestResult {
    let server = server_id()?;
    let selected = uri("docs://project/plan")?;
    let transport = Arc::new(
        FakeTransport::new(server_descriptor(&server), Vec::new(), HashMap::new())
            .without_resource_capability(),
    );
    let mut broker = McpBroker::new();
    broker.register(transport)?;
    let mut grant = SessionContextGrant::new();
    grant.allow_resource(server.clone(), selected)?;
    let result = broker
        .resolve(&grant, ContextBudget::default(), ContextCancellation::new())
        .await;
    assert!(
        matches!(result, Err(ContextError::ResourcesUnsupported(id)) if id == server),
        "missing resource capability should be distinct"
    );

    let (broker, _, server, _) = broker_with_text("known")?;
    let mut grant = SessionContextGrant::new();
    grant.allow_resource(server.clone(), uri("docs://project/missing")?)?;
    let result = broker
        .resolve(&grant, ContextBudget::default(), ContextCancellation::new())
        .await;
    assert!(
        matches!(result, Err(ContextError::UnknownResource { server: id, .. }) if id == server),
        "unadvertised selected resource should be explicit"
    );
    Ok(())
}

#[tokio::test]
async fn timeout_and_cancellation_stop_resolution() -> TestResult {
    let server = server_id()?;
    let resource_uri = uri("docs://project/plan")?;
    let slow = Arc::new(
        FakeTransport::new(
            server_descriptor(&server),
            vec![descriptor(&server, &resource_uri)],
            HashMap::new(),
        )
        .with_delay(Duration::from_secs(60)),
    );
    let mut broker = McpBroker::new();
    broker.register(slow)?;
    let mut grant = SessionContextGrant::new();
    grant.allow_resource(server.clone(), resource_uri)?;
    let timeout_budget = ContextBudget {
        call_timeout: Duration::from_millis(5),
        ..ContextBudget::default()
    };
    let result = broker
        .resolve(&grant, timeout_budget, ContextCancellation::new())
        .await;
    assert!(
        matches!(result, Err(ContextError::TimedOut(id)) if id == server),
        "slow server should time out with server identity only"
    );

    let cancellation = ContextCancellation::new();
    cancellation.cancel();
    let result = broker
        .resolve(&grant, ContextBudget::default(), cancellation)
        .await;
    assert!(
        matches!(result, Err(ContextError::Cancelled)),
        "cancelled call should not run"
    );
    Ok(())
}

#[test]
fn connection_configuration_is_explicit_and_safe() -> TestResult {
    assert!(
        HttpEndpoint::new("https://mcp.example.test/context").is_ok(),
        "HTTPS should be accepted"
    );
    assert!(
        HttpEndpoint::new("http://127.0.0.1:8787/mcp").is_ok(),
        "loopback HTTP should be accepted"
    );
    assert!(
        matches!(
            HttpEndpoint::new_remote("http://127.0.0.1:8787/mcp"),
            Err(ContextError::InsecureEndpoint)
        ),
        "product configuration must require HTTPS even for loopback"
    );
    assert!(
        matches!(
            HttpEndpoint::new("http://mcp.example.test"),
            Err(ContextError::InsecureEndpoint)
        ),
        "non-loopback HTTP must fail"
    );
    assert!(
        matches!(
            HttpEndpoint::new("https://user:secret@mcp.example.test"),
            Err(ContextError::UnsafeEndpoint)
        ),
        "embedded endpoint credentials must fail without echo"
    );
    assert!(
        matches!(
            HttpEndpoint::new("https://mcp.example.test?token=secret"),
            Err(ContextError::UnsafeEndpoint)
        ),
        "query credentials must fail without echo"
    );
    Ok(())
}

#[test]
fn immutable_run_fingerprint_covers_endpoint_resources_and_named_disclosure() -> TestResult {
    let server = server_id()?;
    let descriptor = server_descriptor(&server);
    let first_endpoint = HttpEndpoint::new("https://one.example.test/mcp")?;
    let second_endpoint = HttpEndpoint::new("https://two.example.test/mcp")?;
    let mut grant = SessionContextGrant::new();
    grant.allow_resource(server.clone(), uri("docs://project/plan")?)?;

    let mut first_broker = McpBroker::new();
    first_broker.register(Arc::new(RmcpResourceTransport::new(
        descriptor.clone(),
        ServerConnection::StreamableHttp(first_endpoint),
    )?))?;
    let baseline = first_broker.run_fingerprint(&grant)?;
    grant.set_query_disclosure(server.clone(), crate::MeetingQueryDisclosure::Redacted);
    let disclosure = first_broker.run_fingerprint(&grant)?;
    assert_ne!(baseline, disclosure);

    let mut second_broker = McpBroker::new();
    second_broker.register(Arc::new(RmcpResourceTransport::new(
        descriptor,
        ServerConnection::StreamableHttp(second_endpoint),
    )?))?;
    assert_ne!(disclosure, second_broker.run_fingerprint(&grant)?);

    let mut other_resource = SessionContextGrant::new();
    other_resource.allow_resource(server.clone(), uri("docs://project/other")?)?;
    assert_ne!(baseline, first_broker.run_fingerprint(&other_resource)?);
    assert_eq!(
        grant.query_disclosure(&server),
        crate::MeetingQueryDisclosure::Redacted
    );
    assert!(baseline.as_str().starts_with("mcp-grant-v1-"));
    Ok(())
}

#[test]
fn bearer_credential_is_opaque_in_public_diagnostics() -> TestResult {
    let secret = "must-never-appear-in-mcp-debug";
    let credential = BearerCredential::new(secrecy::SecretString::from(secret.to_owned()));
    assert!(!format!("{credential:?}").contains(secret));
    let transport = RmcpResourceTransport::new_authenticated(
        server_descriptor(&server_id()?),
        ServerConnection::StreamableHttp(HttpEndpoint::new("https://mcp.example.test")?),
        credential,
    )?;
    assert!(!format!("{:?}", transport.descriptor()).contains(secret));
    Ok(())
}

#[test]
fn ids_and_grants_fail_closed() -> TestResult {
    assert!(
        ServerId::new("UPPER").is_err(),
        "server id should be canonical"
    );
    assert!(
        ResourceUri::new("https://user:secret@example.test/doc").is_err(),
        "resource URI credentials should fail"
    );
    assert!(
        ResourceUri::new("https://example.test/doc?token=secret").is_err(),
        "network resource URI queries should fail"
    );
    assert!(
        ResourceUri::new("docs://project/plan#secret").is_err(),
        "custom resource URI fragments should fail"
    );
    assert!(
        ResourceUri::new("docs://project/plan?token=secret").is_err(),
        "custom resource URI queries should fail"
    );
    let server = server_id()?;
    let selected = uri("docs://project/plan")?;
    let mut grant = SessionContextGrant::new();
    grant.allow_resource(server.clone(), selected.clone())?;
    assert!(
        grant.allow_resource(server, selected).is_err(),
        "duplicate selection should fail"
    );
    assert_eq!(
        grant.query_disclosure(&server_id()?),
        crate::MeetingQueryDisclosure::None,
        "meeting query disclosure must default to none"
    );
    Ok(())
}

#[test]
fn public_debug_surfaces_redact_server_controlled_values() -> TestResult {
    const CANARY: &str = "tokenleakcanary";
    let server = ServerId::new(CANARY)?;
    let resource_uri = ResourceUri::new(format!("docs://project/{CANARY}"))?;
    let server_descriptor = ServerDescriptor {
        id: server.clone(),
        display_name: CANARY.to_owned(),
        transport: TransportKind::StreamableHttp,
    };
    let resource_descriptor = ResourceDescriptor {
        server_id: server.clone(),
        uri: resource_uri.clone(),
        name: CANARY.to_owned(),
        title: Some(CANARY.to_owned()),
        description: Some(CANARY.to_owned()),
        mime_type: Some(format!("text/{CANARY}")),
        size: Some(12),
        last_modified: Some(CANARY.to_owned()),
    };
    let receipt = SourceReceipt {
        server_id: server.clone(),
        resource_uri: resource_uri.clone(),
        content_sha256: "00".repeat(32),
        retrieved_at_unix_ms: 1,
        original_bytes: CANARY.len(),
        included_bytes: CANARY.len(),
        truncated: false,
        last_modified: Some(CANARY.to_owned()),
    };
    let excerpt = ContextExcerpt {
        evidence_id: EvidenceId::from_digest(&"11".repeat(32)),
        title: CANARY.to_owned(),
        text: CANARY.to_owned(),
        receipt: receipt.clone(),
    };
    let selection = ResourceSelection {
        server_id: server.clone(),
        uri: resource_uri.clone(),
    };
    let mut grant = SessionContextGrant::new();
    grant.allow_resource(server.clone(), resource_uri.clone())?;
    let catalog = ResourceCatalog {
        server: server_descriptor.clone(),
        resources_supported: true,
        resources: vec![resource_descriptor.clone()],
        pages: 1,
    };
    let content = RawResourceContent::Text {
        uri: resource_uri,
        mime_type: Some(format!("text/{CANARY}")),
        text: CANARY.to_owned(),
    };
    let endpoint = HttpEndpoint::new(&format!("https://{CANARY}.example/mcp"))?;

    for debug in [
        format!("{server:?}"),
        format!("{server}"),
        format!("{server_descriptor:?}"),
        format!("{resource_descriptor:?}"),
        format!("{receipt:?}"),
        format!("{excerpt:?}"),
        format!("{selection:?}"),
        format!("{grant:?}"),
        format!("{catalog:?}"),
        format!("{content:?}"),
        format!("{endpoint:?}"),
    ] {
        assert!(
            !debug.contains(CANARY),
            "public Debug or error-facing Display leaked token canary: {debug}"
        );
    }
    Ok(())
}

#[test]
fn sdk_client_advertises_no_server_initiated_capabilities() {
    let client = ClientInfo::default();
    assert!(
        client.capabilities.roots.is_none(),
        "read-only client must not advertise roots"
    );
    assert!(
        client.capabilities.sampling.is_none(),
        "read-only client must not advertise sampling"
    );
    assert!(
        client.capabilities.elicitation.is_none(),
        "read-only client must not advertise elicitation"
    );
    assert!(
        client.capabilities.extensions.is_none(),
        "read-only client must not advertise extensions"
    );
}

#[tokio::test]
async fn grant_and_transport_size_budgets_fail_before_prompt_context() -> TestResult {
    let (broker, _, server, resource_uri) = broker_with_text("123456789")?;
    let mut grant = SessionContextGrant::new();
    grant.allow_resource(server, resource_uri)?;
    let result = broker
        .resolve(
            &grant,
            ContextBudget {
                max_transport_message_bytes: 8,
                ..ContextBudget::default()
            },
            ContextCancellation::new(),
        )
        .await;
    assert!(
        matches!(
            result,
            Err(ContextError::BudgetExceeded {
                dimension: "transport resource bytes",
                ..
            })
        ),
        "decoded resource larger than transport policy must fail"
    );

    let (broker, _, server, first) = broker_with_text("small")?;
    let mut grant = SessionContextGrant::new();
    grant.allow_resource(server.clone(), first)?;
    grant.allow_resource(server, uri("docs://project/second")?)?;
    let result = broker
        .resolve(
            &grant,
            ContextBudget {
                max_selected_resources: 1,
                ..ContextBudget::default()
            },
            ContextCancellation::new(),
        )
        .await;
    assert!(
        matches!(
            result,
            Err(ContextError::BudgetExceeded {
                dimension: "selected resources",
                ..
            })
        ),
        "selection budget must be checked before MCP I/O"
    );
    Ok(())
}

#[tokio::test]
async fn durable_bundle_integrity_rejects_every_identity_mismatch() -> TestResult {
    let (broker, _, server, resource_uri) = broker_with_text("trusted evidence")?;
    let mut grant = SessionContextGrant::new();
    grant.allow_resource(server, resource_uri)?;
    let bundle = broker
        .resolve(&grant, ContextBudget::default(), ContextCancellation::new())
        .await?;
    bundle.validate_integrity()?;

    let mut later_value = serde_json::to_value(&bundle)?;
    later_value["excerpts"][0]["receipt"]["retrieved_at_unix_ms"] =
        serde_json::json!(9_999_999_u64);
    let later: ContextBundle = serde_json::from_value(later_value)?;
    assert_eq!(
        later.digest(),
        bundle.digest(),
        "retrieval wall clock must not change cache identity"
    );
    assert!(matches!(
        later.validate_integrity(),
        Err(ContextError::InvalidBundle)
    ));

    for (field, replacement) in [
        ("digest", serde_json::json!("00".repeat(32))),
        ("estimated_tokens", serde_json::json!(999)),
    ] {
        let mut value = serde_json::to_value(&bundle)?;
        value[field] = replacement;
        let corrupted: ContextBundle = serde_json::from_value(value)?;
        assert!(matches!(
            corrupted.validate_integrity(),
            Err(ContextError::InvalidBundle)
        ));
    }

    for (field, replacement) in [
        ("evidence_id", serde_json::json!("mcp-evidence-v1-bogus")),
        ("title", serde_json::json!("changed title")),
        ("text", serde_json::json!("changed")),
    ] {
        let mut value = serde_json::to_value(&bundle)?;
        value["excerpts"][0][field] = replacement;
        let corrupted: ContextBundle = serde_json::from_value(value)?;
        assert!(matches!(
            corrupted.validate_integrity(),
            Err(ContextError::InvalidBundle)
        ));
    }

    for (field, replacement) in [
        ("server_id", serde_json::json!("other.server")),
        ("resource_uri", serde_json::json!("docs://project/other")),
        ("content_sha256", serde_json::json!("11".repeat(32))),
        ("retrieved_at_unix_ms", serde_json::json!(0)),
        ("original_bytes", serde_json::json!(999)),
        ("included_bytes", serde_json::json!(1)),
        ("truncated", serde_json::json!(true)),
        ("last_modified", serde_json::json!("changed")),
    ] {
        let mut value = serde_json::to_value(&bundle)?;
        value["excerpts"][0]["receipt"][field] = replacement;
        let corrupted: ContextBundle = serde_json::from_value(value)?;
        assert!(matches!(
            corrupted.validate_integrity(),
            Err(ContextError::InvalidBundle)
        ));
    }

    let mut duplicate_value = serde_json::to_value(&bundle)?;
    let duplicate = duplicate_value["excerpts"][0].clone();
    duplicate_value["excerpts"]
        .as_array_mut()
        .ok_or("serialized excerpts were not an array")?
        .push(duplicate);
    let duplicate: ContextBundle = serde_json::from_value(duplicate_value)?;
    assert!(matches!(
        duplicate.validate_integrity(),
        Err(ContextError::InvalidBundle)
    ));

    let (broker, _, server, resource_uri) = broker_with_text("trusted evidence")?;
    let mut grant = SessionContextGrant::new();
    grant.allow_resource(server, resource_uri)?;
    let truncated = broker
        .resolve(
            &grant,
            ContextBudget {
                max_resource_bytes: 7,
                ..ContextBudget::default()
            },
            ContextCancellation::new(),
        )
        .await?;
    assert!(truncated.excerpts()[0].receipt.truncated);
    truncated.validate_integrity()?;
    let mut changed = serde_json::to_value(&truncated)?;
    changed["excerpts"][0]["text"] = serde_json::json!("changed");
    let changed: ContextBundle = serde_json::from_value(changed)?;
    assert!(matches!(
        changed.validate_integrity(),
        Err(ContextError::InvalidBundle)
    ));
    Ok(())
}
