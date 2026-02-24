//! Content type schema resources.
//!
//! - `trovato://content-types` — all content type definitions
//! - `trovato://content-type/{name}` — single content type by machine name

use rmcp::ErrorData as McpError;
use rmcp::model::*;

use trovato_kernel::state::AppState;

use crate::tools::to_json;

/// Read all content type definitions.
pub async fn read_all(state: &AppState) -> Result<ReadResourceResult, McpError> {
    let types = state.content_types().list();
    let json = to_json(&types)?;

    Ok(ReadResourceResult {
        contents: vec![ResourceContents::text(json, "trovato://content-types")],
    })
}

/// Read a single content type definition by machine name.
pub async fn read_one(state: &AppState, name: &str) -> Result<ReadResourceResult, McpError> {
    let ct = state.content_types().get(name).ok_or_else(|| {
        McpError::resource_not_found(format!("content type not found: {name}"), None)
    })?;

    let json = to_json(&ct)?;
    let uri = format!("trovato://content-type/{name}");

    Ok(ReadResourceResult {
        contents: vec![ResourceContents::text(json, uri)],
    })
}
