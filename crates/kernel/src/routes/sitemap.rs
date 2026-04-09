//! Sitemap.xml and robots.txt routes.
//!
//! Generates an XML sitemap of all published items (with URL alias
//! resolution) and a robots.txt pointing to the sitemap.

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::models::SiteConfig;
use crate::models::stage::LIVE_STAGE_ID;
use crate::state::AppState;

/// Row type for sitemap item queries.
#[derive(sqlx::FromRow)]
struct SitemapRow {
    id: uuid::Uuid,
    changed: i64,
}

/// Row type for URL alias lookup.
#[derive(sqlx::FromRow)]
struct AliasRow {
    source: String,
    alias: String,
}

/// Generate sitemap.xml listing all published live-stage items.
async fn sitemap_xml(State(state): State<AppState>) -> Response {
    let items = match sqlx::query_as::<_, SitemapRow>(
        "SELECT id, changed FROM item WHERE status = 1 AND stage_id = $1 ORDER BY changed DESC",
    )
    .bind(LIVE_STAGE_ID)
    .fetch_all(state.db())
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(error = %e, "failed to query items for sitemap");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Sitemap generation failed",
            )
                .into_response();
        }
    };

    // Bulk-load URL aliases for item sources so we can emit friendly URLs.
    let aliases: std::collections::HashMap<String, String> = match sqlx::query_as::<_, AliasRow>(
        "SELECT source, alias FROM url_alias WHERE source LIKE '/item/%' AND stage_id = $1",
    )
    .bind(LIVE_STAGE_ID)
    .fetch_all(state.db())
    .await
    {
        Ok(rows) => rows.into_iter().map(|r| (r.source, r.alias)).collect(),
        Err(_) => std::collections::HashMap::new(),
    };

    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str("<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n");

    for row in &items {
        let source = format!("/item/{}", row.id);
        let url = aliases.get(&source).cloned().unwrap_or(source);
        let lastmod = chrono::DateTime::from_timestamp(row.changed, 0)
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_default();

        xml.push_str(&format!(
            "  <url>\n    <loc>{url}</loc>\n    <lastmod>{lastmod}</lastmod>\n  </url>\n"
        ));
    }

    xml.push_str("</urlset>");

    (
        axum::http::StatusCode::OK,
        [("content-type", "application/xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

/// Known AI search engine crawlers and their `site_config` toggle keys.
const AI_CRAWLERS: [(&str, &str); 8] = [
    ("GPTBot", "gptbot_blocked"),
    ("ChatGPT-User", "chatgpt_user_blocked"),
    ("ClaudeBot", "claudebot_blocked"),
    ("Google-Extended", "google_extended_blocked"),
    ("Bytespider", "bytespider_blocked"),
    ("CCBot", "ccbot_blocked"),
    ("PerplexityBot", "perplexitybot_blocked"),
    ("Amazonbot", "amazonbot_blocked"),
];

/// Generate robots.txt with AI crawler management and a sitemap reference.
///
/// Reads per-crawler block flags and custom content from `SiteConfig`.
async fn robots_txt(State(state): State<AppState>) -> Response {
    let mut sections = Vec::new();

    // AI crawler rules (managed by SEO settings)
    let mut ai_rules = String::new();
    for (agent, config_key) in &AI_CRAWLERS {
        let blocked = SiteConfig::get(state.db(), config_key)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if blocked {
            use std::fmt::Write;
            // Infallible: write! to String cannot fail
            let _ = write!(ai_rules, "User-agent: {agent}\nDisallow: /\n\n");
        }
    }

    if !ai_rules.is_empty() {
        sections.push("# AI Search Engine Crawlers (managed by Trovato SEO)".to_string());
        sections.push(ai_rules);
        sections.push("# End AI Search Engine Crawlers".to_string());
        sections.push(String::new());
    }

    // Default rules
    sections.push("User-agent: *\nAllow: /".to_string());
    sections.push(String::new());

    // Custom robots.txt content from admin
    if let Ok(Some(custom)) = SiteConfig::get(state.db(), "robots_txt_custom").await
        && let Some(text) = custom.as_str()
        && !text.is_empty()
    {
        sections.push(text.to_string());
        sections.push(String::new());
    }

    // Sitemap reference
    sections.push("Sitemap: /sitemap.xml".to_string());

    let body = sections.join("\n");
    (
        axum::http::StatusCode::OK,
        [("content-type", "text/plain; charset=utf-8")],
        body,
    )
        .into_response()
}

/// Build the sitemap/robots router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sitemap.xml", get(sitemap_xml))
        .route("/robots.txt", get(robots_txt))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn ai_crawlers_list_is_complete() {
        // Ensure we track all major AI crawlers
        assert_eq!(AI_CRAWLERS.len(), 8, "Should track 8 AI crawlers");

        let names: Vec<&str> = AI_CRAWLERS.iter().map(|(name, _)| *name).collect();
        assert!(names.contains(&"GPTBot"));
        assert!(names.contains(&"ChatGPT-User"));
        assert!(names.contains(&"ClaudeBot"));
        assert!(names.contains(&"Google-Extended"));
        assert!(names.contains(&"Bytespider"));
        assert!(names.contains(&"CCBot"));
        assert!(names.contains(&"PerplexityBot"));
        assert!(names.contains(&"Amazonbot"));
    }
}
