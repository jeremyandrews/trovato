//! Database host functions for WASM plugins.
//!
//! Provides both raw and structured database access with DDL guards
//! to prevent schema modification from plugins. All queries use
//! JSON-encoded parameters and return JSON results.

use anyhow::Result;
use regex::Regex;
use sqlx::postgres::PgArguments;
use sqlx::{Column, PgPool, Row, TypeInfo};
use std::sync::LazyLock;
use tracing::warn;
use trovato_sdk::host_errors;
use wasmtime::Linker;

use super::{read_string_from_memory, write_string_to_memory};
use crate::plugin::PluginState;

/// Regex for valid SQL identifiers (table/column names).
///
/// # Panics
///
/// Panics if the hard-coded regex literal is invalid (impossible in practice).
#[allow(clippy::expect_used)]
static VALID_IDENTIFIER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-zA-Z_][a-zA-Z0-9_]*$").expect("valid regex literal"));

/// DDL keywords that `execute-raw` must reject.
const DDL_KEYWORDS: &[&str] = &["CREATE", "DROP", "ALTER", "TRUNCATE", "GRANT", "REVOKE"];

/// Check if SQL starts with one of the DDL keywords (after trimming whitespace).
fn is_ddl(sql: &str) -> bool {
    let first_word = sql.split_whitespace().next().unwrap_or("");
    DDL_KEYWORDS
        .iter()
        .any(|kw| first_word.eq_ignore_ascii_case(kw))
}

/// Check if SQL is a read-only statement (SELECT or WITH).
fn is_read_only(sql: &str) -> bool {
    let first_word = sql.split_whitespace().next().unwrap_or("");
    first_word.eq_ignore_ascii_case("SELECT") || first_word.eq_ignore_ascii_case("WITH")
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
    if !is_read_only(sql) {
        return Err(host_errors::ERR_DDL_REJECTED);
    }

    fetch_rows_as_json(pool, sql, params).await
}

/// Execute a SQL statement that returns rows and serialize them as JSON.
///
/// Shared implementation for `do_query_raw` (after guard) and `do_insert` (RETURNING *).
async fn fetch_rows_as_json(
    pool: &PgPool,
    sql: &str,
    params: &[serde_json::Value],
) -> std::result::Result<String, i32> {
    let query = sqlx::query(sql);
    let query = bind_json_params(params, query);

    let rows = query.fetch_all(pool).await.map_err(|e| {
        warn!(error = %e, sql = sql, "plugin query failed");
        host_errors::ERR_SQL_FAILED
    })?;

    let json_rows: Vec<serde_json::Value> = rows.iter().map(row_to_json).collect();
    serde_json::to_string(&json_rows).map_err(|_| host_errors::ERR_SERIALIZE_FAILED)
}

/// Execute a DML statement and return rows affected.
async fn do_execute_raw(
    pool: &PgPool,
    sql: &str,
    params: &[serde_json::Value],
) -> std::result::Result<u64, i32> {
    if is_ddl(sql) {
        return Err(host_errors::ERR_DDL_REJECTED);
    }

    let query = sqlx::query(sql);
    let query = bind_json_params(params, query);

    let result = query.execute(pool).await.map_err(|e| {
        warn!(error = %e, sql = sql, "plugin execute-raw failed");
        host_errors::ERR_SQL_FAILED
    })?;

    Ok(result.rows_affected())
}

/// Build and execute a structured SELECT query.
async fn do_select(pool: &PgPool, query_json: &str) -> std::result::Result<String, i32> {
    let query: SelectQuery =
        serde_json::from_str(query_json).map_err(|_| host_errors::ERR_PARAM_DESERIALIZE)?;

    if !VALID_IDENTIFIER.is_match(&query.table) {
        return Err(host_errors::ERR_INVALID_IDENTIFIER);
    }

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
    pool: &PgPool,
    table: &str,
    data_json: &str,
) -> std::result::Result<String, i32> {
    if !VALID_IDENTIFIER.is_match(table) {
        return Err(host_errors::ERR_INVALID_IDENTIFIER);
    }

    let data: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(data_json).map_err(|_| host_errors::ERR_PARAM_DESERIALIZE)?;

    if data.is_empty() {
        return Err(host_errors::ERR_PARAM_DESERIALIZE);
    }

    let mut columns = Vec::new();
    let mut placeholders = Vec::new();
    let mut params = Vec::new();
    let mut idx = 1;

    for (col, val) in &data {
        if !VALID_IDENTIFIER.is_match(col) {
            return Err(host_errors::ERR_INVALID_IDENTIFIER);
        }
        columns.push(col.as_str());
        placeholders.push(format!("${idx}"));
        params.push(val.clone());
        idx += 1;
    }

    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({}) RETURNING *",
        table,
        columns.join(", "),
        placeholders.join(", ")
    );

    // Bypass read-only guard since INSERT RETURNING needs row results.
    fetch_rows_as_json(pool, &sql, &params).await
}

/// Build and execute a structured UPDATE.
async fn do_update(
    pool: &PgPool,
    table: &str,
    data_json: &str,
    where_json: &str,
) -> std::result::Result<u64, i32> {
    if !VALID_IDENTIFIER.is_match(table) {
        return Err(host_errors::ERR_INVALID_IDENTIFIER);
    }

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
async fn do_delete(pool: &PgPool, table: &str, where_json: &str) -> std::result::Result<u64, i32> {
    if !VALID_IDENTIFIER.is_match(table) {
        return Err(host_errors::ERR_INVALID_IDENTIFIER);
    }

    let where_map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(where_json).map_err(|_| host_errors::ERR_PARAM_DESERIALIZE)?;

    if where_map.is_empty() {
        return Err(host_errors::ERR_PARAM_DESERIALIZE);
    }

    let mut where_parts = Vec::new();
    let mut params = Vec::new();
    let mut idx = 1;

    for (col, val) in &where_map {
        if !VALID_IDENTIFIER.is_match(col) {
            return Err(host_errors::ERR_INVALID_IDENTIFIER);
        }
        where_parts.push(format!("{col} = ${idx}"));
        params.push(val.clone());
        idx += 1;
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
    linker.func_wrap_async(
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

                let Some(services) = caller.data().request.services() else {
                    return host_errors::ERR_NO_SERVICES;
                };
                let pool = services.db.clone();

                match do_select(&pool, &query_json).await {
                    Ok(result) => {
                        write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &result)
                            .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT)
                    }
                    Err(code) => code,
                }
            })
        },
    )?;

    // insert(table, data_json, out) -> i32 (bytes written or error)
    linker.func_wrap_async(
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

                let Ok(data_json) = read_string_from_memory(&memory, &caller, data_ptr, data_len)
                else {
                    return host_errors::ERR_PARAM2_OR_OUTPUT;
                };

                let Some(services) = caller.data().request.services() else {
                    return host_errors::ERR_NO_SERVICES;
                };
                let pool = services.db.clone();

                match do_insert(&pool, &table, &data_json).await {
                    Ok(result) => {
                        write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &result)
                            .unwrap_or(host_errors::ERR_PARAM3_READ)
                    }
                    Err(code) => code,
                }
            })
        },
    )?;

    // update(table, data_json, where_json) -> i64 (rows affected or error)
    linker.func_wrap_async(
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

                let Ok(data_json) = read_string_from_memory(&memory, &caller, data_ptr, data_len)
                else {
                    return i64::from(host_errors::ERR_PARAM2_OR_OUTPUT);
                };

                let Ok(where_json) =
                    read_string_from_memory(&memory, &caller, where_ptr, where_len)
                else {
                    return i64::from(host_errors::ERR_PARAM3_READ);
                };

                let Some(services) = caller.data().request.services() else {
                    return i64::from(host_errors::ERR_NO_SERVICES);
                };
                let pool = services.db.clone();

                match do_update(&pool, &table, &data_json, &where_json).await {
                    Ok(rows) => rows as i64,
                    Err(code) => i64::from(code),
                }
            })
        },
    )?;

    // delete(table, where_json) -> i64 (rows affected or error)
    linker.func_wrap_async(
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

                let Some(services) = caller.data().request.services() else {
                    return i64::from(host_errors::ERR_NO_SERVICES);
                };
                let pool = services.db.clone();

                match do_delete(&pool, &table, &where_json).await {
                    Ok(rows) => rows as i64,
                    Err(code) => i64::from(code),
                }
            })
        },
    )?;

    // query-raw(sql, params_json, out) -> i32 (bytes written or error)
    linker.func_wrap_async(
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

                let Ok(sql) = read_string_from_memory(&memory, &caller, sql_ptr, sql_len) else {
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
                    Ok(result) => {
                        write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &result)
                            .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT)
                    }
                    Err(code) => code,
                }
            })
        },
    )?;

    // execute-raw(sql, params_json) -> i64 (rows affected or error)
    linker.func_wrap_async(
        "trovato:kernel/db",
        "execute-raw",
        |mut caller: wasmtime::Caller<'_, PluginState>,
         (sql_ptr, sql_len, params_ptr, params_len): (i32, i32, i32, i32)| {
            Box::new(async move {
                let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                    return i64::from(host_errors::ERR_MEMORY_MISSING);
                };

                let Ok(sql) = read_string_from_memory(&memory, &caller, sql_ptr, sql_len) else {
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
    )?;

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
        let mut config = wasmtime::Config::new();
        config.async_support(true);
        let engine = Engine::new(&config).unwrap();
        let mut linker: Linker<PluginState> = Linker::new(&engine);

        let result = register_db_functions(&mut linker);
        assert!(result.is_ok());
    }

    #[test]
    fn ddl_guard_rejects_ddl() {
        assert!(is_ddl("CREATE TABLE foo (id int)"));
        assert!(is_ddl("  DROP TABLE foo"));
        assert!(is_ddl("ALTER TABLE foo ADD COLUMN bar int"));
        assert!(is_ddl("TRUNCATE foo"));
        assert!(is_ddl("GRANT ALL ON foo TO bar"));
        assert!(is_ddl("REVOKE ALL ON foo FROM bar"));
    }

    #[test]
    fn ddl_guard_allows_dml() {
        assert!(!is_ddl("INSERT INTO foo VALUES (1)"));
        assert!(!is_ddl("UPDATE foo SET bar = 1"));
        assert!(!is_ddl("DELETE FROM foo WHERE id = 1"));
        assert!(!is_ddl("SELECT * FROM foo"));
        assert!(!is_ddl("WITH cte AS (SELECT 1) SELECT * FROM cte"));
    }

    #[test]
    fn read_only_guard() {
        assert!(is_read_only("SELECT * FROM foo"));
        assert!(is_read_only("  SELECT 1"));
        assert!(is_read_only("WITH cte AS (SELECT 1) SELECT * FROM cte"));
        assert!(!is_read_only("INSERT INTO foo VALUES (1)"));
        assert!(!is_read_only("UPDATE foo SET bar = 1"));
        assert!(!is_read_only("DELETE FROM foo"));
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
