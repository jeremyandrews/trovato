//! User host functions for WASM plugins.
//!
//! Provides access to current user information and permission checks, and,
//! for a plugin that declares `user_directory`, public facts about other users
//! by id (`load-users`).

use anyhow::Result;
use serde::Serialize;
use tracing::warn;
use wasmtime::Linker;

use super::trace::{HostCallGuard, TracedLinker};

use super::{read_string_from_memory, write_string_to_memory};
use crate::plugin::{PluginState, WasmtimeExt};
use trovato_sdk::host_errors;

/// Most ids one `load-users` call may name.
pub const MAX_LOAD_USERS: usize = 100;

/// What `load-users` answers about one user. Built field by field from a query
/// that selects these columns and no others, so an address, a password hash or
/// the user's `data` cannot reach a plugin through this function.
#[derive(Debug, Serialize, sqlx::FromRow)]
struct PublicUser {
    id: uuid::Uuid,
    name: String,
    #[sqlx(try_from = "i16")]
    #[serde(serialize_with = "serialize_status")]
    status: Status,
    timezone: Option<String>,
    language: Option<String>,
    #[serde(serialize_with = "serialize_unix_seconds")]
    created: chrono::DateTime<chrono::Utc>,
}

/// A user's account status as a plugin sees it.
#[derive(Debug, Clone, Copy)]
struct Status(bool);

impl From<i16> for Status {
    fn from(raw: i16) -> Self {
        Self(raw == 1)
    }
}

fn serialize_status<S: serde::Serializer>(status: &Status, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(if status.0 { "active" } else { "blocked" })
}

fn serialize_unix_seconds<S: serde::Serializer>(
    at: &chrono::DateTime<chrono::Utc>,
    s: S,
) -> Result<S::Ok, S::Error> {
    s.serialize_i64(at.timestamp())
}

/// Answer `load-users` for `ids_json`, a JSON array of user ids: the JSON array
/// of public facts the plugin receives, or the error code it gets.
///
/// The limit and the shape are checked before any query, and the lookup is
/// one statement for the whole batch. `executor` is taken by value, so this
/// can run one query and no more.
///
/// # Errors
///
/// [`host_errors::ERR_USER_IDS_INVALID`] when `ids_json` is not an array of
/// UUID strings, [`host_errors::ERR_USER_IDS_TOO_MANY`] for more than
/// [`MAX_LOAD_USERS`], and [`host_errors::ERR_SQL_FAILED`] when the query
/// fails.
pub async fn load_users_json<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    ids_json: &str,
) -> Result<String, i32> {
    let raw: Vec<String> =
        serde_json::from_str(ids_json).map_err(|_| host_errors::ERR_USER_IDS_INVALID)?;
    if raw.len() > MAX_LOAD_USERS {
        return Err(host_errors::ERR_USER_IDS_TOO_MANY);
    }
    let ids = raw
        .iter()
        .map(|id| uuid::Uuid::parse_str(id))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| host_errors::ERR_USER_IDS_INVALID)?;

    let users = sqlx::query_as::<_, PublicUser>(
        "SELECT id, name, status, timezone, language, created \
         FROM users WHERE id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(executor)
    .await
    .map_err(|e| {
        warn!(error = %e, "load-users query failed");
        host_errors::ERR_SQL_FAILED
    })?;

    serde_json::to_string(&users).map_err(|_| host_errors::ERR_SERIALIZE_FAILED)
}

/// Register user host functions.
pub fn register_user_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // current_user_id() -> string
    linker
        .func_wrap(
            "trovato:kernel/user-api",
            "current-user-id",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             out_ptr: i32,
             out_max_len: i32|
             -> i32 {
                let _trace =
                    HostCallGuard::new(&caller, "trovato:kernel/user-api", "current-user-id");
                let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return 0;
                };

                let user_id = caller.data().request.user_id_string();

                write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &user_id)
                    .unwrap_or(0)
            },
        )
        .into_anyhow()?;

    // current_user_has_permission(permission) -> bool
    linker
        .func_wrap(
            "trovato:kernel/user-api",
            "current-user-has-permission",
            |mut caller: wasmtime::Caller<'_, PluginState>, perm_ptr: i32, perm_len: i32| -> i32 {
                let _trace = HostCallGuard::new(
                    &caller,
                    "trovato:kernel/user-api",
                    "current-user-has-permission",
                );
                let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return 0;
                };

                let Ok(permission) = read_string_from_memory(&memory, &caller, perm_ptr, perm_len)
                else {
                    return 0;
                };

                // The *effective* permission, not raw set membership: a site
                // administrator holds everything, the same answer the kernel's
                // own route guards give. Answering from the set alone was the
                // second half of BL-33 — an administrator could open a plugin's
                // screen through `require_permission` and then have that
                // plugin's own check refuse them every action on it.
                let has_perm = caller.data().request.user.can(&permission);
                if has_perm { 1 } else { 0 }
            },
        )
        .into_anyhow()?;

    // load-users(ids_json) -> i32: bytes written, or a negative error code.
    linker
        .func_wrap_async_traced(
            "trovato:kernel/user-api",
            "load-users",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (ids_ptr, ids_len, out_ptr, out_max_len): (i32, i32, i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };
                    // The capability first: a plugin without it learns nothing,
                    // not even whether its ids parse.
                    if !caller.data().user_directory {
                        warn!(
                            plugin = %caller.data().plugin_name,
                            "load-users refused: the plugin does not declare user_directory"
                        );
                        return host_errors::ERR_USER_DIRECTORY_DENIED;
                    }
                    let Ok(ids_json) = read_string_from_memory(&memory, &caller, ids_ptr, ids_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };
                    let Some(services) = caller.data().request.services() else {
                        return host_errors::ERR_NO_SERVICES;
                    };
                    let db = services.db.clone();

                    match load_users_json(&db, &ids_json).await {
                        Ok(json) => write_string_to_memory(
                            &memory,
                            &mut caller,
                            out_ptr,
                            out_max_len,
                            &json,
                        )
                        .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT),
                        Err(code) => code,
                    }
                })
            },
        )
        .into_anyhow()?;

    Ok(())
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use wasmtime::Engine;

    #[test]
    fn register_user_succeeds() {
        let config = wasmtime::Config::new();
        let engine = Engine::new(&config).unwrap();
        let mut linker: Linker<PluginState> = Linker::new(&engine);

        let result = register_user_functions(&mut linker);
        assert!(result.is_ok());
    }
}
