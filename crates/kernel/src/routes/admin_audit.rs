//! The audit log report: `/admin/reports/audit`.
//!
//! Pages through the `trovato_audit_log` plugin's `audit_log` table, newest
//! first unless a column is sorted, with the admin listing pattern
//! ([`super::admin_list`]): a text filter over the action, type, ID and user
//! name, sortable columns and the shared pager. The rows are written by the
//! write services through [`AuditTrail`](crate::services::audit::AuditTrail).
//! Gated on the plugin, and on its `view audit log` permission.

use std::collections::HashMap;

use axum::Router;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use serde::Serialize;
use sqlx::AssertSqlSafe;
use tower_sessions::Session;
use uuid::Uuid;

use crate::state::AppState;

use super::admin_list::{ListQuery, PER_PAGE};
use super::helpers::{render_admin_template, render_server_error, require_permission};

/// The plugin's permission.
const PERMISSION: &str = "view audit log";

/// The audit log's sortable columns: key and SQL.
const AUDIT_SORTS: &[(&str, &str)] = &[
    ("time", "a.created"),
    ("user", "lower(u.name)"),
    ("action", "a.action"),
    ("type", "a.entity_type"),
];

/// The text filter's condition, its pattern bound as `$1`.
const AUDIT_FILTER_SQL: &str = " WHERE (a.action ILIKE $1 OR a.entity_type ILIKE $1 \
     OR a.entity_id ILIKE $1 OR u.name ILIKE $1)";

pub fn router() -> Router<AppState> {
    Router::new().route("/admin/reports/audit", get(audit_report))
}

/// One row as the template shows it.
#[derive(Debug, Serialize, sqlx::FromRow)]
struct AuditRow {
    created: i64,
    action: String,
    entity_type: String,
    entity_id: String,
    user_id: Option<Uuid>,
    user_name: Option<String>,
    ip_address: String,
    details: serde_json::Value,
}

/// GET /admin/reports/audit
async fn audit_report(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;

    let query = ListQuery::from_params(&params);
    let sort = query.sort(AUDIT_SORTS, ("time", true));
    let like = query.like();
    let condition = if like.is_some() { AUDIT_FILTER_SQL } else { "" };
    let n = usize::from(like.is_some());

    let count_sql = format!(
        "SELECT COUNT(*) FROM audit_log a LEFT JOIN users u ON u.id = a.user_id{condition}"
    );
    let mut count = sqlx::query_scalar::<_, i64>(AssertSqlSafe(count_sql));
    // Ties: newest first, then the id; a sort by time needs only the id.
    let order_by = sort.order_by();
    let ties = if order_by.starts_with("a.created ") {
        "a.id DESC"
    } else {
        "a.created DESC, a.id DESC"
    };
    let list_sql = format!(
        "SELECT a.created, a.action, a.entity_type, a.entity_id, a.user_id, \
                u.name AS user_name, a.ip_address, a.details \
         FROM audit_log a LEFT JOIN users u ON u.id = a.user_id{condition} \
         ORDER BY {order_by}, {ties} LIMIT ${} OFFSET ${}",
        n + 1,
        n + 2
    );
    let mut list = sqlx::query_as::<_, AuditRow>(AssertSqlSafe(list_sql));
    if let Some(like) = &like {
        count = count.bind(like);
        list = list.bind(like);
    }

    let total = match count.fetch_one(state.db()).await {
        Ok(total) => total,
        Err(e) => {
            tracing::error!(error = %e, "failed to count the audit log");
            return render_server_error("The audit log could not be read.");
        }
    };
    let rows = match list
        .bind(PER_PAGE)
        .bind(query.offset())
        .fetch_all(state.db())
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(error = %e, "failed to read the audit log");
            return render_server_error("The audit log could not be read.");
        }
    };

    let mut context = tera::Context::new();
    context.insert("list", &query.view(total, rows.len(), Some(&sort)));
    context.insert("entries", &rows);
    context.insert("path", "/admin/reports/audit");
    render_admin_template(&state, &viewer, "admin/reports/audit.html", context).await
}
