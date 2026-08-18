//! "Download my data": one JSON document holding everything the site keeps that
//! is the account holder's own.
//!
//! # Why this exists at all
//!
//! For a site operated from the EU with open registration, GDPR article 15 gives a
//! person the right to a copy of their data, and until now Trovato had no way to
//! produce one. That is the legal reading; the practical one is that a person who
//! cannot get their own writing back out of a site does not really have it.
//!
//! # What is in it, and what is deliberately not
//!
//! In: the profile fields, the roles held, every authored item with its fields and
//! timestamps, every comment, and the metadata of active sessions.
//!
//! Not in: **anything the person merely looked at.** Trovato keeps no reading
//! history to export, and the page says so rather than leaving the absence to be
//! guessed at. Also not in: session tokens or credential material. A session's
//! metadata answers "which of my devices are logged in"; the token would let
//! whoever holds the file *be* those devices, which is the opposite of a privacy
//! feature.
//!
//! # One request per hour
//!
//! The document is built in memory and is as large as the account's content, so it
//! is the one authenticated read on the site whose cost scales with what the caller
//! wrote. Once an hour per account is generous for the purpose and cheap to serve.
//! An account with hundreds of thousands of items would want a queued export
//! written to a file; that is not this, and the limit is named rather than
//! discovered.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use tower_sessions::Session;

use crate::models::{Comment, Item, Role, SiteConfig};
use crate::state::AppState;

use super::helpers::require_login;

/// The rate-limit category for the export.
const RATE_CATEGORY: &str = "data_export";

/// What the export document looks like.
#[derive(Debug, Serialize)]
struct DataExport {
    /// The format's own version, so a consumer can tell what it is reading.
    export_format: &'static str,
    /// The site this came from, and when.
    site: SiteInfo,
    /// The account.
    account: AccountInfo,
    /// Roles held, by name.
    roles: Vec<String>,
    /// Every item this account authored.
    items: Vec<ItemExport>,
    /// Every comment this account wrote.
    comments: Vec<CommentExport>,
    /// Active sessions, metadata only.
    sessions: Vec<SessionExport>,
    /// What is not here, stated in the file as well as on the page.
    not_included: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
struct SiteInfo {
    name: String,
    exported_at: i64,
}

#[derive(Debug, Serialize)]
struct AccountInfo {
    id: String,
    username: String,
    email: String,
    created: i64,
    last_login: Option<i64>,
    status: i16,
    is_admin: bool,
}

#[derive(Debug, Serialize)]
struct ItemExport {
    id: String,
    r#type: String,
    title: String,
    status: i16,
    created: i64,
    changed: i64,
    language: String,
    fields: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct CommentExport {
    id: String,
    item_id: String,
    parent_id: Option<String>,
    body: String,
    status: String,
    created: i64,
    changed: i64,
}

#[derive(Debug, Serialize)]
struct SessionExport {
    device_name: String,
    user_agent: String,
    created_at: i64,
    last_seen: i64,
}

/// Serve the export.
///
/// GET /user/data-export
pub async fn export_my_data(State(state): State<AppState>, session: Session) -> Response {
    let user = match require_login(&state, &session).await {
        Ok(user) => user,
        Err(redirect) => return redirect,
    };

    // Keyed on the account rather than the IP: this is a per-account cost, and two
    // people behind one address should not be able to starve each other of their
    // own data.
    if let Err(retry_after) = state
        .rate_limiter()
        .check(RATE_CATEGORY, &user.id.to_string())
        .await
    {
        let minutes = retry_after / 60;
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, retry_after.to_string())],
            axum::response::Html(format!(
                "<h1>Not yet</h1><p>An export can be downloaded once an hour. \
                 Try again in about {minutes} minute(s).</p>\
                 <p><a href=\"/user/profile\">Back to my account</a></p>"
            )),
        )
            .into_response();
    }

    let pool = state.db();

    let roles = Role::get_user_roles(pool, user.id)
        .await
        .map(|roles| roles.into_iter().map(|r| r.name).collect())
        .unwrap_or_default();

    let items = match Item::list_by_author(pool, user.id).await {
        Ok(items) => items
            .into_iter()
            .map(|item| ItemExport {
                id: item.id.to_string(),
                r#type: item.item_type,
                title: item.title,
                status: item.status,
                created: item.created,
                changed: item.changed,
                language: item.language,
                fields: item.fields,
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "failed to list a user's items for export");
            return super::helpers::render_server_error("Failed to build the export.");
        }
    };

    let comments = match Comment::list_by_author(pool, user.id).await {
        Ok(comments) => comments
            .into_iter()
            .map(|comment| CommentExport {
                id: comment.id.to_string(),
                item_id: comment.item_id.to_string(),
                parent_id: comment.parent_id.map(|p| p.to_string()),
                body: comment.body,
                // The label, not the raw number, and an unknown value says so
                // rather than being silently reported as one of the known ones.
                status: crate::models::CommentStatus::from_i16(comment.status).map_or_else(
                    || format!("unknown ({})", comment.status),
                    |status| status.label().to_string(),
                ),
                created: comment.created,
                changed: comment.changed,
            })
            .collect(),
        Err(e) => {
            tracing::error!(error = %e, "failed to list a user's comments for export");
            return super::helpers::render_server_error("Failed to build the export.");
        }
    };

    // Metadata only. A session's token would let whoever reads the file be that
    // session, which is the opposite of what this feature is for.
    let sessions = state
        .session_registry()
        .list(user.id)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|entry| SessionExport {
            device_name: entry.device_name,
            user_agent: entry.user_agent,
            created_at: entry.created_at,
            last_seen: entry.last_seen,
        })
        .collect();

    let export = DataExport {
        export_format: "trovato-data-export-1",
        site: SiteInfo {
            name: SiteConfig::site_name(pool)
                .await
                .unwrap_or_else(|_| "Trovato".to_string()),
            exported_at: chrono::Utc::now().timestamp(),
        },
        account: AccountInfo {
            id: user.id.to_string(),
            username: user.name.clone(),
            email: user.mail.clone(),
            created: user.created.timestamp(),
            last_login: user.login.map(|t| t.timestamp()),
            status: user.status,
            is_admin: user.is_admin,
        },
        roles,
        items,
        comments,
        sessions,
        not_included: vec![
            "Content you only viewed: this site keeps no reading history.",
            "Session tokens and credential material.",
        ],
    };

    let body = match serde_json::to_vec_pretty(&export) {
        Ok(body) => body,
        Err(e) => {
            tracing::error!(error = %e, "failed to serialize a data export");
            return super::helpers::render_server_error("Failed to build the export.");
        }
    };

    // A filename an operating system will keep, derived from the username rather
    // than trusting it: a username is user input and this ends up in a header.
    let safe_name: String = user
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(64)
        .collect();
    let filename = format!("trovato-data-{safe_name}.json");

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")).unwrap_or_else(
            |_| HeaderValue::from_static("attachment; filename=\"trovato-data.json\""),
        ),
    );
    // Never cached: it is a personal document served over an authenticated session.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, private"),
    );

    tracing::info!(user_id = %user.id, items = export.items.len(), "data export served");

    (StatusCode::OK, headers, body).into_response()
}
