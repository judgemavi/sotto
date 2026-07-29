//! Local SQLite persistence and hybrid retrieval.
//!
//! `fastembed` downloads model weights on first use and caches them; weights are not
//! bundled into Sotto. The model is loaded lazily and can be explicitly unloaded.

#![deny(warnings)]

mod schema;
mod store;

pub use store::{DocumentKind, IngestMetadata, SearchFilter, Store};
