//! Gather query execution tool.
//!
//! Runs pre-defined Gather query definitions and returns results.

use std::collections::HashMap;

use rmcp::ErrorData as McpError;
use rmcp::model::*;

use trovato_kernel::LIVE_STAGE_ID;
use trovato_kernel::gather::types::{FilterValue, QueryContext};
use trovato_kernel::state::AppState;
use trovato_kernel::tap::UserContext;

use crate::server::RunGatherParams;
use crate::tools::{internal_err, require_mcp_permission, to_json, validate_machine_name};

/// Execute a named Gather query.
pub async fn run_gather(
    state: &AppState,
    user_ctx: &UserContext,
    params: RunGatherParams,
) -> Result<CallToolResult, McpError> {
    require_mcp_permission(user_ctx, "access content")?;
    validate_machine_name(&params.query_id, "query_id")?;

    let page = params.page.unwrap_or(1).max(1);

    // Convert JSON filter values to Gather FilterValue types.
    // Unsupported value types (arrays, objects) produce an error.
    let mut exposed_filters: HashMap<String, FilterValue> = HashMap::new();
    for (key, value) in params.filters.unwrap_or_default() {
        let fv = json_to_filter_value(&value).map_err(|type_name| {
            McpError::invalid_params(
                format!("unsupported filter value type for key \"{key}\": {type_name}"),
                None,
            )
        })?;
        exposed_filters.insert(key, fv);
    }

    let context = QueryContext {
        current_user_id: Some(user_ctx.id),
        url_args: HashMap::new(),
    };

    let result = state
        .gather()
        .execute(
            &params.query_id,
            page,
            exposed_filters,
            LIVE_STAGE_ID,
            &context,
        )
        .await
        .map_err(internal_err)?;

    let json = to_json(&result)?;
    Ok(CallToolResult::success(vec![Content::text(json)]))
}

/// Convert a `serde_json::Value` to a Gather `FilterValue`.
///
/// Returns `Err` with the JSON type name for unsupported types (array, object).
fn json_to_filter_value(v: &serde_json::Value) -> Result<FilterValue, &'static str> {
    match v {
        serde_json::Value::String(s) => Ok(FilterValue::String(s.clone())),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(FilterValue::Integer(i))
            } else {
                n.as_f64()
                    .map(FilterValue::Float)
                    .ok_or("unsupported number")
            }
        }
        serde_json::Value::Bool(b) => Ok(FilterValue::Boolean(*b)),
        serde_json::Value::Null => Ok(FilterValue::Null(())),
        serde_json::Value::Array(_) => Err("array"),
        serde_json::Value::Object(_) => Err("object"),
    }
}
