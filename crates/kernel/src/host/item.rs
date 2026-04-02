//! Item host functions for WASM plugins.
//!
//! Provides CRUD operations for items (content records) via direct
//! `Item` model queries. Uses the model directly (not `ItemService`)
//! to avoid re-entrant tap dispatch when a plugin calls save_item
//! from within a tap handler.

use anyhow::Result;
use tracing::warn;
use trovato_sdk::host_errors;
use uuid::Uuid;
use wasmtime::Linker;

use super::{read_string_from_memory, write_string_to_memory};
use crate::models::{CreateItem, Item, UpdateItem};
use crate::plugin::PluginState;

/// Register item host functions.
pub fn register_item_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // get-item(id, out) -> i32 (bytes written or error)
    linker.func_wrap_async(
        "trovato:kernel/item-api",
        "get-item",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         (id_ptr, id_len, out_ptr, out_max_len): (i32, i32, i32, i32)| {
            Box::new(async move {
                let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return host_errors::ERR_MEMORY_MISSING;
                };

                let Ok(id_str) = read_string_from_memory(&memory, &caller, id_ptr, id_len) else {
                    return host_errors::ERR_PARAM1_READ;
                };

                let Ok(id) = id_str.parse::<Uuid>() else {
                    return host_errors::ERR_PARAM1_READ;
                };

                let Some(services) = caller.data().request.services() else {
                    return host_errors::ERR_NO_SERVICES;
                };
                let pool = services.db.clone();

                match Item::find_by_id(&pool, id).await {
                    Ok(Some(item)) => match serde_json::to_string(&item) {
                        Ok(json) => write_string_to_memory(
                            &memory,
                            &mut caller,
                            out_ptr,
                            out_max_len,
                            &json,
                        )
                        .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT),
                        Err(_) => host_errors::ERR_SERIALIZE_FAILED,
                    },
                    Ok(None) => {
                        // Item not found — write empty JSON object
                        write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, "null")
                            .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT)
                    }
                    Err(e) => {
                        warn!(item_id = %id, error = %e, "get-item host function failed");
                        host_errors::ERR_SQL_FAILED
                    }
                }
            })
        },
    )?;

    // save-item(item_json, out) -> i32 (bytes written or error)
    linker.func_wrap_async(
        "trovato:kernel/item-api",
        "save-item",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         (item_ptr, item_len, out_ptr, out_max_len): (i32, i32, i32, i32)| {
            Box::new(async move {
                let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return host_errors::ERR_MEMORY_MISSING;
                };

                let Ok(item_json) = read_string_from_memory(&memory, &caller, item_ptr, item_len)
                else {
                    return host_errors::ERR_PARAM1_READ;
                };

                let Some(services) = caller.data().request.services() else {
                    return host_errors::ERR_NO_SERVICES;
                };
                let pool = services.db.clone();
                let user_id = caller.data().request.user.id;

                // Parse the item JSON to determine create vs update
                let parsed: serde_json::Value = match serde_json::from_str(&item_json) {
                    Ok(v) => v,
                    Err(_) => return host_errors::ERR_PARAM_DESERIALIZE,
                };

                // If the JSON has an "id" field with a valid non-nil UUID, it's an update
                let existing_id = parsed
                    .get("id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<Uuid>().ok())
                    .filter(|id| !id.is_nil());

                let result = if let Some(id) = existing_id {
                    // Update existing item
                    let update = UpdateItem {
                        title: parsed
                            .get("title")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        status: parsed
                            .get("status")
                            .and_then(|v| v.as_i64())
                            .map(|n| n as i16),
                        promote: None,
                        sticky: None,
                        fields: parsed.get("fields").cloned(),
                        log: parsed.get("log").and_then(|v| v.as_str()).map(String::from),
                    };
                    Item::update(&pool, id, user_id, update).await
                } else {
                    // Create new item
                    let item_type = match parsed
                        .get("type")
                        .or(parsed.get("item_type"))
                        .and_then(|v| v.as_str())
                    {
                        Some(t) => t.to_string(),
                        None => return host_errors::ERR_PARAM_DESERIALIZE,
                    };
                    let title = parsed
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Untitled")
                        .to_string();
                    let status = parsed.get("status").and_then(|v| v.as_i64()).unwrap_or(0) as i16;

                    let create = CreateItem {
                        item_type,
                        title,
                        status: Some(status),
                        author_id: user_id,
                        fields: parsed.get("fields").cloned(),
                        promote: Some(0),
                        sticky: Some(0),
                        stage_id: None,
                        language: None,
                        log: None,
                    };
                    Item::create(&pool, create).await.map(Some)
                };

                match result {
                    Ok(Some(item)) => match serde_json::to_string(&item) {
                        Ok(json) => write_string_to_memory(
                            &memory,
                            &mut caller,
                            out_ptr,
                            out_max_len,
                            &json,
                        )
                        .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT),
                        Err(_) => host_errors::ERR_SERIALIZE_FAILED,
                    },
                    Ok(None) => {
                        // Update target not found
                        write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, "null")
                            .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT)
                    }
                    Err(e) => {
                        warn!(error = %e, "save-item host function failed");
                        host_errors::ERR_SQL_FAILED
                    }
                }
            })
        },
    )?;

    // delete-item(id) -> i32 (0 = success, negative = error)
    linker.func_wrap_async(
        "trovato:kernel/item-api",
        "delete-item",
        |mut caller: wasmtime::Caller<'_, PluginState>, (id_ptr, id_len): (i32, i32)| {
            Box::new(async move {
                let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return host_errors::ERR_MEMORY_MISSING;
                };

                let Ok(id_str) = read_string_from_memory(&memory, &caller, id_ptr, id_len) else {
                    return host_errors::ERR_PARAM1_READ;
                };

                let Ok(id) = id_str.parse::<Uuid>() else {
                    return host_errors::ERR_PARAM1_READ;
                };

                let Some(services) = caller.data().request.services() else {
                    return host_errors::ERR_NO_SERVICES;
                };
                let pool = services.db.clone();

                match Item::delete(&pool, id).await {
                    Ok(true) => 0,
                    Ok(false) => 0, // Item didn't exist — still success
                    Err(e) => {
                        warn!(item_id = %id, error = %e, "delete-item host function failed");
                        host_errors::ERR_SQL_FAILED
                    }
                }
            })
        },
    )?;

    // query-items(query_json, out) -> i32 (bytes written or error)
    linker.func_wrap_async(
        "trovato:kernel/item-api",
        "query-items",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         (query_ptr, query_len, out_ptr, out_max_len): (i32, i32, i32, i32)| {
            Box::new(async move {
                let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return host_errors::ERR_MEMORY_MISSING;
                };

                let Ok(query_json) =
                    read_string_from_memory(&memory, &caller, query_ptr, query_len)
                else {
                    return host_errors::ERR_PARAM1_READ;
                };

                let Some(services) = caller.data().request.services() else {
                    return host_errors::ERR_NO_SERVICES;
                };
                let pool = services.db.clone();

                // Parse query: {"type": "...", "status": N, "limit": N, "offset": N}
                let query: serde_json::Value = match serde_json::from_str(&query_json) {
                    Ok(v) => v,
                    Err(_) => return host_errors::ERR_PARAM_DESERIALIZE,
                };

                let item_type = query.get("type").and_then(|v| v.as_str());
                let status = query
                    .get("status")
                    .and_then(|v| v.as_i64())
                    .map(|n| n as i16);
                let limit = query
                    .get("limit")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(50)
                    .min(100);
                let offset = query.get("offset").and_then(|v| v.as_i64()).unwrap_or(0);

                match Item::list_filtered(&pool, item_type, status, None, limit, offset).await {
                    Ok(items) => match serde_json::to_string(&items) {
                        Ok(json) => write_string_to_memory(
                            &memory,
                            &mut caller,
                            out_ptr,
                            out_max_len,
                            &json,
                        )
                        .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT),
                        Err(_) => host_errors::ERR_SERIALIZE_FAILED,
                    },
                    Err(e) => {
                        warn!(error = %e, "query-items host function failed");
                        host_errors::ERR_SQL_FAILED
                    }
                }
            })
        },
    )?;

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use wasmtime::Engine;

    #[test]
    fn register_item_succeeds() {
        let mut config = wasmtime::Config::new();
        config.async_support(true);
        let engine = Engine::new(&config).expect("valid engine config");
        let mut linker: Linker<PluginState> = Linker::new(&engine);

        let result = register_item_functions(&mut linker);
        assert!(result.is_ok());
    }
}
