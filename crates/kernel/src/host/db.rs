//! Database host functions for WASM plugins.
//!
//! Provides both raw and structured database access. All queries use
//! JSON-encoded parameters and return JSON results.
//!
//! Structured calls (`select`/`insert`/`update`/`delete`) are confined to the
//! plugin's effective table allowlist, under the protected-table floor in
//! [`crate::plugin::db_policy`]. Raw calls are parsed and judged as a tree by
//! [`super::sql_guard`], and `query-raw` additionally runs inside a read-only
//! transaction, which is the control that does not depend on the parser being
//! right.

use anyhow::Result;
use regex::Regex;
use sqlx::postgres::PgArguments;
use sqlx::{Column, Executor, PgPool, Row, TypeInfo};
use std::sync::LazyLock;
use tracing::warn;
use trovato_sdk::host_errors;
use wasmtime::Linker;

use super::sql_guard;
use super::trace::TracedLinker;

use crate::plugin::WasmtimeExt;

/// Maximum execution time for plugin SQL queries (5 seconds).
const PLUGIN_QUERY_TIMEOUT_MS: u32 = 5000;

use super::{read_string_from_memory, write_string_to_memory};
use crate::plugin::{DbPolicy, PluginState};

/// Regex for valid SQL identifiers (table/column names).
///
/// # Panics
///
/// Panics if the hard-coded regex literal is invalid (impossible in practice).
#[allow(clippy::expect_used)]
static VALID_IDENTIFIER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z_][a-zA-Z0-9_]*$").expect("valid regex literal"));

/// Check if SQL contains semicolons (potential multi-statement injection).
fn has_semicolons(sql: &str) -> bool {
    sql.contains(';')
}

/// Bind JSON parameter values to a sqlx query dynamically.
fn bind_json_params<'q>(
    params: &[serde_json::Value],
    mut query: sqlx::query::Query<'q, sqlx::Postgres, PgArguments>,
) -> sqlx::query::Query<'q, sqlx::Postgres, PgArguments> {
    for param in params {
        match param {
            serde_json::Value::String(s) => query = query.bind(s.clone()),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    query = query.bind(i);
                } else if let Some(f) = n.as_f64() {
                    query = query.bind(f);
                }
            }
            serde_json::Value::Bool(b) => query = query.bind(*b),
            serde_json::Value::Null => query = query.bind(Option::<String>::None),
            // Arrays/objects: bind as JSON
            other => {
                if let Ok(s) = serde_json::to_string(other) {
                    query = query.bind(s);
                }
            }
        }
    }
    query
}

/// Serialize a sqlx Row to a JSON object using column metadata.
fn row_to_json(row: &sqlx::postgres::PgRow) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for col in row.columns() {
        let name = col.name();
        let type_name = col.type_info().name();
        let value = match type_name {
            "BOOL" => row
                .try_get::<bool, _>(name)
                .ok()
                .map(serde_json::Value::Bool)
                .unwrap_or(serde_json::Value::Null),
            "INT2" => row
                .try_get::<i16, _>(name)
                .ok()
                .map(|v| serde_json::Value::Number(v.into()))
                .unwrap_or(serde_json::Value::Null),
            "INT4" => row
                .try_get::<i32, _>(name)
                .ok()
                .map(|v| serde_json::Value::Number(v.into()))
                .unwrap_or(serde_json::Value::Null),
            "INT8" => row
                .try_get::<i64, _>(name)
                .ok()
                .map(|v| serde_json::Value::Number(v.into()))
                .unwrap_or(serde_json::Value::Null),
            "FLOAT4" => row
                .try_get::<f32, _>(name)
                .ok()
                .and_then(|v| serde_json::Number::from_f64(f64::from(v)))
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            "FLOAT8" => row
                .try_get::<f64, _>(name)
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            "UUID" => row
                .try_get::<uuid::Uuid, _>(name)
                .ok()
                .map(|v| serde_json::Value::String(v.to_string()))
                .unwrap_or(serde_json::Value::Null),
            "JSON" | "JSONB" => row
                .try_get::<serde_json::Value, _>(name)
                .ok()
                .unwrap_or(serde_json::Value::Null),
            // TEXT, VARCHAR, CHAR, and everything else → string
            _ => row
                .try_get::<String, _>(name)
                .ok()
                .map(serde_json::Value::String)
                .unwrap_or(serde_json::Value::Null),
        };
        map.insert(name.to_string(), value);
    }
    serde_json::Value::Object(map)
}

/// Execute a SELECT query and return JSON results, writing to the WASM output buffer.
async fn do_query_raw(
    pool: &PgPool,
    sql: &str,
    params: &[serde_json::Value],
) -> std::result::Result<String, i32> {
    if has_semicolons(sql) {
        return Err(host_errors::ERR_DDL_REJECTED);
    }
    guard_raw(sql, sql_guard::check_query_raw(sql))?;

    // Read only from the transaction's first statement. The parser above is the
    // first control and this is the second: a write the parser somehow blessed
    // is refused by the server, which does not depend on this kernel being
    // right about PostgreSQL's grammar.
    fetch_rows(pool, sql, params, Access::ReadOnly).await
}

/// Map a raw-SQL rejection onto the ABI, logging which rule refused it.
///
/// A protected table reports as `table-not-declared`, the code a structured
/// call to the same table already returns; everything else keeps the
/// `ddl-rejected` code raw SQL has always used on refusal. Neither is new, so
/// the SDK is unchanged.
fn guard_raw(
    sql: &str,
    outcome: std::result::Result<(), sql_guard::RawSqlReject>,
) -> std::result::Result<(), i32> {
    match outcome {
        Ok(()) => Ok(()),
        Err(reject) => {
            warn!(denied = %reject, sql = sql, "plugin raw SQL rejected");
            Err(match reject {
                sql_guard::RawSqlReject::ProtectedTable(_) => host_errors::ERR_TABLE_NOT_DECLARED,
                _ => host_errors::ERR_DDL_REJECTED,
            })
        }
    }
}

/// Whether the transaction a statement runs in may write.
///
/// `query-raw` is [`Access::ReadOnly`]; `do_insert`'s `RETURNING *` runs the
/// same row-fetching code and must stay [`Access::ReadWrite`], which is why
/// this is a parameter rather than a property of the function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    ReadOnly,
    ReadWrite,
}

impl Access {
    /// The statement that opens the transaction.
    ///
    /// `BEGIN READ ONLY` makes the server the authority on whether anything
    /// wrote, from the transaction's first statement rather than after some
    /// first statement has already run.
    fn begin(self) -> &'static str {
        match self {
            Access::ReadOnly => "BEGIN READ ONLY",
            Access::ReadWrite => "BEGIN",
        }
    }
}

/// Execute a SQL statement that returns rows and serialize them as JSON.
///
/// Shared implementation for `do_query_raw` (after the guard, read only) and
/// `do_insert` (`RETURNING *`, read write). Wraps the query in an explicit
/// transaction so `SET LOCAL statement_timeout` is scoped correctly (it has no
/// effect outside a transaction).
async fn fetch_rows(
    pool: &PgPool,
    sql: &str,
    params: &[serde_json::Value],
    access: Access,
) -> std::result::Result<String, i32> {
    let mut conn = pool.acquire().await.map_err(|e| {
        warn!(error = %e, "failed to acquire DB connection for plugin query");
        host_errors::ERR_SQL_FAILED
    })?;

    // Wrap in explicit transaction so SET LOCAL is properly scoped. The access
    // mode is set by BEGIN itself, before any other statement in the
    // transaction, so there is no window in which a write would be accepted.
    conn.execute(access.begin()).await.map_err(|e| {
        warn!(error = %e, "failed to begin transaction for plugin query");
        host_errors::ERR_SQL_FAILED
    })?;

    // Set statement timeout scoped to this transaction.
    let timeout_result = conn
        .execute(format!("SET LOCAL statement_timeout = '{PLUGIN_QUERY_TIMEOUT_MS}'").as_str())
        .await;
    if let Err(e) = timeout_result {
        warn!(error = %e, "failed to set statement_timeout");
        let _ = conn.execute("ROLLBACK").await;
        return Err(host_errors::ERR_SQL_FAILED);
    }

    let query = sqlx::query(sql);
    let query = bind_json_params(params, query);

    let rows = match query.fetch_all(&mut *conn).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, sql = sql, "plugin query failed");
            let _ = conn.execute("ROLLBACK").await;
            return Err(host_errors::ERR_SQL_FAILED);
        }
    };

    let _ = conn.execute("COMMIT").await;

    let json_rows: Vec<serde_json::Value> = rows.iter().map(row_to_json).collect();
    serde_json::to_string(&json_rows).map_err(|_| host_errors::ERR_SERIALIZE_FAILED)
}

/// Execute a DML statement and return rows affected.
///
/// Wraps the statement in an explicit transaction so `SET LOCAL statement_timeout`
/// is scoped correctly. Accepts exactly one INSERT, UPDATE or DELETE and
/// nothing else — see [`super::sql_guard`] for what that excludes and why.
async fn do_execute_raw(
    pool: &PgPool,
    sql: &str,
    params: &[serde_json::Value],
) -> std::result::Result<u64, i32> {
    if has_semicolons(sql) {
        return Err(host_errors::ERR_DDL_REJECTED);
    }
    guard_raw(sql, sql_guard::check_execute_raw(sql))?;

    let mut conn = pool.acquire().await.map_err(|e| {
        warn!(error = %e, "failed to acquire DB connection for plugin execute");
        host_errors::ERR_SQL_FAILED
    })?;

    // Wrap in explicit transaction so SET LOCAL is properly scoped.
    conn.execute("BEGIN").await.map_err(|e| {
        warn!(error = %e, "failed to begin transaction for plugin execute");
        host_errors::ERR_SQL_FAILED
    })?;

    let timeout_result = conn
        .execute(format!("SET LOCAL statement_timeout = '{PLUGIN_QUERY_TIMEOUT_MS}'").as_str())
        .await;
    if let Err(e) = timeout_result {
        warn!(error = %e, "failed to set statement_timeout");
        let _ = conn.execute("ROLLBACK").await;
        return Err(host_errors::ERR_SQL_FAILED);
    }

    let query = sqlx::query(sql);
    let query = bind_json_params(params, query);

    let result = match query.execute(&mut *conn).await {
        Ok(result) => result,
        Err(e) => {
            warn!(error = %e, sql = sql, "plugin execute-raw failed");
            let _ = conn.execute("ROLLBACK").await;
            return Err(host_errors::ERR_SQL_FAILED);
        }
    };

    let _ = conn.execute("COMMIT").await;

    Ok(result.rows_affected())
}

/// Reject a structured call whose target table is outside the plugin's effective
/// allowlist (WASM-2 / D-19). Logs the declarative `table-not-declared` detail
/// and maps to the numeric ABI code.
fn enforce_table(policy: &DbPolicy, table: &str) -> std::result::Result<(), i32> {
    policy.check_table(table).map_err(|msg| {
        warn!(denied = %msg, "plugin db call rejected: table outside allowlist");
        host_errors::ERR_TABLE_NOT_DECLARED
    })
}

/// Build and execute a structured SELECT query.
async fn do_select(
    policy: &DbPolicy,
    pool: &PgPool,
    query_json: &str,
) -> std::result::Result<String, i32> {
    let query: SelectQuery =
        serde_json::from_str(query_json).map_err(|_| host_errors::ERR_PARAM_DESERIALIZE)?;

    if !VALID_IDENTIFIER.is_match(&query.table) {
        return Err(host_errors::ERR_INVALID_IDENTIFIER);
    }

    // WASM-2 allowlist gate (before any pool work).
    enforce_table(policy, &query.table)?;

    // Build column list
    let columns = if query.columns.is_empty() || query.columns.iter().any(|c| c == "*") {
        "*".to_string()
    } else {
        for col in &query.columns {
            if !VALID_IDENTIFIER.is_match(col) {
                return Err(host_errors::ERR_INVALID_IDENTIFIER);
            }
        }
        query.columns.join(", ")
    };

    let mut sql = format!("SELECT {columns} FROM {}", query.table);
    let mut params: Vec<serde_json::Value> = Vec::new();
    let mut param_idx = 1;

    // WHERE clause
    if let Some(ref where_map) = query.where_clause {
        let mut conditions = Vec::new();
        for (col, val) in where_map {
            if !VALID_IDENTIFIER.is_match(col) {
                return Err(host_errors::ERR_INVALID_IDENTIFIER);
            }
            conditions.push(format!("{col} = ${param_idx}"));
            params.push(val.clone());
            param_idx += 1;
        }
        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }
    }

    // ORDER BY
    if let Some(ref orders) = query.order {
        let mut order_parts = Vec::new();
        for o in orders {
            if !VALID_IDENTIFIER.is_match(&o.column) {
                return Err(host_errors::ERR_INVALID_IDENTIFIER);
            }
            let dir = if o.direction.eq_ignore_ascii_case("desc") {
                "DESC"
            } else {
                "ASC"
            };
            order_parts.push(format!("{} {dir}", o.column));
        }
        if !order_parts.is_empty() {
            sql.push_str(" ORDER BY ");
            sql.push_str(&order_parts.join(", "));
        }
    }

    // LIMIT
    if let Some(limit) = query.limit {
        sql.push_str(&format!(" LIMIT ${param_idx}"));
        params.push(serde_json::json!(limit));
    }

    do_query_raw(pool, &sql, &params).await
}

/// Build and execute a structured INSERT.
async fn do_insert(
    policy: &DbPolicy,
    pool: &PgPool,
    table: &str,
    data_json: &str,
) -> std::result::Result<String, i32> {
    if !VALID_IDENTIFIER.is_match(table) {
        return Err(host_errors::ERR_INVALID_IDENTIFIER);
    }

    // WASM-2 allowlist gate (before any pool work).
    enforce_table(policy, table)?;

    let data: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(data_json).map_err(|_| host_errors::ERR_PARAM_DESERIALIZE)?;

    if data.is_empty() {
        return Err(host_errors::ERR_PARAM_DESERIALIZE);
    }

    let mut columns = Vec::new();
    let mut placeholders = Vec::new();
    let mut params = Vec::new();

    for (idx, (col, val)) in (1..).zip(&data) {
        if !VALID_IDENTIFIER.is_match(col) {
            return Err(host_errors::ERR_INVALID_IDENTIFIER);
        }
        columns.push(col.as_str());
        placeholders.push(format!("${idx}"));
        params.push(val.clone());
    }

    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({}) RETURNING *",
        table,
        columns.join(", "),
        placeholders.join(", ")
    );

    // Bypass read-only guard since INSERT RETURNING needs row results.
    fetch_rows(pool, &sql, &params, Access::ReadWrite).await
}

/// Build and execute a structured UPDATE.
async fn do_update(
    policy: &DbPolicy,
    pool: &PgPool,
    table: &str,
    data_json: &str,
    where_json: &str,
) -> std::result::Result<u64, i32> {
    if !VALID_IDENTIFIER.is_match(table) {
        return Err(host_errors::ERR_INVALID_IDENTIFIER);
    }

    // WASM-2 allowlist gate (before any pool work).
    enforce_table(policy, table)?;

    let data: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(data_json).map_err(|_| host_errors::ERR_PARAM_DESERIALIZE)?;
    let where_map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(where_json).map_err(|_| host_errors::ERR_PARAM_DESERIALIZE)?;

    if data.is_empty() || where_map.is_empty() {
        return Err(host_errors::ERR_PARAM_DESERIALIZE);
    }

    let mut set_parts = Vec::new();
    let mut params = Vec::new();
    let mut idx = 1;

    for (col, val) in &data {
        if !VALID_IDENTIFIER.is_match(col) {
            return Err(host_errors::ERR_INVALID_IDENTIFIER);
        }
        set_parts.push(format!("{col} = ${idx}"));
        params.push(val.clone());
        idx += 1;
    }

    let mut where_parts = Vec::new();
    for (col, val) in &where_map {
        if !VALID_IDENTIFIER.is_match(col) {
            return Err(host_errors::ERR_INVALID_IDENTIFIER);
        }
        where_parts.push(format!("{col} = ${idx}"));
        params.push(val.clone());
        idx += 1;
    }

    let sql = format!(
        "UPDATE {} SET {} WHERE {}",
        table,
        set_parts.join(", "),
        where_parts.join(" AND ")
    );

    do_execute_raw(pool, &sql, &params).await
}

/// Build and execute a structured DELETE.
async fn do_delete(
    policy: &DbPolicy,
    pool: &PgPool,
    table: &str,
    where_json: &str,
) -> std::result::Result<u64, i32> {
    if !VALID_IDENTIFIER.is_match(table) {
        return Err(host_errors::ERR_INVALID_IDENTIFIER);
    }

    // WASM-2 allowlist gate (before any pool work).
    enforce_table(policy, table)?;

    let where_map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(where_json).map_err(|_| host_errors::ERR_PARAM_DESERIALIZE)?;

    if where_map.is_empty() {
        return Err(host_errors::ERR_PARAM_DESERIALIZE);
    }

    let mut where_parts = Vec::new();
    let mut params = Vec::new();

    for (idx, (col, val)) in (1..).zip(&where_map) {
        if !VALID_IDENTIFIER.is_match(col) {
            return Err(host_errors::ERR_INVALID_IDENTIFIER);
        }
        where_parts.push(format!("{col} = ${idx}"));
        params.push(val.clone());
    }

    let sql = format!("DELETE FROM {} WHERE {}", table, where_parts.join(" AND "));

    do_execute_raw(pool, &sql, &params).await
}

/// Structured SELECT query format.
#[derive(serde::Deserialize)]
struct SelectQuery {
    table: String,
    #[serde(default)]
    columns: Vec<String>,
    #[serde(rename = "where")]
    where_clause: Option<serde_json::Map<String, serde_json::Value>>,
    order: Option<Vec<OrderClause>>,
    limit: Option<i64>,
}

/// ORDER BY clause for structured queries.
#[derive(serde::Deserialize)]
struct OrderClause {
    column: String,
    #[serde(default = "default_asc")]
    direction: String,
}

fn default_asc() -> String {
    "asc".to_string()
}

/// Register database host functions.
///
/// All DB host functions use `func_wrap_async` because they need to perform
/// async database queries via sqlx.
pub fn register_db_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    // select(query_json, out) -> i32 (bytes written or error)
    linker
        .func_wrap_async_traced(
            "trovato:kernel/db",
            "select",
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

                    let policy = caller.data().db_policy.clone();

                    let Some(services) = caller.data().request.services() else {
                        return host_errors::ERR_NO_SERVICES;
                    };
                    let pool = services.db.clone();

                    match do_select(&policy, &pool, &query_json).await {
                        Ok(result) => write_string_to_memory(
                            &memory,
                            &mut caller,
                            out_ptr,
                            out_max_len,
                            &result,
                        )
                        .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT),
                        Err(code) => code,
                    }
                })
            },
        )
        .into_anyhow()?;

    // insert(table, data_json, out) -> i32 (bytes written or error)
    linker
        .func_wrap_async_traced(
            "trovato:kernel/db",
            "insert",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (table_ptr, table_len, data_ptr, data_len, out_ptr, out_max_len): (
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

                    let Ok(table) = read_string_from_memory(&memory, &caller, table_ptr, table_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };

                    let Ok(data_json) =
                        read_string_from_memory(&memory, &caller, data_ptr, data_len)
                    else {
                        return host_errors::ERR_PARAM2_OR_OUTPUT;
                    };

                    let policy = caller.data().db_policy.clone();

                    let Some(services) = caller.data().request.services() else {
                        return host_errors::ERR_NO_SERVICES;
                    };
                    let pool = services.db.clone();

                    match do_insert(&policy, &pool, &table, &data_json).await {
                        Ok(result) => write_string_to_memory(
                            &memory,
                            &mut caller,
                            out_ptr,
                            out_max_len,
                            &result,
                        )
                        .unwrap_or(host_errors::ERR_PARAM3_READ),
                        Err(code) => code,
                    }
                })
            },
        )
        .into_anyhow()?;

    // update(table, data_json, where_json) -> i64 (rows affected or error)
    linker
        .func_wrap_async_traced(
            "trovato:kernel/db",
            "update",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (table_ptr, table_len, data_ptr, data_len, where_ptr, where_len): (
                i32,
                i32,
                i32,
                i32,
                i32,
                i32,
            )| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return i64::from(host_errors::ERR_MEMORY_MISSING);
                    };

                    let Ok(table) = read_string_from_memory(&memory, &caller, table_ptr, table_len)
                    else {
                        return i64::from(host_errors::ERR_PARAM1_READ);
                    };

                    let Ok(data_json) =
                        read_string_from_memory(&memory, &caller, data_ptr, data_len)
                    else {
                        return i64::from(host_errors::ERR_PARAM2_OR_OUTPUT);
                    };

                    let Ok(where_json) =
                        read_string_from_memory(&memory, &caller, where_ptr, where_len)
                    else {
                        return i64::from(host_errors::ERR_PARAM3_READ);
                    };

                    let policy = caller.data().db_policy.clone();

                    let Some(services) = caller.data().request.services() else {
                        return i64::from(host_errors::ERR_NO_SERVICES);
                    };
                    let pool = services.db.clone();

                    match do_update(&policy, &pool, &table, &data_json, &where_json).await {
                        Ok(rows) => rows as i64,
                        Err(code) => i64::from(code),
                    }
                })
            },
        )
        .into_anyhow()?;

    // delete(table, where_json) -> i64 (rows affected or error)
    linker
        .func_wrap_async_traced(
            "trovato:kernel/db",
            "delete",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (table_ptr, table_len, where_ptr, where_len): (i32, i32, i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return i64::from(host_errors::ERR_MEMORY_MISSING);
                    };

                    let Ok(table) = read_string_from_memory(&memory, &caller, table_ptr, table_len)
                    else {
                        return i64::from(host_errors::ERR_PARAM1_READ);
                    };

                    let Ok(where_json) =
                        read_string_from_memory(&memory, &caller, where_ptr, where_len)
                    else {
                        return i64::from(host_errors::ERR_PARAM2_OR_OUTPUT);
                    };

                    let policy = caller.data().db_policy.clone();

                    let Some(services) = caller.data().request.services() else {
                        return i64::from(host_errors::ERR_NO_SERVICES);
                    };
                    let pool = services.db.clone();

                    match do_delete(&policy, &pool, &table, &where_json).await {
                        Ok(rows) => rows as i64,
                        Err(code) => i64::from(code),
                    }
                })
            },
        )
        .into_anyhow()?;

    // query-raw(sql, params_json, out) -> i32 (bytes written or error)
    linker
        .func_wrap_async_traced(
            "trovato:kernel/db",
            "query-raw",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (sql_ptr, sql_len, params_ptr, params_len, out_ptr, out_max_len): (
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

                    // WASM-2 raw-SQL gate (D-19 §3): query-raw requires the
                    // declared raw_sql capability. Checked before any read/pool work.
                    if let Err(msg) = caller.data().db_policy.check_raw_sql() {
                        warn!(denied = %msg, "plugin query-raw rejected: raw_sql not declared");
                        return host_errors::ERR_RAW_SQL_NOT_DECLARED;
                    }

                    let Ok(sql) = read_string_from_memory(&memory, &caller, sql_ptr, sql_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };

                    let Ok(params_json) =
                        read_string_from_memory(&memory, &caller, params_ptr, params_len)
                    else {
                        return host_errors::ERR_PARAM2_OR_OUTPUT;
                    };

                    let Some(services) = caller.data().request.services() else {
                        return host_errors::ERR_NO_SERVICES;
                    };
                    let pool = services.db.clone();

                    let params: Vec<serde_json::Value> = match serde_json::from_str(&params_json) {
                        Ok(p) => p,
                        Err(_) => return host_errors::ERR_PARAM_DESERIALIZE,
                    };

                    match do_query_raw(&pool, &sql, &params).await {
                        Ok(result) => write_string_to_memory(
                            &memory,
                            &mut caller,
                            out_ptr,
                            out_max_len,
                            &result,
                        )
                        .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT),
                        Err(code) => code,
                    }
                })
            },
        )
        .into_anyhow()?;

    // execute-raw(sql, params_json) -> i64 (rows affected or error)
    linker
        .func_wrap_async_traced(
            "trovato:kernel/db",
            "execute-raw",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (sql_ptr, sql_len, params_ptr, params_len): (i32, i32, i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return i64::from(host_errors::ERR_MEMORY_MISSING);
                    };

                    // WASM-2 raw-SQL gate (D-19 §3): execute-raw requires the
                    // declared raw_sql capability. Checked before any read/pool work.
                    if let Err(msg) = caller.data().db_policy.check_raw_sql() {
                        warn!(denied = %msg, "plugin execute-raw rejected: raw_sql not declared");
                        return i64::from(host_errors::ERR_RAW_SQL_NOT_DECLARED);
                    }

                    let Ok(sql) = read_string_from_memory(&memory, &caller, sql_ptr, sql_len)
                    else {
                        return i64::from(host_errors::ERR_PARAM1_READ);
                    };

                    let Ok(params_json) =
                        read_string_from_memory(&memory, &caller, params_ptr, params_len)
                    else {
                        return i64::from(host_errors::ERR_PARAM2_OR_OUTPUT);
                    };

                    let Some(services) = caller.data().request.services() else {
                        return i64::from(host_errors::ERR_NO_SERVICES);
                    };
                    let pool = services.db.clone();

                    let params: Vec<serde_json::Value> = match serde_json::from_str(&params_json) {
                        Ok(p) => p,
                        Err(_) => return i64::from(host_errors::ERR_PARAM_DESERIALIZE),
                    };

                    match do_execute_raw(&pool, &sql, &params).await {
                        Ok(rows) => rows as i64,
                        Err(code) => i64::from(code),
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
    fn register_db_succeeds() {
        let config = wasmtime::Config::new();
        let engine = Engine::new(&config).unwrap();
        let mut linker: Linker<PluginState> = Linker::new(&engine);

        let result = register_db_functions(&mut linker);
        assert!(result.is_ok());
    }

    // ---- the raw-SQL guard ----
    //
    // These replace the three keyword-scanner tests (`ddl_guard_rejects_ddl`,
    // `ddl_guard_allows_dml`, `read_only_guard`), whose subject no longer
    // exists: the scanner they tested was the defect.

    /// The first defect the keyword scanner could not see: a statement that
    /// begins with `WITH` and writes in the middle of itself. PostgreSQL runs
    /// the `DELETE`; the old `is_read_only` saw the `WITH` and called it a read.
    #[test]
    fn query_raw_refuses_a_data_modifying_cte() {
        let err = sql_guard::check_query_raw(
            "WITH gone AS (DELETE FROM item RETURNING *) SELECT * FROM gone",
        )
        .expect_err("a writable CTE is not a read");
        assert_eq!(err, sql_guard::RawSqlReject::WriteInsideQuery);
    }

    /// The second: PostgreSQL nests block comments and the old scanner stopped
    /// at the first `*/`, so in this statement it read `SELECT` where the
    /// server reads `DROP TABLE`. Refused on both paths now, for the plain
    /// reason that it is neither a query nor a row change.
    #[test]
    fn both_paths_refuse_the_nested_comment_statement() {
        let sql = "/* /* */ SELECT 1 */ DROP TABLE item";
        assert_eq!(
            sql_guard::check_query_raw(sql).expect_err("DROP is not a read"),
            sql_guard::RawSqlReject::NotAQuery
        );
        assert_eq!(
            sql_guard::check_execute_raw(sql).expect_err("DROP is not a row change"),
            sql_guard::RawSqlReject::NotARowChange
        );
    }

    /// `execute-raw` used to take anything whose first word was not DDL, so a
    /// plain `SET` changed the session every later kernel query on that pooled
    /// connection ran under — the statement outlives the `COMMIT` the host
    /// wraps it in.
    #[test]
    fn execute_raw_takes_only_one_row_change() {
        for refused in [
            "SET search_path TO public",
            "SET ROLE postgres",
            "RESET ALL",
            "DO $$ BEGIN PERFORM 1; END $$",
            "LOCK TABLE item",
            "COPY item FROM '/etc/passwd'",
            "COMMENT ON TABLE item IS 'x'",
            "COMMIT",
            "VACUUM item",
            "TRUNCATE item",
            "CREATE TABLE sneaky (id int)",
            "SELECT 1",
        ] {
            assert!(
                sql_guard::check_execute_raw(refused).is_err(),
                "execute-raw must refuse: {refused}"
            );
        }
    }

    /// The same session change, reached through the read path: `set_config`
    /// with `is_local = false` outlives the transaction exactly as `SET` does.
    #[test]
    fn query_raw_refuses_the_session_changing_functions() {
        for refused in [
            "SELECT set_config('search_path', 'public', false)",
            "SELECT pg_advisory_lock(1)",
            "SELECT pg_read_file('/etc/passwd')",
            "SELECT pg_ls_dir('/')",
            "SELECT query_to_xml('select * from users', true, false, '')",
            "SELECT lo_import('/etc/passwd')",
            "SELECT pg_terminate_backend(1)",
        ] {
            assert!(
                sql_guard::check_query_raw(refused).is_err(),
                "query-raw must refuse: {refused}"
            );
        }
    }

    /// The allowed neighbours of those, so the denylist is a list and not a
    /// prefix sweep: the transaction-scoped advisory lock ends with the
    /// transaction, and a plugin is allowed to be slow.
    #[test]
    fn query_raw_allows_the_harmless_neighbours() {
        assert!(sql_guard::check_query_raw("SELECT pg_advisory_xact_lock(1)").is_ok());
        assert!(sql_guard::check_query_raw("SELECT pg_sleep(4)").is_ok());
    }

    /// A read that is not quite a read: `FOR UPDATE` takes a row lock, and
    /// `SELECT ... INTO` creates a table.
    #[test]
    fn query_raw_refuses_locks_and_select_into() {
        assert_eq!(
            sql_guard::check_query_raw("SELECT * FROM item FOR UPDATE").expect_err("lock"),
            sql_guard::RawSqlReject::RowLock
        );
        assert_eq!(
            sql_guard::check_query_raw("SELECT * INTO copied FROM item").expect_err("into"),
            sql_guard::RawSqlReject::SelectInto
        );
    }

    /// Raw SQL was never checked against any table list, so `raw_sql = true`
    /// was a credential read. It is checked now, anywhere in the tree —
    /// including inside a CTE, which is where a table name hides best.
    #[test]
    fn raw_sql_refuses_protected_tables_anywhere_in_the_tree() {
        for refused in [
            "SELECT * FROM users",
            "SELECT * FROM public.users",
            "WITH u AS (SELECT * FROM users) SELECT * FROM u",
            "SELECT * FROM item JOIN user_roles ON true",
            "SELECT value FROM site_config WHERE key = $1",
            "SELECT (SELECT count(*) FROM api_tokens) AS n",
        ] {
            assert!(
                matches!(
                    sql_guard::check_query_raw(refused),
                    Err(sql_guard::RawSqlReject::ProtectedTable(_))
                ),
                "query-raw must refuse: {refused}"
            );
        }
        assert!(matches!(
            sql_guard::check_execute_raw("UPDATE users SET name = 'x'"),
            Err(sql_guard::RawSqlReject::ProtectedTable(_))
        ));
    }

    /// Every raw statement a shipped plugin or test fixture sends, copied
    /// verbatim, accepted by the function that sends it.
    ///
    /// This is the test that says the guard did not break the site. The
    /// statements were collected by grepping `plugins/` for `query_raw` and
    /// `execute_raw`; a plugin that grows a new one and is not added here is a
    /// plugin whose SQL nobody checked.
    #[test]
    fn every_shipped_plugin_statement_is_still_accepted() {
        // query-raw
        for (plugin, sql) in [
            (
                "trovato_search",
                "SELECT COALESCE(MAX(changed), 0) as max_changed \
                 FROM item WHERE status = 1 AND stage_id = $1::uuid",
            ),
            (
                "trovato_search",
                "SELECT last_indexed_at, rebuild_requested \
                 FROM pagefind_index_status WHERE id = 1",
            ),
            (
                "trovato_series",
                "SELECT id::text, title FROM item \
                 WHERE type = 'blog' \
                 AND status = 1 \
                 AND fields->>'field_series_title' = $1 \
                 ORDER BY created ASC",
            ),
            (
                "trovato_book",
                "SELECT bp.item_id, bp.book_id, bp.parent_item_id, bp.weight, i.title \
                 FROM book_page bp JOIN item i ON i.id = bp.item_id \
                 WHERE bp.item_id = $1::uuid",
            ),
            (
                "trovato_book",
                "SELECT bp.item_id, bp.book_id, bp.parent_item_id, bp.weight, i.title \
                 FROM book_page bp JOIN item i ON i.id = bp.item_id \
                 WHERE bp.book_id = $1::uuid ORDER BY bp.weight, i.title",
            ),
            (
                "trovato_book",
                "SELECT bp.book_id, i.title, COUNT(*) AS pages \
                 FROM book_page bp JOIN item i ON i.id = bp.book_id \
                 GROUP BY bp.book_id, i.title ORDER BY i.title",
            ),
            ("trovato_book", "SELECT id FROM item WHERE id = $1::uuid"),
            (
                "test_plugin_api",
                "SELECT slug, text, method FROM tpa_notes WHERE user_id = $1::uuid ORDER BY slug",
            ),
            ("test_queue_worker", "SELECT pg_sleep(4)"),
            ("test_e2e_callee", "SELECT 1 FROM undeclared_table"),
        ] {
            assert!(
                sql_guard::check_query_raw(sql).is_ok(),
                "{plugin} sends this through query-raw and it must still run: {sql}"
            );
        }

        // execute-raw
        for (plugin, sql) in [
            (
                "trovato_scheduled_publishing",
                "UPDATE item SET status = 1, changed = $1 \
                 WHERE status = 0 \
                 AND fields->>'field_publish_on' IS NOT NULL \
                 AND (fields->>'field_publish_on') ~ '^[0-9]+$' \
                 AND (fields->>'field_publish_on')::bigint <= $1",
            ),
            (
                "trovato_scheduled_publishing",
                "UPDATE item SET status = 0, changed = $1 \
                 WHERE status = 1 \
                 AND fields->>'field_unpublish_on' IS NOT NULL \
                 AND (fields->>'field_unpublish_on') ~ '^[0-9]+$' \
                 AND (fields->>'field_unpublish_on')::bigint <= $1",
            ),
            (
                "trovato_search",
                "UPDATE pagefind_index_status SET rebuild_requested = true WHERE id = 1",
            ),
            (
                "trovato_book",
                "INSERT INTO book_page (item_id, book_id, parent_item_id, weight) \
                 VALUES ($1::uuid, $1::uuid, NULL, 0)",
            ),
            (
                "trovato_book",
                "INSERT INTO book_page (item_id, book_id, parent_item_id, weight) \
                 VALUES ($1::uuid, $2::uuid, $3::uuid, $4) \
                 ON CONFLICT (item_id) DO UPDATE SET \
                 book_id = EXCLUDED.book_id, parent_item_id = EXCLUDED.parent_item_id, \
                 weight = EXCLUDED.weight",
            ),
            (
                "trovato_book",
                "DELETE FROM book_page WHERE book_id = $1::uuid",
            ),
            (
                "trovato_book",
                "UPDATE book_page SET parent_item_id = $1::uuid WHERE parent_item_id = $2::uuid",
            ),
            (
                "trovato_book",
                "DELETE FROM book_page WHERE item_id = $1::uuid",
            ),
            (
                "test_plugin_api",
                "INSERT INTO tpa_notes (user_id, slug, text, method) VALUES ($1::uuid, $2, $3, $4) \
                 ON CONFLICT (user_id, slug) DO UPDATE SET text = EXCLUDED.text",
            ),
        ] {
            assert!(
                sql_guard::check_execute_raw(sql).is_ok(),
                "{plugin} sends this through execute-raw and it must still run: {sql}"
            );
        }
    }

    /// The one shipped statement this change does stop. `trovato_ai` reads its
    /// field rules from `site_config`, which holds the SMTP password, so the
    /// table is on the protected floor and the read is refused. The plugin
    /// already handles the error (it logs the code and returns no rules), and
    /// the query was already failing for its own reason: it filters on a column
    /// named `name` and the column is `key`.
    #[test]
    fn the_trovato_ai_site_config_read_is_refused_and_this_is_deliberate() {
        assert!(matches!(
            sql_guard::check_query_raw("SELECT value FROM site_config WHERE name = $1"),
            Err(sql_guard::RawSqlReject::ProtectedTable(_))
        ));
    }

    /// A statement the guard cannot parse is refused rather than passed to the
    /// server to interpret: if this kernel cannot say what a statement does,
    /// it is not in a position to allow it.
    #[test]
    fn unparseable_and_multiple_statements_are_refused() {
        assert!(matches!(
            sql_guard::check_query_raw("SELEKT 1"),
            Err(sql_guard::RawSqlReject::Unparseable(_))
        ));
        // The semicolon check in `do_query_raw` catches this first in
        // production; the guard refuses it on its own too.
        assert!(sql_guard::check_query_raw("SELECT 1; SELECT 2").is_err());
    }

    /// A lazy, never-connected pool: the WASM-2 table gate rejects before any
    /// query, so these tests need no live Postgres.
    fn lazy_pool() -> PgPool {
        PgPool::connect_lazy("postgres://localhost/trovato").expect("lazy pool")
    }

    /// A policy allowing exactly `owned` (a migration-owned table) with raw SQL
    /// disabled — the shape a `db`-declaring, non-raw plugin gets.
    fn policy_allowing(table: &str) -> DbPolicy {
        DbPolicy::from_parts("probe", [table.to_string()], false)
    }

    #[tokio::test]
    async fn do_select_rejects_undeclared_table() {
        let policy = policy_allowing("owned_items");
        let q = r#"{"table":"users","columns":["id"]}"#;
        assert_eq!(
            do_select(&policy, &lazy_pool(), q).await.unwrap_err(),
            host_errors::ERR_TABLE_NOT_DECLARED
        );
    }

    #[tokio::test]
    async fn do_insert_rejects_undeclared_table() {
        let policy = policy_allowing("owned_items");
        assert_eq!(
            do_insert(&policy, &lazy_pool(), "users", r#"{"name":"x"}"#)
                .await
                .unwrap_err(),
            host_errors::ERR_TABLE_NOT_DECLARED
        );
    }

    #[tokio::test]
    async fn do_update_rejects_undeclared_table() {
        let policy = policy_allowing("owned_items");
        assert_eq!(
            do_update(
                &policy,
                &lazy_pool(),
                "users",
                r#"{"name":"x"}"#,
                r#"{"id":1}"#
            )
            .await
            .unwrap_err(),
            host_errors::ERR_TABLE_NOT_DECLARED
        );
    }

    #[tokio::test]
    async fn do_delete_rejects_undeclared_table() {
        let policy = policy_allowing("owned_items");
        assert_eq!(
            do_delete(&policy, &lazy_pool(), "users", r#"{"id":1}"#)
                .await
                .unwrap_err(),
            host_errors::ERR_TABLE_NOT_DECLARED
        );
    }

    #[test]
    fn enforce_table_maps_allow_and_deny_to_codes() {
        let policy = policy_allowing("owned_items");
        // Migration-owned/declared table passes the gate.
        assert!(enforce_table(&policy, "owned_items").is_ok());
        // Anything else maps to the ABI code.
        assert_eq!(
            enforce_table(&policy, "secrets").unwrap_err(),
            host_errors::ERR_TABLE_NOT_DECLARED
        );
    }

    #[test]
    fn valid_identifier_regex() {
        assert!(VALID_IDENTIFIER.is_match("item"));
        assert!(VALID_IDENTIFIER.is_match("_private"));
        assert!(VALID_IDENTIFIER.is_match("Content_Type_2"));
        assert!(!VALID_IDENTIFIER.is_match("1bad"));
        assert!(!VALID_IDENTIFIER.is_match("no spaces"));
        assert!(!VALID_IDENTIFIER.is_match("no-dashes"));
        assert!(!VALID_IDENTIFIER.is_match("no.dots"));
        assert!(!VALID_IDENTIFIER.is_match(""));
        assert!(!VALID_IDENTIFIER.is_match("Robert'; DROP TABLE students;--"));
    }
}
