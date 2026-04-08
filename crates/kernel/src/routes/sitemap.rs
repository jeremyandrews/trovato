//! Sitemap.xml and robots.txt routes.
//!
//! Generates an XML sitemap of all published items (with URL alias
//! resolution) and a robots.txt pointing to the sitemap.

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

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

/// Generate robots.txt with a sitemap reference.
async fn robots_txt() -> Response {
    let body = "User-agent: *\nAllow: /\n\nSitemap: /sitemap.xml\n";

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
