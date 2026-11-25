//! The mail log (BG18 part 2, `docs/design/mail-queue.md` §9).
//!
//! For the network, under `administer site`:
//!
//! - `GET /admin/reports/mail`: every message, filtered by status, kind,
//!   blog, category and recipient, newest first, with a count per status;
//! - `GET /admin/reports/mail/{id}`: one message, its headers, body, attempts
//!   and last error, with **Retry** (`POST .../retry`, a failed or cancelled
//!   message back in the queue) and **Cancel** (`POST .../cancel`, a queued
//!   one);
//! - `GET /admin/reports/mail/suppressions`: the addresses no blog mails, with
//!   **Remove** and a form to add one by hand.
//!
//! For a blog owner, under `view own blog mail` and an active membership of
//! the blog (`user_tenant`): `GET /admin/mail`, the same list for the blog the
//! request is for and no other, without message bodies or the suppression
//! list. Every action is a form POST with the session's CSRF token; nothing
//! needs JavaScript.

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Form, Router};
use serde::Deserialize;
use tower_sessions::Session;
use uuid::Uuid;

use crate::form::csrf::generate_csrf_token;
use crate::models::User;
use crate::models::tenant::TenantContext;
use crate::services::mail_log::{self, KINDS, MailFilter, STATUSES};
use crate::state::AppState;

use super::helpers::{
    CsrfOnlyForm, render_admin_template, render_error, render_forbidden, render_not_found,
    render_server_error, require_csrf, require_permission,
};

/// What the network pages take.
const NETWORK_PERMISSION: &str = "administer site";

/// What the blog owner page takes, with a membership of the blog.
const OWNER_PERMISSION: &str = "view own blog mail";

/// Session key for the message an action leaves for the next page.
const FLASH_KEY: &str = "mail_report_flash";

/// Rows per page.
const PER_PAGE: i64 = 50;

/// Longest filter value kept, in characters.
const MAX_FILTER_CHARS: usize = 320;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/reports/mail", get(network_list))
        .route(
            "/admin/reports/mail/suppressions",
            get(suppression_list).post(suppress_address),
        )
        .route(
            "/admin/reports/mail/suppressions/remove",
            post(remove_suppression),
        )
        .route("/admin/reports/mail/{id}", get(message_detail))
        .route("/admin/reports/mail/{id}/retry", post(retry_message))
        .route("/admin/reports/mail/{id}/cancel", post(cancel_message))
        .route("/admin/mail", get(owner_list))
}

/// The list's query string.
#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    #[serde(default)]
    status: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    blog: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    recipient: String,
    page: Option<i64>,
}

/// A filter value, trimmed and bounded, or `None` when empty.
fn text_filter(value: &str) -> Option<String> {
    Some(
        value
            .trim()
            .chars()
            .take(MAX_FILTER_CHARS)
            .collect::<String>(),
    )
    .filter(|v| !v.is_empty())
}

/// `value` when it is one of `allowed`.
fn choice(value: &str, allowed: &[&str]) -> Option<String> {
    allowed.contains(&value).then(|| value.to_string())
}

impl ListQuery {
    /// The filter this query asks for. `tenant` is the owner view's blog,
    /// which no query string can change.
    fn filter(&self, tenant: Option<Uuid>) -> MailFilter {
        MailFilter {
            tenant: tenant.or_else(|| Uuid::parse_str(self.blog.trim()).ok()),
            status: choice(&self.status, STATUSES),
            kind: choice(&self.kind, KINDS),
            category: text_filter(&self.category),
            recipient: text_filter(&self.recipient),
        }
    }
}

/// The query string that keeps `filter` across pager and status links,
/// without the status (each link sets its own) and without the page.
fn filter_qs(filter: &MailFilter, owner: bool) -> String {
    let mut pairs: Vec<(&str, String)> = Vec::new();
    if !owner && let Some(tenant) = filter.tenant {
        pairs.push(("blog", tenant.to_string()));
    }
    for (name, value) in [
        ("kind", &filter.kind),
        ("category", &filter.category),
        ("recipient", &filter.recipient),
    ] {
        if let Some(value) = value {
            pairs.push((name, value.clone()));
        }
    }
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Render the list for `filter`: the network's (`owner: false`) or a blog
/// owner's.
async fn render_list(
    state: &AppState,
    session: &Session,
    user: &User,
    query: &ListQuery,
    filter: MailFilter,
    owner: bool,
) -> Response {
    let viewer = super::helpers::user_context_for(state, user).await;
    let page = query.page.unwrap_or(1).clamp(1, 100_000);
    let pool = state.db();
    let rows = mail_log::list(pool, &filter, PER_PAGE, (page - 1) * PER_PAGE);
    let counts = mail_log::status_counts(pool, &filter);
    // The blog filter's choices; the owner view has no blog filter.
    let blogs = async {
        if owner {
            Ok(Vec::new())
        } else {
            mail_log::blogs(pool).await
        }
    };
    let (rows, counts, blogs) = match tokio::try_join!(rows, counts, blogs) {
        Ok(found) => found,
        Err(e) => {
            tracing::error!(error = %e, "failed to read the mail outbox");
            return render_server_error("The mail log could not be read.");
        }
    };
    let total: i64 = match &filter.status {
        Some(status) => counts
            .iter()
            .find(|(s, _)| s == status)
            .map_or(0, |(_, n)| *n),
        None => counts.iter().map(|(_, n)| n).sum(),
    };
    let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();
    let path = if owner {
        "/admin/mail"
    } else {
        "/admin/reports/mail"
    };

    let mut context = tera::Context::new();
    context.insert("owner", &owner);
    context.insert(
        "retention_days",
        &state
            .email()
            .map_or(90, |email| email.config().log_retention_days),
    );
    context.insert("rows", &rows);
    context.insert(
        "counts",
        &counts
            .iter()
            .map(|(status, count)| serde_json::json!({"status": status, "count": count}))
            .collect::<Vec<_>>(),
    );
    context.insert("all_count", &counts.iter().map(|(_, n)| n).sum::<i64>());
    context.insert("total", &total);
    context.insert("page", &page);
    context.insert("total_pages", &((total + PER_PAGE - 1) / PER_PAGE).max(1));
    context.insert("statuses", STATUSES);
    context.insert("kinds", KINDS);
    context.insert(
        "blogs",
        &blogs
            .iter()
            .map(|(id, name)| serde_json::json!({"id": id.to_string(), "name": name}))
            .collect::<Vec<_>>(),
    );
    context.insert("status_filter", &filter.status.clone().unwrap_or_default());
    context.insert("kind_filter", &filter.kind.clone().unwrap_or_default());
    context.insert(
        "blog_filter",
        &filter.tenant.map(|t| t.to_string()).unwrap_or_default(),
    );
    context.insert(
        "category_filter",
        &filter.category.clone().unwrap_or_default(),
    );
    context.insert(
        "recipient_filter",
        &filter.recipient.clone().unwrap_or_default(),
    );
    context.insert("filter_qs", &filter_qs(&filter, owner));
    context.insert("flash", &flash);
    context.insert("path", path);
    render_admin_template(state, &viewer, "admin/reports/mail.html", context).await
}

/// GET /admin/reports/mail
async fn network_list(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<ListQuery>,
) -> Response {
    let user = match require_permission(&state, &session, NETWORK_PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let filter = query.filter(None);
    render_list(&state, &session, &user, &query, filter, false).await
}

/// Whether `user` may see `tenant`'s mail as its owner: the permission, and
/// an active membership of the blog. An administrator may see every blog's.
async fn owns_blog(state: &AppState, user: &User, tenant: Uuid) -> anyhow::Result<bool> {
    if user.is_admin {
        return Ok(true);
    }
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM user_tenant \
         WHERE user_id = $1 AND tenant_id = $2 AND is_active IS TRUE)",
    )
    .bind(user.id)
    .bind(tenant)
    .fetch_one(state.db())
    .await?)
}

/// GET /admin/mail: the blog owner's list, for the blog the request is for.
async fn owner_list(
    State(state): State<AppState>,
    session: Session,
    tenant: Option<Extension<TenantContext>>,
    Query(query): Query<ListQuery>,
) -> Response {
    let user = match require_permission(&state, &session, OWNER_PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let tenant = tenant.map_or_else(TenantContext::default_tenant, |Extension(t)| t);
    match owns_blog(&state, &user, tenant.id).await {
        Ok(true) => {}
        Ok(false) => return render_forbidden("You are not an owner of this blog."),
        Err(e) => {
            tracing::error!(error = %e, "failed to check blog membership");
            return render_server_error("The mail log could not be read.");
        }
    }
    let filter = query.filter(Some(tenant.id));
    render_list(&state, &session, &user, &query, filter, true).await
}

/// GET /admin/reports/mail/{id}
async fn message_detail(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<i64>,
) -> Response {
    let user = match require_permission(&state, &session, NETWORK_PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let message = match mail_log::detail(state.db(), id).await {
        Ok(Some(message)) => message,
        Ok(None) => return render_not_found(),
        Err(e) => {
            tracing::error!(error = %e, id, "failed to read a mail message");
            return render_server_error("The message could not be read.");
        }
    };
    let message_id = state.email().map(|email| email.message_id(id));
    let csrf_token = generate_csrf_token(&session).await;
    let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();

    let mut context = tera::Context::new();
    context.insert("message", &message);
    context.insert("message_id", &message_id);
    context.insert(
        "can_retry",
        &(matches!(message.row.status.as_str(), "failed" | "cancelled")
            && message.text_body.is_some()),
    );
    context.insert("can_cancel", &(message.row.status == "queued"));
    context.insert("csrf_token", &csrf_token);
    context.insert("flash", &flash);
    context.insert("path", "/admin/reports/mail");
    render_admin_template(&state, &viewer, "admin/reports/mail-detail.html", context).await
}

/// The two things an administrator can do to one message.
#[derive(Debug, Clone, Copy)]
enum Action {
    Retry,
    Cancel,
}

/// Run a Retry or Cancel and go back to the message with what happened.
async fn act(
    state: &AppState,
    session: &Session,
    id: i64,
    token: &str,
    action: Action,
) -> Response {
    let user = match require_permission(state, session, NETWORK_PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    if let Err(refusal) = require_csrf(session, token).await {
        return refusal;
    }
    let done = match action {
        Action::Retry => mail_log::retry(state.db(), id).await,
        Action::Cancel => mail_log::cancel(state.db(), id).await,
    };
    let message = match (action, done) {
        (Action::Retry, Ok(true)) => {
            if let Some(email) = state.email() {
                email.wake();
            }
            "The message is queued again."
        }
        (Action::Retry, Ok(false)) => {
            "This message cannot be retried: it is not failed or cancelled, or its body was cleared."
        }
        (Action::Cancel, Ok(true)) => "The message is cancelled.",
        (Action::Cancel, Ok(false)) => "This message cannot be cancelled: it is no longer queued.",
        (_, Err(e)) => {
            tracing::error!(error = %e, id, ?action, "a mail log action failed");
            return render_server_error("The message could not be changed.");
        }
    };
    tracing::info!(user_id = %user.id, id, ?action, "mail log action");
    let _ = session.insert(FLASH_KEY, message).await;
    Redirect::to(&format!("/admin/reports/mail/{id}")).into_response()
}

/// POST /admin/reports/mail/{id}/retry
async fn retry_message(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<i64>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    act(&state, &session, id, &form.token, Action::Retry).await
}

/// POST /admin/reports/mail/{id}/cancel
async fn cancel_message(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<i64>,
    Form(form): Form<CsrfOnlyForm>,
) -> Response {
    act(&state, &session, id, &form.token, Action::Cancel).await
}

#[derive(Debug, Default, Deserialize)]
struct SuppressionQuery {
    #[serde(default)]
    address: String,
    page: Option<i64>,
}

/// GET /admin/reports/mail/suppressions
async fn suppression_list(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<SuppressionQuery>,
) -> Response {
    let user = match require_permission(&state, &session, NETWORK_PERMISSION).await {
        Ok(user) => user,
        Err(refusal) => return refusal,
    };
    let viewer = super::helpers::user_context_for(&state, &user).await;
    let address = text_filter(&query.address);
    let page = query.page.unwrap_or(1).clamp(1, 100_000);
    let (entries, total) = match mail_log::suppressions(
        state.db(),
        address.as_deref(),
        PER_PAGE,
        (page - 1) * PER_PAGE,
    )
    .await
    {
        Ok(found) => found,
        Err(e) => {
            tracing::error!(error = %e, "failed to read the suppression list");
            return render_server_error("The suppression list could not be read.");
        }
    };
    let csrf_token = generate_csrf_token(&session).await;
    let flash: Option<String> = session.remove(FLASH_KEY).await.ok().flatten();

    let mut context = tera::Context::new();
    context.insert("entries", &entries);
    context.insert("total", &total);
    context.insert("page", &page);
    context.insert("total_pages", &((total + PER_PAGE - 1) / PER_PAGE).max(1));
    context.insert("address_filter", &address.clone().unwrap_or_default());
    context.insert(
        "filter_qs",
        &address
            .map(|a| format!("address={}", urlencoding::encode(&a)))
            .unwrap_or_default(),
    );
    context.insert("csrf_token", &csrf_token);
    context.insert("flash", &flash);
    context.insert("path", "/admin/reports/mail/suppressions");
    render_admin_template(
        &state,
        &viewer,
        "admin/reports/mail-suppressions.html",
        context,
    )
    .await
}

/// The add and remove forms.
#[derive(Debug, Deserialize)]
struct AddressForm {
    #[serde(rename = "_token")]
    token: String,
    address: String,
}

/// Check the permission and token of a suppression form, and its address.
async fn suppression_form(
    state: &AppState,
    session: &Session,
    form: &AddressForm,
) -> Result<(User, String), Response> {
    let user = require_permission(state, session, NETWORK_PERMISSION).await?;
    require_csrf(session, &form.token).await?;
    let address = form.address.trim().to_lowercase();
    if address.is_empty() || address.len() > MAX_FILTER_CHARS {
        return Err(render_error("Enter an email address."));
    }
    Ok((user, address))
}

/// POST /admin/reports/mail/suppressions: stop all mail to an address.
async fn suppress_address(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<AddressForm>,
) -> Response {
    let (user, address) = match suppression_form(&state, &session, &form).await {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    if address.parse::<lettre::Address>().is_err() {
        return render_error("That is not an email address.");
    }
    let message = match mail_log::suppress(state.db(), &address).await {
        Ok(true) => format!("No blog will mail {address} until it is removed here."),
        Ok(false) => format!("{address} was already on the list."),
        Err(e) => {
            tracing::error!(error = %e, "failed to add a suppression");
            return render_server_error("The address could not be added.");
        }
    };
    tracing::info!(user_id = %user.id, "mail suppression added by hand");
    let _ = session.insert(FLASH_KEY, message).await;
    Redirect::to("/admin/reports/mail/suppressions").into_response()
}

/// POST /admin/reports/mail/suppressions/remove
async fn remove_suppression(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<AddressForm>,
) -> Response {
    let (user, address) = match suppression_form(&state, &session, &form).await {
        Ok(found) => found,
        Err(refusal) => return refusal,
    };
    let message = match mail_log::unsuppress(state.db(), &address).await {
        Ok(true) => format!("{address} can be mailed again."),
        Ok(false) => format!("{address} was not on the list."),
        Err(e) => {
            tracing::error!(error = %e, "failed to remove a suppression");
            return render_server_error("The address could not be removed.");
        }
    };
    tracing::info!(user_id = %user.id, "mail suppression removed");
    let _ = session.insert(FLASH_KEY, message).await;
    Redirect::to("/admin/reports/mail/suppressions").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_owner_view_cannot_be_pointed_at_another_blog() {
        let own = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        let query = ListQuery {
            blog: other.to_string(),
            ..ListQuery::default()
        };
        assert_eq!(query.filter(Some(own)).tenant, Some(own));
        assert_eq!(query.filter(None).tenant, Some(other));
    }

    #[test]
    fn unknown_statuses_and_kinds_filter_nothing() {
        let query = ListQuery {
            status: "deleted".to_string(),
            kind: "spam".to_string(),
            category: "  ".to_string(),
            ..ListQuery::default()
        };
        let filter = query.filter(None);
        assert_eq!(filter.status, None);
        assert_eq!(filter.kind, None);
        assert_eq!(filter.category, None);
    }

    #[test]
    fn the_pager_keeps_every_filter_but_status() {
        let filter = MailFilter {
            tenant: Some(Uuid::from_u128(3)),
            status: Some("failed".to_string()),
            kind: Some("bulk".to_string()),
            category: None,
            recipient: Some("a+b@x.test".to_string()),
        };
        let qs = filter_qs(&filter, false);
        assert!(qs.contains("blog=00000000-0000-0000-0000-000000000003"));
        assert!(qs.contains("kind=bulk"));
        assert!(qs.contains("recipient=a%2Bb%40x.test"));
        assert!(!qs.contains("status"));
        assert!(!filter_qs(&filter, true).contains("blog="));
    }
}
