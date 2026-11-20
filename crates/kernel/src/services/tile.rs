//! Tile rendering service.
//!
//! Loads visible tiles for a given region/stage/path and renders them to HTML.

use std::collections::BTreeMap;

use anyhow::Result;
use sqlx::PgPool;

use crate::menu::MenuTreeLink;
use crate::models::tile::Tile;
use crate::routes::helpers::html_escape;
use crate::state::AppState;
use crate::tap::UserContext;

/// What rendering a tile needs from the request it is rendered for.
///
/// One struct rather than an argument per need, because tiles that show real
/// content need to know who is looking and in what language, and the menus
/// already built for this render.
pub struct TileRenderContext<'a> {
    /// The application, for the theme engine and the services tiles read.
    pub state: &'a AppState,
    /// Who is looking.
    pub viewer: &'a UserContext,
    /// The language the page is rendered in.
    pub language: &'a str,
    /// The role names tile visibility rules are matched against.
    pub roles: &'a [String],
    /// Every config menu as a tree for this viewer, by name, as the page
    /// context carries it in `menus_by_name`.
    pub menus_by_name: &'a BTreeMap<String, Vec<MenuTreeLink>>,
}

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

    /// Render all visible tiles for a region as a single HTML string.
    pub async fn render_region(
        &self,
        region: &str,
        stage_id: uuid::Uuid,
        path: &str,
        render: &TileRenderContext<'_>,
    ) -> Result<String> {
        let tiles = self
            .get_visible_tiles(region, stage_id, path, render.roles)
            .await?;
        let html: String = tiles
            .iter()
            .map(|tile| render_tile_html(tile, render))
            .collect();
        Ok(html)
    }
}

/// Render a single tile to an HTML string: its body by type, in the wrapper.
fn render_tile_html(tile: &Tile, render: &TileRenderContext<'_>) -> String {
    let body = match tile.tile_type.as_str() {
        "custom_html" => custom_html_body(tile),
        "menu" => render_menu_tile(tile, render),
        "gather_query" => gather_placeholder(tile),
        "chat" => chat_widget(&tile.machine_name),
        _ => "<p>Unknown tile type</p>".to_string(),
    };
    wrap_tile(tile, &body)
}

/// The markup every tile shares around its body: the tile's own classes and
/// id, and its label as a heading.
fn wrap_tile(tile: &Tile, body: &str) -> String {
    format!(
        "<div class=\"tile tile--{kind} tile--{name}\" id=\"tile-{name}\">\n\
         <h3 class=\"tile__title\">{label}</h3>\n\
         <div class=\"tile__content\">\n{body}</div>\n</div>\n",
        kind = html_escape(&tile.tile_type),
        name = html_escape(&tile.machine_name),
        label = html_escape(&tile.label),
    )
}

/// A `gather_query` tile's body: a placeholder nothing fills yet.
fn gather_placeholder(tile: &Tile) -> String {
    let query_id = tile
        .config
        .get("query_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    format!(
        "<div class=\"tile-gather\" data-query-id=\"{}\"></div>",
        html_escape(query_id)
    )
}

/// A `custom_html` tile's body, through the text format it names.
fn custom_html_body(tile: &Tile) -> String {
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
    crate::content::FilterPipeline::for_format_checked(format, true).process(body)
}

/// Render a tile's body through the first template its suggestions resolve to:
/// `elements/tile--{type}--{machine_name}`, then `elements/tile--{type}`.
///
/// A template that fails to render is logged and leaves the body empty rather
/// than failing the page around it.
fn render_tile_template(
    tile: &Tile,
    render: &TileRenderContext<'_>,
    context: &tera::Context,
) -> String {
    let theme = render.state.theme();
    let suggestions = [
        format!("elements/tile--{}--{}", tile.tile_type, tile.machine_name),
        format!("elements/tile--{}", tile.tile_type),
    ];
    let refs: Vec<&str> = suggestions.iter().map(String::as_str).collect();
    let Some(template) = theme.resolve_template(&refs) else {
        tracing::warn!(tile = %tile.machine_name, "no template for tile");
        return String::new();
    };
    match theme.tera().render(&template, context) {
        Ok(html) => html,
        Err(e) => {
            tracing::warn!(tile = %tile.machine_name, error = %e, "failed to render tile");
            String::new()
        }
    }
}

/// A `menu` tile: the links of the config menu it names (`menu_name`, default
/// `main`), filtered for the viewer, from the menus already built for this
/// render.
fn render_menu_tile(tile: &Tile, render: &TileRenderContext<'_>) -> String {
    let menu_name = tile
        .config
        .get("menu_name")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let mut context = tera::Context::new();
    context.insert("tile", tile);
    context.insert("menu_name", menu_name);
    context.insert(
        "links",
        render
            .menus_by_name
            .get(menu_name)
            .map(Vec::as_slice)
            .unwrap_or_default(),
    );
    render_tile_template(tile, render, &context)
}

/// Render the chat widget HTML.
///
/// Emits a `<div>` with data attributes and a `<script src>` for the
/// external chat-widget.js file. No inline JS — compatible with strict
/// Content-Security-Policy.
fn chat_widget(machine_name: &str) -> String {
    if !crate::routes::helpers::is_valid_machine_name(machine_name) {
        return "<p>Invalid chat widget configuration</p>".to_string();
    }
    let escaped = html_escape(machine_name);
    format!(
        r#"<div class="chat-widget" id="chat-widget-{escaped}" data-machine-name="{escaped}">
  <div class="chat-messages" id="chat-messages-{escaped}"></div>
  <form class="chat-input" id="chat-form-{escaped}">
    <input type="text" id="chat-input-{escaped}" placeholder="Ask a question..." autocomplete="off" maxlength="4096">
    <button type="submit">Send</button>
  </form>
</div>
<link rel="stylesheet" href="/static/css/chat-widget.css">
<script src="/static/js/chat-widget.js"></script>"#
    )
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
        let html = wrap_tile(&tile, &custom_html_body(&tile));
        assert!(html.contains("tile--custom_html"));
        assert!(html.contains("Test Tile"));
        assert!(html.contains("<p>Hello <strong>World</strong></p>"));
    }

    #[test]
    fn render_gather_query_tile() {
        let tile = make_tile(
            "gather_query",
            serde_json::json!({ "query_id": "blog_listing" }),
        );
        let html = wrap_tile(&tile, &gather_placeholder(&tile));
        assert!(html.contains("tile-gather"));
        assert!(html.contains("data-query-id=\"blog_listing\""));
    }

    #[test]
    fn render_chat_tile() {
        let tile = make_tile("chat", serde_json::json!({}));
        let html = wrap_tile(&tile, &chat_widget(&tile.machine_name));
        assert!(html.contains("tile--chat"));
        assert!(html.contains("chat-widget"));
        assert!(html.contains("chat-messages"));
        // JS is now in static/js/chat-widget.js, loaded via script src
        assert!(html.contains("chat-widget.js"));
    }
}
