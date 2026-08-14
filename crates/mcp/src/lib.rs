//! Application-controlled MCP resources for meeting context.
//!
//! Sotto, rather than a reasoning model, selects and reads resources. The public
//! API deliberately contains no `rmcp` values and exposes no prompts, tools,
//! sampling, roots, subscriptions, or elicitation surface.

#![deny(warnings)]

mod broker;
mod connection;
mod http_client;
mod transport;
mod types;

pub use broker::{ContextSource, McpBroker};
pub use connection::{BearerCredential, HttpEndpoint, RmcpResourceTransport, ServerConnection};
pub use transport::{BoxContextFuture, RawResourceContent, ResourceCatalog, ResourceTransport};
pub use types::{
    ContextBudget, ContextBundle, ContextCancellation, ContextError, ContextExcerpt,
    ContextGrantError, ContextResult, EvidenceId, GrantRunFingerprint, MeetingQueryDisclosure,
    ResourceDescriptor, ResourceSelection, ResourceUri, ServerDescriptor, ServerId,
    SessionContextGrant, SourceReceipt, TransportKind,
};

#[cfg(test)]
mod tests;
