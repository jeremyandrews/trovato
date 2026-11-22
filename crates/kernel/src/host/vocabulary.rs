//! Vocabulary host functions (B108): create and list owned vocabularies.
//!
//! A plugin creates vocabularies only for the owner types it declares under
//! `[[vocabulary_owners]]` in its manifest, so one plugin cannot create or
//! read another's groups' vocabularies. See `content::owned_vocabulary`.

use anyhow::Result;
use wasmtime::Linker;

use super::trace::TracedLinker;
use super::{read_string_from_memory, write_string_to_memory};
use crate::content::owned_vocabulary::{self, CreateOwned, CreateRefused};
use crate::plugin::{PluginState, WasmtimeExt};
use trovato_sdk::host_errors;

/// Whether `plugin` declares `owner_type`.
fn declares(services: &crate::tap::RequestServices, plugin: &str, owner_type: &str) -> bool {
    services
        .plugin_runtime
        .as_ref()
        .and_then(|runtime| runtime.get_plugin(plugin))
        .is_some_and(|p| {
            p.info
                .vocabulary_owners
                .iter()
                .any(|o| o.owner_type == owner_type)
        })
}

/// `create-owned` for `plugin`: the vocabulary as JSON, or an error code.
pub async fn create_for(
    services: &crate::tap::RequestServices,
    plugin: &str,
    request_json: &str,
) -> Result<String, i32> {
    let request: CreateOwned =
        serde_json::from_str(request_json).map_err(|_| host_errors::ERR_PARAM_DESERIALIZE)?;
    if !declares(services, plugin, &request.owner_type) {
        tracing::warn!(
            plugin,
            owner_type = %request.owner_type,
            "refusing to create a vocabulary for an owner type the plugin does not declare"
        );
        return Err(host_errors::ERR_VOCABULARY_OWNER_UNDECLARED);
    }
    match owned_vocabulary::create(&services.db, request).await {
        Ok(category) => {
            tracing::info!(
                plugin,
                vocabulary = %category.id,
                owner_type = ?category.owner_type,
                owner_id = ?category.owner_id,
                "owned vocabulary created"
            );
            serde_json::to_string(&category).map_err(|_| host_errors::ERR_SERIALIZE_FAILED)
        }
        Err(e) => match e.downcast_ref::<CreateRefused>() {
            Some(CreateRefused::Exists) => Err(host_errors::ERR_VOCABULARY_EXISTS),
            Some(_) => Err(host_errors::ERR_VOCABULARY_INVALID),
            None => {
                tracing::warn!(plugin, error = %e, "failed to create an owned vocabulary");
                Err(host_errors::ERR_SQL_FAILED)
            }
        },
    }
}

/// `list-owned` for `plugin`: a JSON array, or an error code.
pub async fn list_for(
    services: &crate::tap::RequestServices,
    plugin: &str,
    owner_type: &str,
    owner_id: &str,
) -> Result<String, i32> {
    if !declares(services, plugin, owner_type) {
        return Err(host_errors::ERR_VOCABULARY_OWNER_UNDECLARED);
    }
    let list = crate::models::Category::list_owned(&services.db, owner_type, owner_id)
        .await
        .map_err(|_| host_errors::ERR_SQL_FAILED)?;
    serde_json::to_string(&list).map_err(|_| host_errors::ERR_SERIALIZE_FAILED)
}

/// Register the vocabulary host functions.
pub fn register_vocabulary_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    linker
        .func_wrap_async_traced(
            "trovato:kernel/vocabulary",
            "create-owned",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (req_ptr, req_len, out_ptr, out_max_len): (i32, i32, i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };
                    let Ok(request_json) =
                        read_string_from_memory(&memory, &caller, req_ptr, req_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };
                    let Some(services) = caller.data().request.services() else {
                        return host_errors::ERR_NO_SERVICES;
                    };
                    let services = services.clone();
                    let plugin = caller.data().plugin_name.clone();
                    let json = match create_for(&services, &plugin, &request_json).await {
                        Ok(json) => json,
                        Err(code) => return code,
                    };
                    write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &json)
                        .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT)
                })
            },
        )
        .into_anyhow()?;

    linker
        .func_wrap_async_traced(
            "trovato:kernel/vocabulary",
            "list-owned",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (type_ptr, type_len, id_ptr, id_len, out_ptr, out_max_len): (
                i32,
                i32,
                i32,
                i32,
                i32,
                i32,
            )| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };
                    let Ok(owner_type) =
                        read_string_from_memory(&memory, &caller, type_ptr, type_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };
                    let Ok(owner_id) = read_string_from_memory(&memory, &caller, id_ptr, id_len)
                    else {
                        return host_errors::ERR_PARAM2_OR_OUTPUT;
                    };
                    let Some(services) = caller.data().request.services() else {
                        return host_errors::ERR_NO_SERVICES;
                    };
                    let services = services.clone();
                    let plugin = caller.data().plugin_name.clone();
                    let json = match list_for(&services, &plugin, &owner_type, &owner_id).await {
                        Ok(json) => json,
                        Err(code) => return code,
                    };
                    write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &json)
                        .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT)
                })
            },
        )
        .into_anyhow()?;

    Ok(())
}
