//! A confirmation page for destructive admin actions, with or without
//! JavaScript.
//!
//! A form whose action ends in `?confirm` asks before it acts. When it is
//! posted, [`ask_first`] does not pass it to the handler. It keeps the
//! submitted fields in the session and sends the browser to
//! `GET /admin/confirm`. That page names what will happen and offers the real
//! button. The real button posts the same fields to the same address without
//! the marker, so the handler runs exactly as it always has, with its own
//! permission and token checks.
//!
//! The two forms of the marker:
//! - `?confirm` always asks. The form says what it asks with the hidden
//!   fields `_confirm` (the question) and `_confirm_label` (the button), both
//!   already translated by the template that drew the form.
//! - `?confirm=<value>` asks only when the form's `action` field is `<value>`.
//!   A bulk form uses it. The question names how many `ids[]` are selected.
//!
//! The marker is a convenience for people, not a security boundary: a request
//! without it reaches the handler directly, as before. A post whose form token
//! does not verify is passed straight to the handler, which refuses it as it
//! always did, so the page only ever repeats words this site's own form wrote.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::form::csrf::generate_csrf_token;
use crate::i18n::UiMessage;
use crate::state::AppState;

use super::helpers::{get_user_context, render_admin_template, require_login};

/// The query marker a form adds to its action to ask first.
pub const CONFIRM_MARKER: &str = "confirm";

/// The session key holding the action waiting for confirmation.
const PENDING_KEY: &str = "admin_confirm_pending";

/// The address of the confirmation page.
pub const CONFIRM_PATH: &str = "/admin/confirm";

/// The largest form this layer reads: the same limit axum's `Form` applies.
const BODY_LIMIT: usize = 2 * 1024 * 1024;

/// Words on the page: text a template already translated, shown as it is,
/// or a message this module writes, translated when the page is drawn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
enum Words {
    Text(String),
    Message(UiMessage),
}

/// An action waiting for the person to confirm it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    /// Where the confirmed form posts: the original path, without the marker.
    action: String,
    /// The fields as they were submitted, minus the token and the question.
    fields: Vec<(String, String)>,
    /// What the page asks.
    question: Words,
    /// The button that does it.
    label: Words,
    /// Where Cancel goes: the page the form was on.
    cancel: String,
}

/// The layer: ask first when the form's action carries the marker.
pub async fn ask_first(session: Session, request: Request, next: Next) -> Response {
    if request.method() != Method::POST {
        return next.run(request).await;
    }
    let Some(marker) = marker(request.uri().query()) else {
        return next.run(request).await;
    };
    let is_form = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/x-www-form-urlencoded"));
    if !is_form {
        return next.run(request).await;
    }

    let cancel = request
        .headers()
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(same_site_path)
        .unwrap_or_else(|| "/admin".to_string());
    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                "The form is too large.",
            )
                .into_response();
        }
    };
    let fields: Vec<(String, String)> = url::form_urlencoded::parse(&bytes)
        .into_owned()
        .collect();
    let rebuilt = |parts, bytes| next.run(Request::from_parts(parts, Body::from(bytes)));

    let Some(pending) = pending_for(parts.uri.path(), &marker, &fields, cancel) else {
        return rebuilt(parts, bytes).await;
    };
    // Only this site's own forms reach the page: a token that does not verify
    // goes on to the handler, which refuses it.
    let token = field(&fields, "_token").unwrap_or_default();
    let valid = crate::form::csrf::verify_csrf_token(&session, token)
        .await
        .unwrap_or(false);
    if !valid {
        return rebuilt(parts, bytes).await;
    }
    if let Err(e) = session.insert(PENDING_KEY, &pending).await {
        tracing::warn!(error = %e, "could not keep the action to confirm; asking is skipped");
        return rebuilt(parts, bytes).await;
    }
    Redirect::to(CONFIRM_PATH).into_response()
}

/// The marker's value, when the query has it: `""` for a bare `?confirm`.
fn marker(query: Option<&str>) -> Option<String> {
    url::form_urlencoded::parse(query?.as_bytes())
        .find(|(k, _)| k == CONFIRM_MARKER)
        .map(|(_, v)| v.into_owned())
}

fn field<'a>(fields: &'a [(String, String)], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// The path of a same-site address, from a full URL or a path.
fn same_site_path(referer: &str) -> Option<String> {
    let path = if referer.starts_with('/') {
        referer.to_string()
    } else {
        let url = url::Url::parse(referer).ok()?;
        let mut path = url.path().to_string();
        if let Some(q) = url.query() {
            path.push('?');
            path.push_str(q);
        }
        path
    };
    // A path only: never `//host`, which a browser reads as another site.
    (path.starts_with('/') && !path.starts_with("//")).then_some(path)
}

/// What to ask, or `None` when this submission does not need asking.
fn pending_for(
    path: &str,
    marker: &str,
    fields: &[(String, String)],
    cancel: String,
) -> Option<Pending> {
    let kept = |skip: &[&str]| -> Vec<(String, String)> {
        fields
            .iter()
            .filter(|(k, _)| !skip.contains(&k.as_str()))
            .cloned()
            .collect()
    };
    if marker.is_empty() {
        let question = field(fields, "_confirm").map_or_else(
            || Words::Message(UiMessage::new("Are you sure? This cannot be undone.")),
            |s| Words::Text(s.to_string()),
        );
        let label = field(fields, "_confirm_label").map_or_else(
            || Words::Message(UiMessage::new("Confirm")),
            |s| Words::Text(s.to_string()),
        );
        // The template already translated the question and the label; they
        // are words of this site's own form, shown as they came.
        return Some(Pending {
            action: path.to_string(),
            fields: kept(&["_token", "_confirm", "_confirm_label"]),
            question,
            label,
            cancel,
        });
    }
    // Conditional: a bulk form asks only for its destructive action.
    if field(fields, "action") != Some(marker) {
        return None;
    }
    let count = fields.iter().filter(|(k, _)| k == "ids[]").count() as i64;
    if count == 0 {
        // Nothing selected: the handler says so.
        return None;
    }
    Some(Pending {
        action: path.to_string(),
        fields: kept(&["_token", "_confirm", "_confirm_label"]),
        question: Words::Message(
            UiMessage::new("Delete {count} selected item? This cannot be undone.")
                .plural("Delete {count} selected items? This cannot be undone.", count),
        ),
        label: Words::Message(
            UiMessage::new("Delete {count} item").plural("Delete {count} items", count),
        ),
        cancel,
    })
}

/// `GET /admin/confirm`: the waiting action, with the button that does it.
async fn confirm_page(State(state): State<AppState>, session: Session) -> Response {
    if let Err(redirect) = require_login(&state, &session).await {
        return redirect;
    }
    let pending: Option<Pending> = session.get(PENDING_KEY).await.ok().flatten();
    let Some(pending) = pending else {
        // Nothing is waiting (already done, or the session ended).
        return Redirect::to("/admin").into_response();
    };
    let user = get_user_context(&session, &state).await;
    let csrf_token = generate_csrf_token(&session).await;
    let mut context = tera::Context::new();
    context.insert("path", CONFIRM_PATH);
    context.insert("pending", &pending);
    context.insert("csrf_token", &csrf_token);
    render_admin_template(&state, &user, "admin/confirm.html", context).await
}

/// The confirmation page's route.
pub fn router() -> Router<AppState> {
    Router::new().route(CONFIRM_PATH, get(confirm_page))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn the_marker_is_read_from_the_query() {
        assert_eq!(marker(Some("confirm")), Some(String::new()));
        assert_eq!(marker(Some("a=1&confirm=delete")), Some("delete".into()));
        assert_eq!(marker(Some("a=1")), None);
        assert_eq!(marker(None), None);
    }

    #[test]
    fn a_bulk_form_asks_only_for_its_destructive_action() {
        let publish = fields(&[("action", "publish"), ("ids[]", "a")]);
        assert!(pending_for("/admin/content/bulk", "delete", &publish, "/".into()).is_none());
        let none = fields(&[("action", "delete")]);
        assert!(pending_for("/admin/content/bulk", "delete", &none, "/".into()).is_none());
        let two = fields(&[("_token", "t"), ("action", "delete"), ("ids[]", "a"), ("ids[]", "b")]);
        let p = pending_for("/admin/content/bulk", "delete", &two, "/".into()).unwrap();
        assert!(matches!(&p.question, Words::Message(m) if m.count == Some(2)));
        assert!(p.fields.iter().all(|(k, _)| k != "_token"));
        assert_eq!(p.fields.iter().filter(|(k, _)| k == "ids[]").count(), 2);
    }

    #[test]
    fn a_plain_form_asks_its_own_question() {
        let f = fields(&[("_token", "t"), ("_confirm", "Delete the tag Rust?"), ("_confirm_label", "Delete Rust")]);
        let p = pending_for("/admin/structure/tags/1/delete", "", &f, "/admin".into()).unwrap();
        assert_eq!(p.action, "/admin/structure/tags/1/delete");
        assert!(p.fields.is_empty());
        assert!(matches!(&p.question, Words::Text(s) if s == "Delete the tag Rust?"));
        assert!(matches!(&p.label, Words::Text(s) if s == "Delete Rust"));
    }

    #[test]
    fn cancel_never_leaves_the_site() {
        assert_eq!(same_site_path("https://example.com/admin/x?page=2"), Some("/admin/x?page=2".into()));
        assert_eq!(same_site_path("/admin/x"), Some("/admin/x".into()));
        assert_eq!(same_site_path("//evil.example/x"), None);
    }
}
