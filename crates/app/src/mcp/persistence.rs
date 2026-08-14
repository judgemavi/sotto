//! Identifier-only MCP settings. Credentials are deliberately absent.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use serde::{Deserialize, Serialize};

use super::McpUiError;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct PersistedMcpSettings {
    pub version: u32,
    pub servers: Vec<PersistedServer>,
    pub grants: Vec<PersistedGrant>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct PersistedServer {
    pub id: String,
    pub display_name: String,
    pub endpoint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct PersistedGrant {
    pub session_id: u128,
    pub resources: Vec<PersistedResource>,
    pub query_disclosure_servers: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct PersistedResource {
    pub server_id: String,
    pub uri: String,
}

pub(super) fn load(path: &Path) -> Result<Option<PersistedMcpSettings>, McpUiError> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| McpUiError::Persistence(format!("invalid MCP settings: {error}"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(McpUiError::Persistence(format!(
            "could not read MCP settings: {error}"
        ))),
    }
}

pub(super) fn save(path: &Path, settings: &PersistedMcpSettings) -> Result<(), McpUiError> {
    let parent = path
        .parent()
        .ok_or_else(|| McpUiError::Persistence("MCP settings path has no parent".to_owned()))?;
    fs::create_dir_all(parent)
        .map_err(|error| McpUiError::Persistence(format!("could not create settings: {error}")))?;
    let temporary = parent.join(format!(".mcp-settings-{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| {
                McpUiError::Persistence(format!("could not create settings: {error}"))
            })?;
        let bytes = serde_json::to_vec_pretty(settings).map_err(|error| {
            McpUiError::Persistence(format!("could not encode settings: {error}"))
        })?;
        file.write_all(&bytes).map_err(|error| {
            McpUiError::Persistence(format!("could not write settings: {error}"))
        })?;
        file.sync_all().map_err(|error| {
            McpUiError::Persistence(format!("could not sync settings: {error}"))
        })?;
        fs::rename(&temporary, path).map_err(|error| {
            McpUiError::Persistence(format!("could not replace settings: {error}"))
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
