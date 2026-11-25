//! User host functions for WASM plugins.
//!
//! Provides access to current user information and permission checks, and,
//! for a plugin that declares `user_directory`, public facts about other users
//! by id (`load-users`), by exact name (`find-user-by-name`) or by a name
//! prefix (`find-users-by-name-prefix`).

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

/// The shortest and longest name `find-user-by-name` or
/// `find-users-by-name-prefix` accepts. The upper bound matches
/// `users.name`'s `VARCHAR(255)` width: nothing longer could ever name an
/// account, so refusing it before the query is a cheap rejection rather than
/// a correctness rule.
pub const MIN_USER_NAME_LEN: usize = 1;
pub const MAX_USER_NAME_LEN: usize = 255;

/// At most this many rows one `find-users-by-name-prefix` call answers: a
/// short list for an autocomplete widget, not a directory dump. Not
/// caller-configurable, so a plugin cannot turn the prefix search into a way
/// to page through every account.
pub const MAX_PREFIX_RESULTS: i64 = 10;

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

/// Escape `\`, `%` and `_` in `input` so it is safe to use as a LIKE prefix
/// pattern once `%` is appended.
///
/// A plugin's prefix is text a person typed, not a pattern: without this, a
/// prefix containing `_` (a single-character wildcard) or `%` would silently
/// match more than it named, and would also defeat the bound Postgres derives
/// from the pattern's fixed portion to drive `idx_users_name_prefix`, falling
/// back to a sequential scan.
fn escape_like_prefix(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch == '\\' || ch == '%' || ch == '_' {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Answer `find-user-by-name` for `name`: the public facts for the one
/// account whose name matches `name` case-insensitively — the same match
/// login makes (`User::find_by_name`) — or JSON `null` when no account has
/// that name.
///
/// Visibility rule: the same one `load-users` and the kernel's own
/// `/admin/people` listing use — no status or identity filter beyond the
/// `user_directory` capability gate itself. A blocked account or the
/// anonymous account answers exactly as an active, ordinary account would:
/// `user_directory` is a grant to read the directory, equivalent in scope to
/// the admin people listing, not a public-facing directory that would need
/// its own narrower rule.
///
/// # Errors
///
/// [`host_errors::ERR_USER_NAME_INVALID`] when `name` is empty or longer than
/// [`MAX_USER_NAME_LEN`] characters, and [`host_errors::ERR_SQL_FAILED`] when
/// the query fails.
pub async fn find_user_by_name_json<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    name: &str,
) -> Result<String, i32> {
    let len = name.chars().count();
    if !(MIN_USER_NAME_LEN..=MAX_USER_NAME_LEN).contains(&len) {
        return Err(host_errors::ERR_USER_NAME_INVALID);
    }

    let user = sqlx::query_as::<_, PublicUser>(
        "SELECT id, name, status, timezone, language, created \
         FROM users WHERE LOWER(name) = LOWER($1)",
    )
    .bind(name)
    .fetch_optional(executor)
    .await
    .map_err(|e| {
        warn!(error = %e, "find-user-by-name query failed");
        host_errors::ERR_SQL_FAILED
    })?;

    serde_json::to_string(&user).map_err(|_| host_errors::ERR_SERIALIZE_FAILED)
}

/// Answer `find-users-by-name-prefix` for `prefix`: up to
/// [`MAX_PREFIX_RESULTS`] accounts whose name starts with `prefix`
/// case-insensitively, sorted by name — a short list an autocomplete widget
/// can offer as the plugin's user types, not a directory dump.
///
/// The same visibility rule as [`find_user_by_name_json`] applies.
///
/// # Errors
///
/// [`host_errors::ERR_USER_NAME_PREFIX_INVALID`] when `prefix` is empty or
/// longer than [`MAX_USER_NAME_LEN`] characters, and
/// [`host_errors::ERR_SQL_FAILED`] when the query fails.
pub async fn find_users_by_name_prefix_json<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    prefix: &str,
) -> Result<String, i32> {
    let len = prefix.chars().count();
    if !(MIN_USER_NAME_LEN..=MAX_USER_NAME_LEN).contains(&len) {
        return Err(host_errors::ERR_USER_NAME_PREFIX_INVALID);
    }
    let pattern = format!("{}%", escape_like_prefix(prefix));

    let users = sqlx::query_as::<_, PublicUser>(
        "SELECT id, name, status, timezone, language, created \
         FROM users WHERE LOWER(name) LIKE LOWER($1) ESCAPE '\\' \
         ORDER BY LOWER(name) LIMIT $2",
    )
    .bind(&pattern)
    .bind(MAX_PREFIX_RESULTS)
    .fetch_all(executor)
    .await
    .map_err(|e| {
        warn!(error = %e, "find-users-by-name-prefix query failed");
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

    // find-user-by-name(name) -> i32: bytes written, or a negative error code.
    linker
        .func_wrap_async_traced(
            "trovato:kernel/user-api",
            "find-user-by-name",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (name_ptr, name_len, out_ptr, out_max_len): (i32, i32, i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };
                    if !caller.data().user_directory {
                        warn!(
                            plugin = %caller.data().plugin_name,
                            "find-user-by-name refused: the plugin does not declare user_directory"
                        );
                        return host_errors::ERR_USER_DIRECTORY_DENIED;
                    }
                    let Ok(name) = read_string_from_memory(&memory, &caller, name_ptr, name_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };
                    let Some(services) = caller.data().request.services() else {
                        return host_errors::ERR_NO_SERVICES;
                    };
                    let db = services.db.clone();

                    match find_user_by_name_json(&db, &name).await {
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

    // find-users-by-name-prefix(prefix) -> i32: bytes written, or a negative
    // error code.
    linker
        .func_wrap_async_traced(
            "trovato:kernel/user-api",
            "find-users-by-name-prefix",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (prefix_ptr, prefix_len, out_ptr, out_max_len): (i32, i32, i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };
                    if !caller.data().user_directory {
                        warn!(
                            plugin = %caller.data().plugin_name,
                            "find-users-by-name-prefix refused: the plugin does not declare user_directory"
                        );
                        return host_errors::ERR_USER_DIRECTORY_DENIED;
                    }
                    let Ok(prefix) =
                        read_string_from_memory(&memory, &caller, prefix_ptr, prefix_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };
                    let Some(services) = caller.data().request.services() else {
                        return host_errors::ERR_NO_SERVICES;
                    };
                    let db = services.db.clone();

                    match find_users_by_name_prefix_json(&db, &prefix).await {
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

    #[test]
    fn escape_like_prefix_neutralizes_wildcards() {
        assert_eq!(escape_like_prefix("plain"), "plain");
        assert_eq!(escape_like_prefix("a_b"), "a\\_b");
        assert_eq!(escape_like_prefix("a%b"), "a\\%b");
        assert_eq!(escape_like_prefix(r"a\b"), r"a\\b");
        assert_eq!(escape_like_prefix("_%\\"), "\\_\\%\\\\");
    }
}
