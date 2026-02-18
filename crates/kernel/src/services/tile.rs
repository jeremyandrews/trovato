//! Tile rendering service.
//!
//! Loads visible tiles for a given region/stage/path and renders them to HTML.

use anyhow::Result;
use sqlx::PgPool;

use crate::models::tile::Tile;

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
        stage_id: &str,
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
        stage_id: &str,
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

            // Tiles are admin-only, so full_html is permitted.
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
        _ => {
            html.push_str("<p>Unknown tile type</p>");
        }
    }
    html.push_str("</div>\n</div>\n");

    html
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

#[cfg(test)]
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
            stage_id: "live".into(),
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
}
