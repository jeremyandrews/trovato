//! Tile rendering service.
//!
//! Loads visible tiles for a given region/stage/path and renders them to HTML.

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use sqlx::PgPool;

use crate::gather::{GatherDenied, GatherQuery};
use crate::menu::MenuTreeLink;
use crate::models::tile::{CreateTile, Tile, UpdateTile, gather_tile_items};
use crate::routes::gather::{ExecuteParams, execute_query_only, query_language};
use crate::routes::helpers::{TranslationLink, html_escape};
use crate::services::audit::AuditTrail;
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
    /// The page's root values, as the page context carries them in
    /// `page_root` (see `content::page_builder::page_root_values`): every
    /// tile template receives them, so a tile can follow a page's settings.
    pub page_root: &'a serde_json::Value,
    /// The address this page is read at, as asked for: `requested_path` in
    /// the page context, the language switcher's own address for the active
    /// language.
    pub current_path: &'a str,
    /// Every language this page can be read in and its address there, when
    /// the route knows (an item page does; most others do not). `None` for a
    /// tile to fall back to a language's front page, the same way a crawler
    /// is offered no `hreflang` alternate at all for a page with none.
    pub translations: Option<&'a [TranslationLink]>,
}

/// Service for loading and rendering tiles.
pub struct TileService {
    db: PgPool,
    audit: AuditTrail,
}

impl TileService {
    /// Create a new tile service.
    pub fn new(db: PgPool, audit: AuditTrail) -> Self {
        Self { db, audit }
    }

    /// Record one tile write in the audit log.
    async fn audit(&self, action: &str, tile: &Tile) {
        self.audit
            .record(
                action,
                "tile",
                tile.id,
                None,
                serde_json::json!({ "machine_name": tile.machine_name, "region": tile.region }),
            )
            .await;
    }

    /// Create a tile.
    ///
    /// Tile writes go through here rather than the model so that what a tile
    /// write must also do has one place to live. Today there is nothing more:
    /// tiles are read per render, not cached, and the kernel defines no tile
    /// taps.
    pub async fn create(&self, input: CreateTile) -> Result<Tile> {
        let tile = Tile::create(&self.db, input).await?;
        self.audit("create", &tile).await;
        Ok(tile)
    }

    /// Update a tile. `None` when there is no such tile.
    pub async fn update(&self, id: uuid::Uuid, input: UpdateTile) -> Result<Option<Tile>> {
        let tile = Tile::update(&self.db, id, input).await?;
        if let Some(tile) = &tile {
            self.audit("update", tile).await;
        }
        Ok(tile)
    }

    /// Delete a tile. `false` when there is no such tile.
    pub async fn delete(&self, id: uuid::Uuid) -> Result<bool> {
        let deleted = Tile::delete(&self.db, id).await?;
        if deleted {
            self.audit
                .record("delete", "tile", id, None, serde_json::json!({}))
                .await;
        }
        Ok(deleted)
    }

    /// Get visible tiles for a region, filtered by path, role and audience
    /// visibility rules. `authenticated` decides an `audience` rule; it is the
    /// viewer's actual signed-in state, not derived from `user_roles` (every
    /// viewer, signed in or not, can carry `anonymous user` there).
    pub async fn get_visible_tiles(
        &self,
        region: &str,
        stage_id: uuid::Uuid,
        path: &str,
        user_roles: &[String],
        authenticated: bool,
    ) -> Result<Vec<Tile>> {
        let tiles = Tile::list_by_region(&self.db, region, stage_id).await?;

        Ok(tiles
            .into_iter()
            .filter(|t| t.is_visible(path, user_roles, authenticated))
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
            .get_visible_tiles(
                region,
                stage_id,
                path,
                render.roles,
                render.viewer.authenticated,
            )
            .await?;
        let mut html = String::new();
        for tile in &tiles {
            if let Some(tile_html) = render_tile_html(tile, render).await {
                html.push_str(&tile_html);
            }
        }
        Ok(html)
    }
}

/// Render a single tile to an HTML string: its body by type, in the wrapper.
///
/// `None` when the tile has nothing it may show this viewer, and so is left
/// off the page entirely rather than drawn as an empty box.
async fn render_tile_html(tile: &Tile, render: &TileRenderContext<'_>) -> Option<String> {
    let body = match tile.tile_type.as_str() {
        "custom_html" => render_custom_html_tile(tile, render)?,
        "menu" => render_menu_tile(tile, render),
        "gather_query" => render_gather_tile(tile, render).await?,
        "language_switcher" => render_language_switcher_tile(tile, render)?,
        "chat" => chat_widget(&tile.machine_name),
        _ => "<p>Unknown tile type</p>".to_string(),
    };
    Some(wrap_tile(tile, &body))
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

/// A `gather_query` tile: the first rows of the gather its config names
/// (`query_id`), as many as its `items` (default 5).
///
/// The gather runs as the viewer, in the page's language, on its first page,
/// through [`execute_query_only`]: the path the gather's own JSON and feed
/// take, with the same authorization, stages and item access. One run per
/// tile, fetching only the rows the tile shows.
///
/// `None`, and so no tile, when the viewer may not run the gather; when the
/// gather does not exist, the config is invalid or the run fails, also with a
/// warning naming the tile.
async fn render_gather_tile(tile: &Tile, render: &TileRenderContext<'_>) -> Option<String> {
    let state = render.state;
    let query_id = tile
        .config
        .get("query_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let Some(query) = state.gather().get_query(query_id) else {
        tracing::warn!(tile = %tile.machine_name, query_id, "gather tile names no registered gather");
        return None;
    };
    let items = match gather_tile_items(&tile.config) {
        Ok(items) => items,
        Err(message) => {
            tracing::warn!(tile = %tile.machine_name, query_id, message, "gather tile config is invalid");
            return None;
        }
    };

    let result = match execute_query_only(
        state,
        query_id,
        ExecuteParams::new(1, String::new(), HashMap::new()),
        query_language(render.language, state.default_language()),
        render.viewer.clone(),
        Some(items),
    )
    .await
    {
        Ok(result) => result,
        Err(e) if e.downcast_ref::<GatherDenied>().is_some() => return None,
        Err(e) => {
            tracing::warn!(tile = %tile.machine_name, query_id, error = %e, "gather tile query failed");
            return None;
        }
    };

    let mut context = tera::Context::new();
    context.insert("tile", tile);
    context.insert("rows", &result.items);
    context.insert("empty_text", &query.display.empty_text);
    context.insert("more_url", &gather_page_path(&query));
    Some(render_tile_template(tile, render, &context))
}

/// Where a gather's own page is: its canonical URL, else its first route that
/// takes no arguments, else `/gather/{query_id}`.
fn gather_page_path(query: &GatherQuery) -> String {
    query
        .display
        .canonical_url
        .clone()
        .or_else(|| {
            query
                .display
                .routes
                .iter()
                .find(|route| route.params.is_empty())
                .map(|route| route.path.clone())
        })
        .unwrap_or_else(|| format!("/gather/{}", query.query_id))
}

/// A `custom_html` tile: its body, through the text format it names, or,
/// when a template `elements/tile--custom_html--{machine_name}` or
/// `elements/tile--custom_html` is on the search path, that template's
/// rendering of it, with `tile`, `body` (the filtered HTML) and `page_root`.
///
/// A template is how a sitewide tile follows a page's settings: a footer
/// call to action that a page's root field hides or replaces. `None`, and so
/// no tile at all, when the template renders nothing but whitespace.
fn render_custom_html_tile(tile: &Tile, render: &TileRenderContext<'_>) -> Option<String> {
    let body = custom_html_body(tile);
    let suggestions = [
        format!("elements/tile--custom_html--{}", tile.machine_name),
        "elements/tile--custom_html".to_string(),
    ];
    let refs: Vec<&str> = suggestions.iter().map(String::as_str).collect();
    let Some(template) = render.state.theme().resolve_template(&refs) else {
        return Some(body);
    };
    let mut context = tera::Context::new();
    context.insert("tile", tile);
    context.insert("body", &body);
    context.insert("page_root", render.page_root);
    match render.state.theme().tera().render(&template, &context) {
        Ok(html) if html.trim().is_empty() => None,
        Ok(html) => Some(html),
        Err(e) => {
            tracing::warn!(tile = %tile.machine_name, error = %e, "failed to render tile");
            Some(String::new())
        }
    }
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

    // The same view path every other body goes through: full_html is cleaned
    // by TrustedHtmlFilter, so a tile body is sanitized regardless of who
    // wrote it. An unknown format string falls through to plain_text (safe).
    crate::content::FilterPipeline::for_format(format).process(body)
}

/// Render a tile's body through the first template its suggestions resolve to:
/// `elements/tile--{type}--{machine_name}`, then `elements/tile--{type}`, with
/// the page's root values added as `page_root`.
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
    let mut context = context.clone();
    context.insert("page_root", render.page_root);
    match theme.tera().render(&template, &context) {
        Ok(html) => html,
        Err(e) => {
            tracing::warn!(tile = %tile.machine_name, error = %e, "failed to render tile");
            String::new()
        }
    }
}

/// A `language_switcher` tile: one link per language the site has enabled, to
/// this same page in that language when the route knows where that is (an
/// item page does, through `translations`), else to that language's front
/// page — the fallback Drupal's own switcher uses for a page with no
/// translation. No settings beyond the usual title and visibility.
///
/// `None`, and so no tile at all, on a site with one enabled language: there
/// is nothing to switch to.
fn render_language_switcher_tile(tile: &Tile, render: &TileRenderContext<'_>) -> Option<String> {
    let links = language_switcher_links(
        render.state.known_languages(),
        render.state.default_language(),
        render.language,
        render.current_path,
        render.translations,
    );
    if links.is_empty() {
        return None;
    }
    let mut context = tera::Context::new();
    context.insert("tile", tile);
    context.insert("links", &links);
    Some(render_tile_template(tile, render, &context))
}

/// The links a language switcher tile shows: one per language in
/// `known_languages`, each `{ language, path, current }`. Empty when there is
/// one language or fewer — nothing to switch to.
///
/// The active language links to `current_path`, whatever page that is. Every
/// other language links to its address from `translations` when the route
/// supplied one for it, else to that language's front page: the fallback
/// Drupal's own switcher block uses for a page with no translation.
fn language_switcher_links(
    known_languages: &[String],
    default_language: &str,
    active_language: &str,
    current_path: &str,
    translations: Option<&[TranslationLink]>,
) -> Vec<serde_json::Value> {
    if known_languages.len() <= 1 {
        return Vec::new();
    }
    known_languages
        .iter()
        .map(|language| {
            let current = language == active_language;
            let path = if current {
                current_path.to_string()
            } else {
                translations
                    .and_then(|list| list.iter().find(|t| &t.language == language))
                    .map(|t| t.path.clone())
                    .unwrap_or_else(|| language_front_page(language, default_language))
            };
            serde_json::json!({
                "language": language,
                "path": path,
                "current": current,
            })
        })
        .collect()
}

/// Where a language's front page is: `/` for the site's default language,
/// `/{language}` for any other — the same address the front page route reads
/// a language prefix back from.
fn language_front_page(language: &str, default_language: &str) -> String {
    if language == default_language {
        "/".to_string()
    } else {
        format!("/{language}")
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

    // SECURITY REGRESSION TEST — B151: a custom_html tile in full_html is
    // sanitized like any other render path, regardless of who wrote it.
    #[test]
    fn render_custom_html_tile_in_full_html_strips_script() {
        let tile = make_tile(
            "custom_html",
            serde_json::json!({
                "body": r#"<p class="lead">Hi</p><script>alert(1)</script><img src=x onerror="alert(1)">"#,
                "format": "full_html"
            }),
        );
        let html = wrap_tile(&tile, &custom_html_body(&tile));
        assert!(html.contains(r#"<p class="lead">Hi</p>"#), "{html}");
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("onerror"), "{html}");
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

    #[test]
    fn a_gather_page_is_its_canonical_url_then_a_plain_route_then_its_id() {
        let mut query: GatherQuery = serde_json::from_value(serde_json::json!({
            "query_id": "upcoming",
            "label": "Upcoming",
            "description": null,
            "definition": { "base_table": "item" },
            "display": {
                "format": "list",
                "items_per_page": 10,
                "routes": [
                    { "path": "/topics/{slug}", "params": [{ "segment": "slug", "param": "tag" }] },
                    { "path": "/upcoming" }
                ]
            },
            "plugin": "core"
        }))
        .unwrap();
        assert_eq!(gather_page_path(&query), "/upcoming");
        query.display.canonical_url = Some("/events".into());
        assert_eq!(gather_page_path(&query), "/events");
        query.display.canonical_url = None;
        query.display.routes.clear();
        assert_eq!(gather_page_path(&query), "/gather/upcoming");
    }

    fn lang(code: &str) -> String {
        code.to_string()
    }

    #[test]
    fn a_single_language_site_has_nothing_to_switch_to() {
        let links = language_switcher_links(&[lang("en")], "en", "en", "/about", None);
        assert!(links.is_empty());
    }

    #[test]
    fn one_link_per_enabled_language_the_current_one_marked() {
        let known = vec![lang("en"), lang("it"), lang("he")];
        let links = language_switcher_links(&known, "en", "it", "/it/why", None);
        assert_eq!(links.len(), 3);

        let en = links.iter().find(|l| l["language"] == "en").unwrap();
        assert_eq!(en["path"], "/");
        assert_eq!(en["current"], false);

        let it = links.iter().find(|l| l["language"] == "it").unwrap();
        assert_eq!(it["path"], "/it/why");
        assert_eq!(it["current"], true);

        let he = links.iter().find(|l| l["language"] == "he").unwrap();
        assert_eq!(he["path"], "/he");
        assert_eq!(he["current"], false);
    }

    #[test]
    fn a_translated_page_links_to_its_own_alias_in_each_language() {
        let known = vec![lang("en"), lang("it")];
        let translations = vec![
            TranslationLink {
                language: "en".to_string(),
                path: "/test-article".to_string(),
            },
            TranslationLink {
                language: "it".to_string(),
                path: "/it/articolo-di-prova".to_string(),
            },
        ];
        let links =
            language_switcher_links(&known, "en", "en", "/test-article", Some(&translations));

        let it = links.iter().find(|l| l["language"] == "it").unwrap();
        assert_eq!(it["path"], "/it/articolo-di-prova");
        assert_eq!(it["current"], false);
    }

    #[test]
    fn a_language_with_no_translation_falls_back_to_its_front_page() {
        let known = vec![lang("en"), lang("it"), lang("he")];
        let translations = vec![TranslationLink {
            language: "en".to_string(),
            path: "/test-article".to_string(),
        }];
        let links =
            language_switcher_links(&known, "en", "en", "/test-article", Some(&translations));

        let he = links.iter().find(|l| l["language"] == "he").unwrap();
        assert_eq!(he["path"], "/he");
    }
}
