//! Front page route handler.

use axum::{Router, extract::State, response::Html, routing::get};
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::FilterPipeline;
use crate::models::{Item, SiteConfig};
use crate::state::AppState;
use crate::tap::UserContext;

use super::helpers::{html_escape, inject_site_context};

/// Session key for user ID.
const SESSION_USER_ID: &str = "user_id";

/// Create the front page router.
pub fn router() -> Router<AppState> {
    Router::new().route("/", get(front_page))
}

/// Front page handler.
///
/// If `site_front_page` is configured, loads and renders that item.
/// Otherwise, shows promoted content or a welcome message.
async fn front_page(State(state): State<AppState>, session: Session) -> Html<String> {
    // Check for configured front page item
    if let Ok(Some(front_path)) = SiteConfig::front_page(state.db()).await {
        if let Some(html) = render_configured_front_page(&state, &session, &front_path).await {
            return Html(html);
        }
    }

    // Fall back to promoted items listing
    let content = render_promoted_listing(&state).await;

    let mut context = tera::Context::new();
    inject_site_context(&state, &session, &mut context, "/").await;

    let html = state
        .theme()
        .render_page("/front", "Home", &content, &mut context)
        .unwrap_or_else(|_| format!("<html><body>{}</body></html>", content));

    Html(html)
}

/// Render a configured front page item.
async fn render_configured_front_page(
    state: &AppState,
    session: &Session,
    front_path: &str,
) -> Option<String> {
    // Extract item ID from path like "/item/{uuid}"
    let item_id = front_path
        .strip_prefix("/item/")
        .and_then(|id_str| Uuid::parse_str(id_str).ok())?;

    // Use load_for_view to invoke tap hooks and check access
    let user = get_user_context(session).await;
    let (item, render_outputs) = state.items().load_for_view(item_id, &user).await.ok()??;

    if !item.is_published() {
        return None;
    }

    // Render item fields and plugin outputs
    let mut children_html = render_item_fields(&item);
    for output in render_outputs {
        children_html.push_str(&output);
    }

    // Resolve item template
    let suggestions = vec![
        format!("elements/item--{}--{}", item.item_type, item.id),
        format!("elements/item--{}", item.item_type),
        "elements/item".to_string(),
    ];
    let suggestion_refs: Vec<&str> = suggestions.iter().map(|s| s.as_str()).collect();
    let template = state
        .theme()
        .resolve_template(&suggestion_refs)
        .unwrap_or_else(|| "elements/item.html".to_string());

    let mut context = tera::Context::new();
    context.insert("item", &item);
    context.insert("children", &children_html);

    let item_html = state.theme().tera().render(&template, &context).ok()?;

    // Wrap in page layout
    inject_site_context(state, session, &mut context, "/").await;

    state
        .theme()
        .render_page("/front", &item.title, &item_html, &mut context)
        .ok()
}

/// Render promoted items listing HTML.
async fn render_promoted_listing(state: &AppState) -> String {
    let items = Item::list_published(state.db(), 10, 0)
        .await
        .unwrap_or_default();

    let promoted: Vec<&Item> = items.iter().filter(|i| i.is_promoted()).collect();

    if promoted.is_empty() {
        return String::new();
    }

    let mut html = String::from("<div class=\"front-listing\">");

    for item in promoted {
        html.push_str("<div class=\"blog-teaser\">");
        html.push_str(&format!(
            "<h2 class=\"blog-teaser__title\"><a href=\"/item/{}\">{}</a></h2>",
            item.id,
            html_escape(&item.title)
        ));

        let date = chrono::DateTime::from_timestamp(item.created, 0)
            .map(|dt| dt.format("%B %-d, %Y").to_string())
            .unwrap_or_default();
        if !date.is_empty() {
            html.push_str(&format!(
                "<div class=\"blog-teaser__meta\"><time>{}</time></div>",
                date
            ));
        }

        // Render body field summary if available
        if let Some(body) = item
            .fields
            .get("body")
            .and_then(|f| f.get("value"))
            .and_then(|v| v.as_str())
        {
            let format = item
                .fields
                .get("body")
                .and_then(|f| f.get("format"))
                .and_then(|v| v.as_str())
                .unwrap_or("plain_text");
            let filtered = FilterPipeline::for_format(format).process(body);
            // Truncate for teaser (char-boundary safe)
            let summary = if filtered.chars().count() > 200 {
                let truncated: String = filtered.chars().take(200).collect();
                format!("{}...", truncated)
            } else {
                filtered
            };
            html.push_str(&format!(
                "<div class=\"blog-teaser__summary\">{}</div>",
                summary
            ));
        }

        html.push_str(&format!(
            "<a href=\"/item/{}\" class=\"blog-teaser__read-more\">Read more &rarr;</a>",
            item.id
        ));
        html.push_str("</div>");
    }

    html.push_str("</div>");
    html
}

/// Render item fields to HTML.
fn render_item_fields(item: &Item) -> String {
    let mut html = String::new();

    if let Some(fields) = item.fields.as_object() {
        for (name, value) in fields {
            if let Some(text_val) = value.get("value").and_then(|v| v.as_str()) {
                let format = value
                    .get("format")
                    .and_then(|v| v.as_str())
                    .unwrap_or("plain_text");
                let filtered = FilterPipeline::for_format(format).process(text_val);
                html.push_str(&format!(
                    "<div class=\"field field-{}\">{}</div>",
                    name, filtered
                ));
            }
        }
    }

    html
}

/// Get user context from session for access control.
async fn get_user_context(session: &Session) -> UserContext {
    let user_id: Option<Uuid> = session.get(SESSION_USER_ID).await.ok().flatten();
    match user_id {
        Some(id) => UserContext {
            id,
            authenticated: true,
            permissions: vec!["access content".to_string()],
        },
        None => UserContext::anonymous(),
    }
}
