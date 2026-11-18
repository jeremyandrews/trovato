//! Item host functions for WASM plugins.
//!
//! `get-item`, `save-item`, `delete-item` and `query-items`. Reads decide
//! through [`ItemService`]'s read seam; writes **are** [`ItemService`] writes,
//! so a plugin writes an item exactly as a person does.
//!
//! # Who a call acts as
//!
//! **A request-scoped call is decided as the user the plugin is acting for, and
//! a background call needs a declared capability.**
//!
//! - **Request-scoped**: any user that is not the background principal,
//!   anonymous included. `get-item` requires `view` and drops the fields the
//!   user may not see; `query-items` returns what the user may view;
//!   `save-item` needs `create {type} content` on a create (checked here, as the
//!   item routes check it before calling the service) and everything else the
//!   service decides: `edit` access on an update, the field and publish write
//!   gates on both; `delete-item` needs `delete` access, decided by the service.
//!   A denied read is indistinguishable from a missing item and writes `null`,
//!   so the error cannot be used to confirm that a draft exists. A denied write
//!   returns [`host_errors::ERR_ITEM_ACCESS_DENIED`].
//! - **Background**: cron and the queue worker, under the kernel-internal
//!   background principal, which carries no identity and holds no permissions.
//!   A plugin that needs kernel authority there declares `item_background =
//!   true`; the call then goes to the service as the background principal,
//!   which passes the access decision and both write gates. Without it the call
//!   returns [`host_errors::ERR_ITEM_BACKGROUND_DENIED`].
//!
//! # What a write does
//!
//! `save-item` calls [`ItemService::create`] or [`ItemService::update`] and
//! `delete-item` calls [`ItemService::delete`], so a plugin's write fires
//! `tap_item_presave`, `tap_item_insert` or `tap_item_update`,
//! `tap_item_update_index` and `tap_item_delete` on the other plugins, records a
//! revision, syncs file references, enqueues the item's embedding and
//! invalidates the item cache.
//!
//! A tap handling a write may write again. The bound, from
//! `docs/design/plugin-item-plumbing.md`: a nested write fires its taps to every
//! plugin except those whose write tap is already running around it, and a write
//! nested [`MAX_ITEM_WRITE_DEPTH`] write taps deep is refused with
//! [`host_errors::ERR_ITEM_WRITE_DEPTH`]. An item-api call made while
//! `tap_item_access` or `tap_field_access` is being dispatched is refused
//! outright, because the decision would ask for itself again. Both markers live
//! in [`crate::content::reentry`].
//!
//! [`ItemService`]: crate::content::ItemService
//! [`ItemService::create`]: crate::content::ItemService::create
//! [`ItemService::update`]: crate::content::ItemService::update
//! [`ItemService::delete`]: crate::content::ItemService::delete
//! [`MAX_ITEM_WRITE_DEPTH`]: crate::content::reentry::MAX_ITEM_WRITE_DEPTH
use anyhow::Result;
use tracing::warn;
use trovato_sdk::host_errors;
use uuid::Uuid;
use wasmtime::Linker;

use super::trace::TracedLinker;

use super::{read_string_from_memory, write_string_to_memory};
use std::sync::Arc;

use crate::content::reentry::{self, WriteTooDeep};
use crate::content::{ItemService, WriteDenied};
use crate::models::{CreateItem, Item, Language, UpdateItem};
use crate::plugin::{PluginState, WasmtimeExt};
use crate::tap::UserContext;

/// Who an `item-api` call acts as, decided once at the top of each call.
///
/// A request-scoped call acts as the user the plugin is handling the request
/// for, anonymous included, because that is whose authority the plugin is
/// borrowing. A background call has no such user, so it acts with kernel
/// authority only if the plugin declared that it needs to, and is refused
/// otherwise.
enum Acting {
    /// Decide as this user. Owned, because the `Caller` borrow does not
    /// survive the awaits that follow.
    As(Box<UserContext>),
    /// Kernel authority: a background call from a plugin holding
    /// `item_background`. Reads skip the decision; writes go to the service as
    /// the background principal.
    Kernel,
    /// Refuse: a background call from a plugin without the capability.
    BackgroundDenied,
}

impl Acting {
    /// The user a write is made as: the requesting user, or the background
    /// principal for kernel authority. `None` for a refused call.
    fn writer(&self) -> Option<UserContext> {
        match self {
            Self::As(user) => Some((**user).clone()),
            Self::Kernel => Some(UserContext::background()),
            Self::BackgroundDenied => None,
        }
    }
}

/// Resolve who a call acts as, from the request state and the plugin's
/// declared capability.
fn acting_for(state: &PluginState) -> Acting {
    let user = &state.request.user;
    if !user.is_background() {
        return Acting::As(Box::new(user.clone()));
    }
    if state.item_background {
        Acting::Kernel
    } else {
        Acting::BackgroundDenied
    }
}

/// The item service this module decides with, or `None` when there is none.
///
/// `RequestServices` holds it weakly (`ItemService` owns a `RequestServices`
/// of its own, so a strong handle there would be a cycle that never frees), so
/// this can fail in a serviceless or test context that never wired one. The
/// callers then report `ERR_NO_SERVICES` rather than deciding without it.
fn decide_with(services: &crate::tap::RequestServices) -> Option<Arc<ItemService>> {
    services.item_service()
}

/// Decide a single item for `view` and drop the fields the user may not see.
///
/// Returns `None` when the item is denied, which every caller renders exactly
/// as a missing item: a plugin must not be able to tell a draft it may not see
/// from an id that does not exist.
async fn visible_to(items: &Arc<ItemService>, item: Item, user: &UserContext) -> Option<Item> {
    // `filter_page_for_view` is the same seam the REST, SSR, gather and search
    // read paths use, and it does both tiers: the per-item access decision and
    // the field-level one.
    items
        .filter_page_for_view(vec![item], user, "view", 1)
        .await
        .into_iter()
        .next()
}

/// The host code for a write the service refused or failed.
///
/// A refusal by the field or publish gate and the service's own "access denied"
/// both read as [`host_errors::ERR_ITEM_ACCESS_DENIED`], the code a denied write
/// has always returned; reads are unaffected, so a denied read still cannot
/// confirm that an item exists.
fn write_error_code(e: &anyhow::Error) -> i32 {
    if e.downcast_ref::<WriteTooDeep>().is_some() {
        return host_errors::ERR_ITEM_WRITE_DEPTH;
    }
    if e.downcast_ref::<WriteDenied>().is_some() || e.to_string().contains("access denied") {
        return host_errors::ERR_ITEM_ACCESS_DENIED;
    }
    warn!(error = %e, "item-api write failed");
    host_errors::ERR_SQL_FAILED
}

/// Register item host functions.
pub fn register_item_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // get-item(id, out) -> i32 (bytes written or error)
    linker
        .func_wrap_async_traced(
            "trovato:kernel/item-api",
            "get-item",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (id_ptr, id_len, out_ptr, out_max_len): (i32, i32, i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };

                    let Ok(id_str) = read_string_from_memory(&memory, &caller, id_ptr, id_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };

                    let Ok(id) = id_str.parse::<Uuid>() else {
                        return host_errors::ERR_PARAM1_READ;
                    };

                    let Some(services) = caller.data().request.services() else {
                        return host_errors::ERR_NO_SERVICES;
                    };
                    let services = services.clone();
                    let pool = services.db.clone();

                    // Fail closed inside an access decision: a `tap_item_access`
                    // handler reading an item would ask for another decision,
                    // and so on without end.
                    if reentry::inside_access_decision() {
                        return host_errors::ERR_ITEM_ACCESS_DENIED;
                    }
                    let acting = acting_for(caller.data());
                    // Decided before the load: a refused call has no business
                    // touching the database.
                    if matches!(acting, Acting::BackgroundDenied) {
                        return host_errors::ERR_ITEM_BACKGROUND_DENIED;
                    }

                    let loaded = match Item::find_by_id(&pool, id).await {
                        Ok(found) => found,
                        Err(e) => {
                            warn!(item_id = %id, error = %e, "get-item host function failed");
                            return host_errors::ERR_SQL_FAILED;
                        }
                    };

                    // A denial and a miss are one answer on purpose: a plugin
                    // must not be able to tell a draft it may not see from an
                    // id that does not exist.
                    let item = match &acting {
                        Acting::BackgroundDenied => return host_errors::ERR_ITEM_BACKGROUND_DENIED,
                        Acting::Kernel => loaded,
                        Acting::As(user) => match loaded {
                            None => None,
                            Some(item) => {
                                let Some(items) = decide_with(&services) else {
                                    return host_errors::ERR_NO_SERVICES;
                                };
                                visible_to(&items, item, user).await
                            }
                        },
                    };

                    match item {
                        Some(item) => match serde_json::to_string(&item) {
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
                        None => write_string_to_memory(
                            &memory,
                            &mut caller,
                            out_ptr,
                            out_max_len,
                            "null",
                        )
                        .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT),
                    }
                })
            },
        )
        .into_anyhow()?;

    // save-item(item_json, out) -> i32 (bytes written or error)
    //
    // A JSON object with a non-nil `id` is an update through
    // `ItemService::update`; anything else is a create through
    // `ItemService::create`. An update of an id that names no item writes
    // `null`.
    linker
        .func_wrap_async_traced(
            "trovato:kernel/item-api",
            "save-item",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (item_ptr, item_len, out_ptr, out_max_len): (i32, i32, i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };

                    let Ok(item_json) =
                        read_string_from_memory(&memory, &caller, item_ptr, item_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };

                    let Some(services) = caller.data().request.services() else {
                        return host_errors::ERR_NO_SERVICES;
                    };
                    let services = services.clone();

                    if reentry::inside_access_decision() {
                        return host_errors::ERR_ITEM_ACCESS_DENIED;
                    }
                    let acting = acting_for(caller.data());
                    let Some(writer) = acting.writer() else {
                        return host_errors::ERR_ITEM_BACKGROUND_DENIED;
                    };
                    let Some(items) = decide_with(&services) else {
                        return host_errors::ERR_NO_SERVICES;
                    };

                    let parsed: serde_json::Value = match serde_json::from_str(&item_json) {
                        Ok(v) => v,
                        Err(_) => return host_errors::ERR_PARAM_DESERIALIZE,
                    };
                    let text = |key: &str| parsed.get(key).and_then(|v| v.as_str());
                    let status = parsed
                        .get("status")
                        .and_then(|v| v.as_i64())
                        .map(|n| n as i16);

                    // A non-nil `id` is an update.
                    let existing_id = text("id")
                        .and_then(|s| s.parse::<Uuid>().ok())
                        .filter(|id| !id.is_nil());

                    let result = if let Some(id) = existing_id {
                        let update = UpdateItem {
                            title: text("title").map(String::from),
                            status,
                            promote: None,
                            sticky: None,
                            fields: parsed.get("fields").cloned(),
                            log: text("log").map(String::from),
                        };
                        items.update(id, update, &writer).await
                    } else {
                        let Some(item_type) = text("type").or(text("item_type")) else {
                            return host_errors::ERR_PARAM_DESERIALIZE;
                        };
                        let item_type = item_type.to_string();

                        // The item routes check the create permission before
                        // calling the service, and the service does not, so
                        // this one decision stays here.
                        if let Acting::As(user) = &acting {
                            let permission = format!("create {item_type} content");
                            if !user.can(&permission) {
                                warn!(
                                    user_id = %user.id,
                                    permission = %permission,
                                    "save-item denied: may not create this type"
                                );
                                return host_errors::ERR_ITEM_ACCESS_DENIED;
                            }
                        }

                        // A language must be one the site has, the same list the
                        // item routes resolve a request's language against.
                        let language = match text("language") {
                            None => None,
                            Some(lang) => match Language::find_by_id(&services.db, lang).await {
                                Ok(Some(_)) => Some(lang.to_string()),
                                Ok(None) => return host_errors::ERR_PARAM_DESERIALIZE,
                                Err(e) => {
                                    warn!(error = %e, "save-item language lookup failed");
                                    return host_errors::ERR_SQL_FAILED;
                                }
                            },
                        };

                        let create = CreateItem {
                            item_type,
                            title: text("title").unwrap_or("Untitled").to_string(),
                            status: Some(status.unwrap_or(0)),
                            author_id: writer.id,
                            fields: parsed.get("fields").cloned(),
                            promote: Some(0),
                            sticky: Some(0),
                            // Stage on create is B69's (BL-92).
                            stage_id: None,
                            language,
                            log: text("log").map(String::from),
                        };
                        items.create(create, &writer).await.map(Some)
                    };

                    let json = match result {
                        Ok(Some(item)) => match serde_json::to_string(&item) {
                            Ok(json) => json,
                            Err(_) => return host_errors::ERR_SERIALIZE_FAILED,
                        },
                        // An update of an id that names no item.
                        Ok(None) => "null".to_string(),
                        Err(e) => return write_error_code(&e),
                    };
                    write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &json)
                        .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT)
                })
            },
        )
        .into_anyhow()?;

    // delete-item(id) -> i32 (0 = deleted, negative = error)
    //
    // Through `ItemService::delete`. An id that names no item is
    // `ERR_ITEM_NOT_FOUND`: deleting nothing is not a success.
    linker
        .func_wrap_async_traced(
            "trovato:kernel/item-api",
            "delete-item",
            |mut caller: wasmtime::Caller<'_, PluginState>, (id_ptr, id_len): (i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };

                    let Ok(id_str) = read_string_from_memory(&memory, &caller, id_ptr, id_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };

                    let Ok(id) = id_str.parse::<Uuid>() else {
                        return host_errors::ERR_PARAM1_READ;
                    };

                    let Some(services) = caller.data().request.services() else {
                        return host_errors::ERR_NO_SERVICES;
                    };
                    let services = services.clone();

                    if reentry::inside_access_decision() {
                        return host_errors::ERR_ITEM_ACCESS_DENIED;
                    }
                    let Some(writer) = acting_for(caller.data()).writer() else {
                        return host_errors::ERR_ITEM_BACKGROUND_DENIED;
                    };
                    let Some(items) = decide_with(&services) else {
                        return host_errors::ERR_NO_SERVICES;
                    };

                    match items.delete(id, &writer).await {
                        Ok(true) => 0,
                        Ok(false) => host_errors::ERR_ITEM_NOT_FOUND,
                        Err(e) => write_error_code(&e),
                    }
                })
            },
        )
        .into_anyhow()?;

    // query-items(query_json, out) -> i32 (bytes written or error)
    linker
        .func_wrap_async_traced(
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
                    let services = services.clone();
                    let pool = services.db.clone();

                    if reentry::inside_access_decision() {
                        return host_errors::ERR_ITEM_ACCESS_DENIED;
                    }
                    let acting = acting_for(caller.data());
                    if matches!(acting, Acting::BackgroundDenied) {
                        return host_errors::ERR_ITEM_BACKGROUND_DENIED;
                    }

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

                    let fetched =
                        match Item::list_filtered(&pool, item_type, status, None, limit, offset)
                            .await
                        {
                            Ok(found) => found,
                            Err(e) => {
                                warn!(error = %e, "query-items host function failed");
                                return host_errors::ERR_SQL_FAILED;
                            }
                        };

                    // What the user may view, with the fields they may see.
                    // The page can come back shorter than `limit`; that is the
                    // same thing every other filtered read path does, and the
                    // alternative is telling the caller how many items it was
                    // not allowed to see.
                    let visible = match &acting {
                        Acting::BackgroundDenied => return host_errors::ERR_ITEM_BACKGROUND_DENIED,
                        Acting::Kernel => fetched,
                        Acting::As(user) => {
                            let Some(items) = decide_with(&services) else {
                                return host_errors::ERR_NO_SERVICES;
                            };
                            let page = usize::try_from(limit).unwrap_or(usize::MAX);
                            items
                                .filter_page_for_view(fetched, user, "view", page)
                                .await
                        }
                    };

                    match serde_json::to_string(&visible) {
                        Ok(json) => write_string_to_memory(
                            &memory,
                            &mut caller,
                            out_ptr,
                            out_max_len,
                            &json,
                        )
                        .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT),
                        Err(_) => host_errors::ERR_SERIALIZE_FAILED,
                    }
                })
            },
        )
        .into_anyhow()?;

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use wasmtime::Engine;

    #[test]
    fn register_item_succeeds() {
        let config = wasmtime::Config::new();
        let engine = Engine::new(&config).expect("valid engine config");
        let mut linker: Linker<PluginState> = Linker::new(&engine);

        let result = register_item_functions(&mut linker);
        assert!(result.is_ok());
    }
}
