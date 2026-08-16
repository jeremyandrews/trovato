//! Tile rendering service.
//!
//! Loads visible tiles for a given region/stage/path and renders them to HTML.

use anyhow::Result;
use sqlx::PgPool;

use crate::models::tile::Tile;
use crate::routes::helpers::html_escape;

/// Service for loading and rendering tiles.
pub struct TileService {
    db: PgPool,
}

impl TileService {
    /// Create a new tile service.
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// Get visible tiles for a region, filtered by path and role visibility rules.
    pub async fn get_visible_tiles(
        &self,
        region: &str,
        stage_id: uuid::Uuid,
        path: &str,
        user_roles: &[String],
    ) -> Result<Vec<Tile>> {
        let tiles = Tile::list_by_region(&self.db, region, stage_id).await?;

        Ok(tiles
            .into_iter()
            .filter(|t| t.is_visible(path, user_roles))
            .collect())
    }

    /// Render a single tile to an HTML string.
    pub fn render_tile(&self, tile: &Tile) -> String {
        render_tile_html(tile)
    }

    /// Render all visible tiles for a region as a single HTML string.
    pub async fn render_region(
        &self,
        region: &str,
        stage_id: uuid::Uuid,
        path: &str,
        user_roles: &[String],
    ) -> Result<String> {
        let tiles = self
            .get_visible_tiles(region, stage_id, path, user_roles)
            .await?;
        let html: String = tiles.iter().map(render_tile_html).collect();
        Ok(html)
    }
}

/// Render a single tile to an HTML string (standalone, no database needed).
fn render_tile_html(tile: &Tile) -> String {
    let mut html = String::new();

    html.push_str(&format!(
        "<div class=\"tile tile--{} tile--{}\" id=\"tile-{}\">\n",
        html_escape(&tile.tile_type),
        html_escape(&tile.machine_name),
        html_escape(&tile.machine_name),
    ));

    // Label
    html.push_str(&format!(
        "<h3 class=\"tile__title\">{}</h3>\n",
        html_escape(&tile.label)
    ));

    // Body depends on tile_type
    html.push_str("<div class=\"tile__content\">\n");
    match tile.tile_type.as_str() {
        "custom_html" => {
            let body = tile
                .config
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let format = tile
                .config
                .get("format")
                .and_then(|v| v.as_str())
                .unwrap_or("filtered_html");

            // Tile configs are admin-set (saved via admin forms with "configure site"
            // permission). for_format_checked with has_full_html=true is appropriate —
            // admins may use full_html. Unknown format strings fall through to
            // for_format() which defaults to plain_text (safe).
            let pipeline = crate::content::FilterPipeline::for_format_checked(format, true);
            html.push_str(&pipeline.process(body));
        }
        "menu" => {
            let menu_name = tile
                .config
                .get("menu_name")
                .and_then(|v| v.as_str())
                .unwrap_or("main");
            html.push_str(&format!(
                "<nav class=\"tile-menu\" data-menu=\"{}\"></nav>",
                html_escape(menu_name)
            ));
        }
        "gather_query" => {
            let query_id = tile
                .config
                .get("query_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            html.push_str(&format!(
                "<div class=\"tile-gather\" data-query-id=\"{}\"></div>",
                html_escape(query_id)
            ));
        }
        "chat" => {
            render_chat_widget(&mut html, &tile.machine_name);
        }
        _ => {
            html.push_str("<p>Unknown tile type</p>");
        }
    }
    html.push_str("</div>\n</div>\n");

    html
}

/// Render the chat widget HTML.
///
/// Emits a `<div>` with data attributes and a `<script src>` for the
/// external chat-widget.js file. No inline JS — compatible with strict
/// Content-Security-Policy.
fn render_chat_widget(html: &mut String, machine_name: &str) {
    if !crate::routes::helpers::is_valid_machine_name(machine_name) {
        html.push_str("<p>Invalid chat widget configuration</p>");
        return;
    }
    let escaped = html_escape(machine_name);
    html.push_str(&format!(
        r#"<div class="chat-widget" id="chat-widget-{escaped}" data-machine-name="{escaped}">
  <div class="chat-messages" id="chat-messages-{escaped}"></div>
  <form class="chat-input" id="chat-form-{escaped}">
    <input type="text" id="chat-input-{escaped}" placeholder="Ask a question..." autocomplete="off" maxlength="4096">
    <button type="submit">Send</button>
  </form>
</div>
<link rel="stylesheet" href="/static/css/chat-widget.css">
<script src="/static/js/chat-widget.js"></script>"#
    ));
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn make_tile(tile_type: &str, config: serde_json::Value) -> Tile {
        Tile {
            id: uuid::Uuid::nil(),
            machine_name: "test_tile".into(),
            label: "Test Tile".into(),
            region: "sidebar".into(),
            tile_type: tile_type.into(),
            config,
            visibility: serde_json::json!({}),
            weight: 0,
            status: 1,
            plugin: "core".into(),
            stage_id: crate::models::stage::LIVE_STAGE_ID,
            created: 0,
            changed: 0,
        }
    }

    #[test]
    fn render_custom_html_tile() {
        let tile = make_tile(
            "custom_html",
            serde_json::json!({
                "body": "<p>Hello <strong>World</strong></p>",
                "format": "filtered_html"
            }),
        );
        let html = render_tile_html(&tile);
        assert!(html.contains("tile--custom_html"));
        assert!(html.contains("Test Tile"));
        assert!(html.contains("<p>Hello <strong>World</strong></p>"));
    }

    #[test]
    fn render_menu_tile() {
        let tile = make_tile("menu", serde_json::json!({ "menu_name": "footer" }));
        let html = render_tile_html(&tile);
        assert!(html.contains("tile-menu"));
        assert!(html.contains("data-menu=\"footer\""));
    }

    #[test]
    fn render_gather_query_tile() {
        let tile = make_tile(
            "gather_query",
            serde_json::json!({ "query_id": "blog_listing" }),
        );
        let html = render_tile_html(&tile);
        assert!(html.contains("tile-gather"));
        assert!(html.contains("data-query-id=\"blog_listing\""));
    }

    #[test]
    fn render_chat_tile() {
        let tile = make_tile("chat", serde_json::json!({}));
        let html = render_tile_html(&tile);
        assert!(html.contains("tile--chat"));
        assert!(html.contains("chat-widget"));
        assert!(html.contains("chat-messages"));
        // JS is now in static/js/chat-widget.js, loaded via script src
        assert!(html.contains("chat-widget.js"));
    }
}
