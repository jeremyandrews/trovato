//! The audit log report: `/admin/reports/audit`.
//!
//! Pages through the `trovato_audit_log` plugin's `audit_log` table, newest
//! first, with the admin list macros and the shared pager. The rows are
//! written by the write services through
//! [`AuditTrail`](crate::services::audit::AuditTrail). Gated on the plugin,
//! and on its `view audit log` permission.

use axum::Router;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use serde::{Deserialize, Serialize};
use tower_sessions::Session;
use uuid::Uuid;

use crate::state::AppState;

use super::helpers::{render_admin_template, render_server_error, require_permission};

/// The plugin's permission.
const PERMISSION: &str = "view audit log";

/// Rows per page.
const PER_PAGE: i64 = 50;

pub fn router() -> Router<AppState> {
    Router::new().route("/admin/reports/audit", get(audit_report))
}

#[derive(Debug, Deserialize)]
struct AuditQuery {
    page: Option<i64>,
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
    Query(query): Query<AuditQuery>,
) -> Response {
    let user = match require_permission(&state, &session, PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let page = query.page.unwrap_or(1).max(1);

    let total: i64 = match sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(state.db())
        .await
    {
        Ok(total) => total,
        Err(e) => {
            tracing::error!(error = %e, "failed to count the audit log");
            return render_server_error("The audit log could not be read.");
        }
    };
    let rows = match sqlx::query_as::<_, AuditRow>(
        "SELECT a.created, a.action, a.entity_type, a.entity_id, a.user_id, \
                u.name AS user_name, a.ip_address, a.details \
         FROM audit_log a LEFT JOIN users u ON u.id = a.user_id \
         ORDER BY a.created DESC, a.id DESC LIMIT $1 OFFSET $2",
    )
    .bind(PER_PAGE)
    .bind((page - 1) * PER_PAGE)
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
    context.insert("entries", &rows);
    context.insert("total", &total);
    context.insert("page", &page);
    context.insert("total_pages", &((total + PER_PAGE - 1) / PER_PAGE).max(1));
    context.insert("path", "/admin/reports/audit");
    render_admin_template(&state, &viewer, "admin/reports/audit.html", context).await
}
