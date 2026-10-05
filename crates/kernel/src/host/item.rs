//! Item host functions for WASM plugins.
//!
//! Provides CRUD operations for items (content records) via direct
//! `Item` model queries. Uses the model directly (not `ItemService`)
//! to avoid re-entrant tap dispatch when a plugin calls save_item
//! from within a tap handler.
//!
//! # Who a call acts as
//!
//! These four functions used to decide nothing. They read and wrote through
//! the `Item` model and used the requesting user only as an author id, so a
//! plugin handling an anonymous visitor's request could read unpublished items
//! and restricted fields through `get-item` and `query-items`, and rewrite or
//! delete any item by id through `save-item` and `delete-item`.
//!
//! The rule now, in one sentence: **a request-scoped call is decided as the
//! user the plugin is acting for, and a background call needs a declared
//! capability.**
//!
//! - **Request-scoped** — any user that is not the background principal,
//!   anonymous included. The call gets that user's own answer. `get-item`
//!   requires `view` and drops the fields the user may not see; `query-items`
//!   returns what the user may view; `save-item` requires `edit` on an update
//!   and `create {type} content` on a create; `delete-item` requires `delete`.
//!   A denied read is indistinguishable from a missing item and writes `null`,
//!   so the error cannot be used to confirm that a draft exists. A denied write
//!   returns [`host_errors::ERR_ITEM_ACCESS_DENIED`].
//! - **Background** — cron and the queue worker, under the kernel-internal
//!   background principal, which carries no identity and holds no permissions.
//!   There is no user whose authority the call could act with, so a plugin that
//!   needs kernel authority there declares `item_background = true` and gets
//!   the old behaviour; without it the call returns
//!   [`host_errors::ERR_ITEM_BACKGROUND_DENIED`]. Same manifest plane as
//!   `ai_background`, for the same reason: a background grant should be visible
//!   in the manifest and reviewable at install time.
//!
//! # What did *not* change
//!
//! Writes still go straight to `Item::create` / `Item::update` rather than
//! through [`crate::content::ItemService`], so the insert, update and delete
//! taps still do not fire from here. That is the contract this module exists
//! for — dispatching a tap from inside a tap is the re-entrancy it was written
//! to avoid — and only the access decision is new. `ItemService` is reached for
//! that decision alone.
//!
//! Because the decision itself dispatches `tap_item_access`, a plugin handling
//! that tap could call back in and dispatch it again without end. Any item-api
//! call made while a decision is in progress fails closed; the marker is the
//! private `DECIDING_ACCESS` task-local, whose own comment says why it is a
//! task-local and not a field on the request state.
use anyhow::Result;
use tracing::warn;
use trovato_sdk::host_errors;
use uuid::Uuid;
use wasmtime::Linker;

use super::trace::TracedLinker;

use super::{read_string_from_memory, write_string_to_memory};
use std::sync::Arc;

use crate::models::{CreateItem, Item, UpdateItem};
use crate::plugin::{PluginState, WasmtimeExt};
use crate::tap::UserContext;

/// Record the "this item needs embedding" intent for an item a **plugin** just
/// saved (**G-ITEM-NO-EMBED**, Argus M2).
///
/// This module deliberately calls `Item::create` / `Item::update` directly
/// rather than going through [`crate::content::ItemService`], to avoid
/// re-entrant tap dispatch when a plugin saves from inside a tap. The cost was
/// that it also bypassed `ItemService::index_item`, so a plugin-created item was
/// full-text findable (the `search_vector` trigger is inside the save
/// transaction) but had **no embedding** — invisible to `SemanticSimilarity`
/// gathers, recoverable only by an operator running the admin backfill by hand.
/// Argus declares `argus_story` as an Item *specifically* to make stories
/// semantically searchable, so the gap negated the reason for the Item tier.
///
/// The fix is the async half of `index_item` and only the async half: one
/// `INSERT` recording the intent, which the queue-v2 embed drain resolves later
/// under the background AI principal. No provider call, no pgvector dependency,
/// and — the point — **no tap dispatch**, so the re-entrancy this module exists
/// to avoid is not reintroduced.
///
/// Two deliberate differences from the `ItemService` path, both stated rather
/// than hidden:
///
/// - `tap_item_update_index` is **not** dispatched here. It is a tap, and
///   dispatching it is exactly the re-entrancy hazard.
/// - A content type on [`EmbedPolicy`]'s `sync_types` opt-out is enqueued
///   anyway rather than embedded inline. Blocking a host call on a provider
///   round-trip inside a plugin's epoch budget is the wrong trade; the item
///   still gets embedded, just by the drain.
///
/// [`EmbedPolicy`]: crate::services::embed_index::EmbedPolicy
///
/// Best-effort by construction: a failure to enqueue is logged and the save
/// still succeeds, because an unembedded item is a degraded item, not a lost
/// one.
async fn enqueue_embed_for_plugin_item(pool: &sqlx::PgPool, item: &Item) {
    let text = crate::content::item_service::item_embedding_text(item);
    if text.trim().is_empty() {
        return;
    }
    if let Err(e) = crate::services::embed_index::enqueue_embed_job(
        pool,
        item.id,
        &crate::services::embed_index::embed_content_hash(&text),
    )
    .await
    {
        warn!(
            item_id = %item.id,
            error = %e,
            "failed to enqueue embed job for a plugin-saved item"
        );
    }
}

/// Who an `item-api` call acts as, decided once at the top of each call.
///
/// The three outcomes are the whole identity rule. A request-scoped call acts
/// as the user the plugin is handling the request for, anonymous included,
/// because that is whose authority the plugin is borrowing. A background call
/// has no such user, so it acts with kernel authority only if the plugin
/// declared that it needs to, and is refused otherwise.
enum Acting {
    /// Decide as this user. Owned, because the `Caller` borrow does not
    /// survive the awaits that follow.
    As(Box<UserContext>),
    /// Kernel authority: a background call from a plugin holding
    /// `item_background`. Behaves as every call behaved before this change.
    Kernel,
    /// Refuse: a background call from a plugin without the capability.
    BackgroundDenied,
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

tokio::task_local! {
    /// Set while an item or field access decision is being dispatched.
    ///
    /// `ItemService::check_access` dispatches `tap_item_access`, and a plugin
    /// handling that tap can call `get-item`, which asks for an access
    /// decision, which dispatches the tap again, without end. An item-api call
    /// made while this is set is refused without dispatching anything.
    ///
    /// A task-local, not a field on the request state, because the dispatch
    /// does not carry the caller's state: `ItemService::tap_state` builds a
    /// **fresh** `RequestState` for the handler, so the invocation depth
    /// `plugin-api` carries does not cross this boundary and neither would a
    /// flag placed beside it. Tap dispatch awaits its handlers inline on the
    /// calling task — nothing under `tap/` spawns — so a task-local does cross
    /// it, and being per-task, two concurrent requests cannot see each other's.
    static DECIDING_ACCESS: ();
}

/// Whether this call is already inside an access decision.
fn inside_access_decision() -> bool {
    DECIDING_ACCESS.try_with(|()| ()).is_ok()
}

/// Run an access decision with the re-entrancy marker set. Every decision this
/// module makes goes through here.
async fn deciding_access<F: std::future::Future>(f: F) -> F::Output {
    DECIDING_ACCESS.scope((), f).await
}

/// The item service this module decides with, or `None` when there is none.
///
/// `RequestServices` holds it weakly — `ItemService` owns a `RequestServices`
/// of its own, so a strong handle there would be a cycle that never frees — so
/// this can fail in a serviceless or test context that never wired one. The
/// callers then report `ERR_NO_SERVICES` rather than deciding without it.
fn decide_with(services: &crate::tap::RequestServices) -> Option<Arc<crate::content::ItemService>> {
    services.item_service()
}

/// Decide a single item for `view` and drop the fields the user may not see.
///
/// Returns `None` when the item is denied, which every caller renders exactly
/// as a missing item: a plugin must not be able to tell a draft it may not see
/// from an id that does not exist.
async fn visible_to(
    items: &Arc<crate::content::ItemService>,
    item: Item,
    user: &UserContext,
) -> Option<Item> {
    // `filter_page_for_view` is the same seam the REST, SSR, gather and search
    // read paths use, and it does both tiers: the per-item access decision and
    // the field-level one. Reusing it is what keeps this function's answer the
    // same as every other read path's.
    deciding_access(items.filter_page_for_view(vec![item], user, "view", 1))
        .await
        .into_iter()
        .next()
}

/// Run a future as though an access decision were in progress.
///
/// The marker is a task-local, so a test cannot set it from outside without a
/// hook: this is that hook, and the only thing in this module that is public
/// beyond [`register_item_functions`]. It exists so the re-entrancy guard can
/// be tested for what it actually promises — that an item-api call made inside
/// a decision is refused — without building a fixture plugin whose
/// `tap_item_access` handler calls back in, which would test the same one line
/// through several hundred more.
#[doc(hidden)]
pub async fn deciding_access_for_test<F: std::future::Future>(f: F) -> F::Output {
    deciding_access(f).await
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
                    if inside_access_decision() {
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
    // Gated as the requesting user: an update needs `edit` on the item that is
    // there, a create needs `create {type} content`, the same permission the
    // item routes check. The write itself still goes straight to the model, so
    // the insert and update taps do not fire; only the decision is new.
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
                    let pool = services.db.clone();
                    let user_id = caller.data().request.user.id;

                    if inside_access_decision() {
                        return host_errors::ERR_ITEM_ACCESS_DENIED;
                    }
                    let acting = acting_for(caller.data());
                    if matches!(acting, Acting::BackgroundDenied) {
                        return host_errors::ERR_ITEM_BACKGROUND_DENIED;
                    }

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
                        // An update is decided on the item that is there now.
                        // A missing one is reported as a miss below, unchanged.
                        if let Acting::As(user) = &acting {
                            let Some(items) = decide_with(&services) else {
                                return host_errors::ERR_NO_SERVICES;
                            };
                            match Item::find_by_id(&pool, id).await {
                                Ok(Some(existing)) => {
                                    let permitted = deciding_access(
                                        items.check_access(&existing, "edit", user),
                                    )
                                    .await
                                    .unwrap_or(false);
                                    if !permitted {
                                        warn!(
                                            item_id = %id,
                                            user_id = %user.id,
                                            "save-item denied: no edit access"
                                        );
                                        return host_errors::ERR_ITEM_ACCESS_DENIED;
                                    }
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    warn!(item_id = %id, error = %e, "save-item load failed");
                                    return host_errors::ERR_SQL_FAILED;
                                }
                            }
                        }

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
                        let status =
                            parsed.get("status").and_then(|v| v.as_i64()).unwrap_or(0) as i16;

                        // A create is decided by the permission the item routes
                        // check for the same act, so a plugin cannot be a way
                        // around the form.
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
                        Ok(Some(item)) => {
                            enqueue_embed_for_plugin_item(&pool, &item).await;
                            match serde_json::to_string(&item) {
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
                        }
                        Ok(None) => {
                            // Update target not found
                            write_string_to_memory(
                                &memory,
                                &mut caller,
                                out_ptr,
                                out_max_len,
                                "null",
                            )
                            .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT)
                        }
                        Err(e) => {
                            warn!(error = %e, "save-item host function failed");
                            host_errors::ERR_SQL_FAILED
                        }
                    }
                })
            },
        )
        .into_anyhow()?;

    // delete-item(id) -> i32 (0 = success, negative = error)
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
                    let pool = services.db.clone();

                    if inside_access_decision() {
                        return host_errors::ERR_ITEM_ACCESS_DENIED;
                    }
                    let acting = acting_for(caller.data());
                    if matches!(acting, Acting::BackgroundDenied) {
                        return host_errors::ERR_ITEM_BACKGROUND_DENIED;
                    }

                    // A missing item is still a success, as it was: deleting
                    // what is not there is what the caller asked for. Only an
                    // item that *is* there has to be decided.
                    if let Acting::As(user) = &acting {
                        let Some(items) = decide_with(&services) else {
                            return host_errors::ERR_NO_SERVICES;
                        };
                        match Item::find_by_id(&pool, id).await {
                            Ok(Some(existing)) => {
                                let permitted =
                                    deciding_access(items.check_access(&existing, "delete", user))
                                        .await
                                        .unwrap_or(false);
                                if !permitted {
                                    warn!(
                                        item_id = %id,
                                        user_id = %user.id,
                                        "delete-item denied: no delete access"
                                    );
                                    return host_errors::ERR_ITEM_ACCESS_DENIED;
                                }
                            }
                            Ok(None) => return 0,
                            Err(e) => {
                                warn!(item_id = %id, error = %e, "delete-item load failed");
                                return host_errors::ERR_SQL_FAILED;
                            }
                        }
                    }

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

                    if inside_access_decision() {
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
                            deciding_access(items.filter_page_for_view(fetched, user, "view", page))
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
