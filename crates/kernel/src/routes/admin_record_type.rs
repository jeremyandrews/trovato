//! Admin read surfaces for lightweight-record types (P11g / D-58).
//!
//! The 1.0 minimum for the lightweight-record tier is **list + view** — making
//! plugin-owned rows visible in the admin UI. Full CRUD stays Item-only (D-58):
//! the plugin owns writes to its own table through the `db` host capability; the
//! kernel owns the read surfaces.
//!
//! Three read-only, admin-gated routes:
//! - `GET /admin/structure/records` — the registered record types.
//! - `GET /admin/structure/records/{type}` — rows of one record type.
//! - `GET /admin/structure/records/{type}/{id}` — one record row.
//!
//! Every SQL identifier interpolated below (`table`, `id_column`, `title_column`,
//! `changed_column`) comes from a [`RecordTypeDef`](crate::content::RecordTypeDef)
//! that was validated as a safe SQL identifier at manifest parse and admitted to
//! the registry only after the effective-allowlist cross-check — so interpolation
//! carries no injection surface. The record id is always a bound parameter.
//!
//! Neither read surface assumes anything about the *type* of the declared id
//! column: both compare it as text, so a record type keyed by a uuid, a bigint,
//! or any other scalar lists and opens through the same route.

use axum::Router;
use axum::extract::{Path, State};
use axum::response::Response;
use axum::routing::get;
use tower_sessions::Session;

use crate::state::AppState;

use super::helpers::{render_not_found, render_server_error, require_admin};

/// Maximum record rows shown on the admin list surface.
const RECORD_LIST_LIMIT: i64 = 200;

/// List all registered lightweight-record types.
///
/// GET /admin/structure/records
async fn list_record_types(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let types: Vec<serde_json::Value> = state
        .record_types()
        .list()
        .into_iter()
        .map(|def| {
            serde_json::json!({
                "name": def.name,
                "plugin": def.plugin,
                "table": def.table,
                "published": def.published_column.is_some(),
                "field_count": def.field_map.len(),
            })
        })
        .collect();

    let mut context = tera::Context::new();
    context.insert("record_types", &types);
    context.insert("path", "/admin/structure/records");
    super::helpers::render_admin_template(&state, "admin/record-types.html", context).await
}

/// List the rows of one lightweight-record type.
///
/// GET /admin/structure/records/{type}
async fn list_records(
    State(state): State<AppState>,
    session: Session,
    Path(type_name): Path<String>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let Some(def) = state.record_types().get(&type_name) else {
        return render_not_found();
    };

    // Identifiers are registry-validated safe identifiers; id is projected as
    // text for a stable string key. Admin surface: every row, no published gate.
    let sql = format!(
        "SELECT row_to_json(t) FROM (\
           SELECT {id}::text AS id, {title} AS title \
           FROM {table} ORDER BY {changed} DESC LIMIT {limit}\
         ) t",
        id = def.id_column,
        title = def.title_column,
        table = def.table,
        changed = def.changed_column,
        limit = RECORD_LIST_LIMIT,
    );

    let rows: Vec<serde_json::Value> = match sqlx::query_scalar(&sql).fetch_all(state.db()).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(error = %e, record_type = %type_name, "failed to list records");
            return render_server_error("Failed to list records");
        }
    };

    let mut context = tera::Context::new();
    context.insert("record_type", &type_name);
    context.insert("plugin", &def.plugin);
    context.insert("records", &rows);
    context.insert("truncated", &(rows.len() as i64 >= RECORD_LIST_LIMIT));
    context.insert("path", "/admin/structure/records");
    super::helpers::render_admin_template(&state, "admin/record-list.html", context).await
}

/// View one lightweight-record row.
///
/// GET /admin/structure/records/{type}/{id}
async fn view_record(
    State(state): State<AppState>,
    session: Session,
    Path((type_name, id)): Path<(String, String)>,
) -> Response {
    if let Err(redirect) = require_admin(&state, &session).await {
        return redirect;
    }

    let Some(def) = state.record_types().get(&type_name) else {
        return render_not_found();
    };

    // Registry-validated identifiers; the id is a bound parameter.
    //
    // The id is compared **as text**, which is what lets one route serve a
    // record type keyed by a uuid, a bigint, or any other scalar: the registry
    // declares the key column, and nothing here assumes its type. The list
    // route already projects `{id}::text` as the row key, so the ids this
    // compares against are exactly the ids that route links to.
    let sql = format!(
        "SELECT row_to_json(t) FROM (SELECT * FROM {table} WHERE {id_col}::text = $1) t",
        table = def.table,
        id_col = def.id_column,
    );

    // A uuid-shaped segment is normalized to the canonical lowercase hyphenated
    // form Postgres renders `uuid::text` as, so a uuid-keyed type still opens
    // from an uppercase, braced or unhyphenated spelling — the ones the uuid
    // extractor used to accept. Anything that is not a uuid is compared exactly
    // as it arrived.
    let id_param = uuid::Uuid::parse_str(&id).map_or_else(|_| id.clone(), |u| u.to_string());

    let row: Option<serde_json::Value> = match sqlx::query_scalar(&sql)
        .bind(&id_param)
        .fetch_optional(state.db())
        .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::error!(error = %e, record_type = %type_name, "failed to load record");
            return render_server_error("Failed to load record");
        }
    };

    let Some(row) = row else {
        return render_not_found();
    };

    // Render the row as ordered {column, value} pairs for a stable table.
    let mut pairs: Vec<serde_json::Value> = Vec::new();
    if let Some(obj) = row.as_object() {
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        for k in keys {
            let display = match &obj[k] {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            };
            pairs.push(serde_json::json!({ "column": k, "value": display }));
        }
    }

    let mut context = tera::Context::new();
    context.insert("record_type", &type_name);
    context.insert("plugin", &def.plugin);
    context.insert("record_id", &id);
    context.insert(
        "title",
        &row.get(&def.title_column)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    );
    context.insert("fields", &pairs);
    context.insert("path", "/admin/structure/records");
    super::helpers::render_admin_template(&state, "admin/record-view.html", context).await
}

/// Build the lightweight-record admin router (P11g / D-58, read-only).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/structure/records", get(list_record_types))
        .route("/admin/structure/records/{type}", get(list_records))
        .route("/admin/structure/records/{type}/{id}", get(view_record))
}
