//! Sitemap.xml and robots.txt routes.
//!
//! Generates an XML sitemap of all published items (with URL alias
//! resolution) and a robots.txt pointing to the sitemap.
//!
//! # Absolute addresses
//!
//! Every `<loc>` is absolute, built from `SITE_URL`. The sitemap protocol
//! requires it ("You must specify the protocol ... and a full path"), and a
//! sitemap is read with no request context to resolve a relative path against:
//! whoever fetches it may have arrived at a different host than the one the site
//! considers canonical. The same reasoning already governs `<link
//! rel="canonical">`, the Open Graph tags and the RSS feeds, which read the same
//! setting.
//!
//! # Translations
//!
//! A translated item is a page at more than one address, and the protocol's
//! answer is one `<url>` entry per address, each listing every address as an
//! `xhtml:link` alternate. The alternate set comes from
//! [`crate::routes::helpers::build_hreflang_links`], the
//! same function the item and front pages use for their `<head>` tags, so a
//! crawler cannot be told two different stories about the same page.

use std::collections::HashMap;

use axum::Router;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::menu::MenuDefinition;
use crate::models::SiteConfig;
use crate::models::stage::LIVE_STAGE_ID;
use crate::routes::helpers::{TranslationLink, build_hreflang_links, xml_escape};
use crate::state::AppState;

/// The paths this module serves.
///
/// A plugin menu entry claiming one of these is refused at startup by
/// [`ensure_reserved_paths_unclaimed`]. These routes are kernel infrastructure
/// (see the module docs on `crate::routes::feed` for why they cannot live in a
/// plugin), and before this check a plugin claiming one reached `Router::merge`,
/// which panics on an overlapping route — a plugin taking the process down at
/// boot with axum's message rather than the kernel's.
pub const RESERVED_PATHS: [&str; 2] = ["/sitemap.xml", "/robots.txt"];

/// Refuse at startup when a plugin menu entry claims a path this module serves.
///
/// Returns every offending (plugin, path) pair rather than the first, so one
/// restart names every collision instead of one per attempt.
pub fn reserved_path_claims(menus: &[MenuDefinition]) -> Vec<(String, String)> {
    menus
        .iter()
        .filter(|menu| {
            let path = menu.path.trim_end_matches('/');
            RESERVED_PATHS
                .iter()
                .any(|reserved| path.eq_ignore_ascii_case(reserved))
        })
        .map(|menu| (menu.plugin.clone(), menu.path.clone()))
        .collect()
}

/// Startup gate: a plugin may not serve `/sitemap.xml` or `/robots.txt`.
///
/// # Errors
///
/// Names every plugin claiming a reserved path, and what to do about it.
pub fn ensure_reserved_paths_unclaimed(menus: &[MenuDefinition]) -> anyhow::Result<()> {
    let claims = reserved_path_claims(menus);
    if claims.is_empty() {
        return Ok(());
    }

    let detail = claims
        .iter()
        .map(|(plugin, path)| format!("  {plugin} declares a menu entry for {path}"))
        .collect::<Vec<_>>()
        .join("\n");

    anyhow::bail!(
        "a plugin claims a path the kernel serves and cannot yield:\n{detail}\n\
         {} are served by the kernel because they render query results under the \
         stage and access filters, which plugin space has no seam onto. Change the \
         plugin's menu path, or disable the plugin.",
        RESERVED_PATHS.join(" and ")
    )
}

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

/// Row type for the bulk translation-existence lookup.
#[derive(sqlx::FromRow)]
struct TranslationRow {
    item_id: uuid::Uuid,
    language: String,
}

/// The site's base URL with any trailing slash removed.
///
/// Every path this module emits already starts with `/`, so a `SITE_URL` stored
/// as `https://example.com/` would otherwise produce `https://example.com//blog`
/// — a different URL to a crawler, and a duplicate of one it already has.
fn canonical_base(site_url: &str) -> &str {
    site_url.trim_end_matches('/')
}

/// One `<url>` entry, rendered.
///
/// `alternates` is empty for a page that exists in one language: a lone
/// `xhtml:link` pointing at the entry's own address says nothing and is noise in
/// every entry of a monolingual site's sitemap.
fn render_url_entry(
    base_url: &str,
    path: &str,
    lastmod: &str,
    alternates: &[serde_json::Value],
) -> String {
    use std::fmt::Write as _;

    let mut entry = String::from("  <url>\n");
    // Infallible: writing to a String.
    let _ = writeln!(
        entry,
        "    <loc>{}</loc>",
        xml_escape(&format!("{base_url}{path}"))
    );
    if !lastmod.is_empty() {
        let _ = writeln!(entry, "    <lastmod>{}</lastmod>", xml_escape(lastmod));
    }
    for alternate in alternates {
        let (Some(lang), Some(href)) = (
            alternate.get("lang").and_then(|v| v.as_str()),
            alternate.get("href").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        let _ = writeln!(
            entry,
            "    <xhtml:link rel=\"alternate\" hreflang=\"{}\" href=\"{}\"/>",
            xml_escape(lang),
            xml_escape(&format!("{base_url}{href}"))
        );
    }
    entry.push_str("  </url>\n");
    entry
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

    // Which items exist in which languages, in one query rather than one per
    // item: `available_translations` asks per item, which is fine for a page
    // rendering itself and is an N+1 across the whole site.
    let item_ids: Vec<uuid::Uuid> = items.iter().map(|row| row.id).collect();
    let mut translated: HashMap<uuid::Uuid, Vec<String>> = HashMap::new();
    match sqlx::query_as::<_, TranslationRow>(
        "SELECT item_id, language FROM item_translation \
         WHERE item_id = ANY($1) ORDER BY item_id, language",
    )
    .bind(&item_ids)
    .fetch_all(state.db())
    .await
    {
        Ok(rows) => {
            for row in rows {
                translated
                    .entry(row.item_id)
                    .or_default()
                    .push(row.language);
            }
        }
        Err(e) => {
            // A sitemap of default-language addresses is worth serving; one that
            // 500s because the translation table was unreadable is not.
            tracing::warn!(error = %e, "failed to load translations for sitemap, omitting alternates");
        }
    }

    let base_url = canonical_base(&state.runtime().site_url);
    let default_language = state.default_language();

    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str(
        "<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\" \
         xmlns:xhtml=\"http://www.w3.org/1999/xhtml\">\n",
    );

    for row in &items {
        let source = format!("/item/{}", row.id);
        let canonical_path = aliases.get(&source).cloned().unwrap_or(source);
        let lastmod = chrono::DateTime::from_timestamp(row.changed, 0)
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_default();

        // The same shape `routes::helpers::available_translations` builds: the
        // canonical address in the default language, then that address behind a
        // `/{lang}` prefix for every other language the item exists in and the
        // site still configures.
        let mut translations = vec![TranslationLink {
            language: default_language.to_string(),
            path: canonical_path.clone(),
        }];
        for language in translated.get(&row.id).into_iter().flatten() {
            if language == default_language || !state.known_languages().contains(language) {
                continue;
            }
            translations.push(TranslationLink {
                path: format!("/{language}{canonical_path}"),
                language: language.clone(),
            });
        }

        // One entry per address, each carrying the whole alternate set. A page in
        // one language gets no alternates at all.
        let alternates = if translations.len() > 1 {
            build_hreflang_links(&translations, default_language)
        } else {
            Vec::new()
        };
        for translation in &translations {
            xml.push_str(&render_url_entry(
                base_url,
                &translation.path,
                &lastmod,
                &alternates,
            ));
        }
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

    // Sitemap reference. Absolute for the same reason every `<loc>` is: the
    // robots.txt specification requires the sitemap address to be a full URL,
    // and a crawler reading a relative one has nothing to resolve it against.
    let base_url = canonical_base(&state.runtime().site_url);
    sections.push(format!("Sitemap: {base_url}/sitemap.xml"));

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

    fn menu(plugin: &str, path: &str) -> MenuDefinition {
        MenuDefinition {
            path: path.to_string(),
            title: "Fixture".to_string(),
            plugin: plugin.to_string(),
            permission: String::new(),
            callback: String::new(),
            parent: None,
            weight: 0,
            visible: true,
            method: "GET".to_string(),
            handler_type: "page".to_string(),
            local_task: false,
        }
    }

    // --- absolute addresses (the sitemap protocol requires them) ---

    /// The defect: `<loc>` carried a bare path, which is not a location.
    #[test]
    fn a_loc_is_absolute() {
        let entry = render_url_entry("https://example.com", "/blog/hello", "2026-09-18", &[]);
        assert!(
            entry.contains("<loc>https://example.com/blog/hello</loc>"),
            "got {entry}"
        );
        assert!(
            !entry.contains("<loc>/blog/hello</loc>"),
            "a relative loc is what this fixes"
        );
        assert!(entry.contains("<lastmod>2026-09-18</lastmod>"));
    }

    /// A `SITE_URL` saved with a trailing slash must not produce a doubled one:
    /// `https://example.com//blog` is a different URL to a crawler.
    #[test]
    fn the_base_url_never_doubles_the_slash() {
        assert_eq!(
            canonical_base("https://example.com/"),
            "https://example.com"
        );
        assert_eq!(canonical_base("https://example.com"), "https://example.com");
        assert_eq!(
            canonical_base("https://example.com/sub/"),
            "https://example.com/sub"
        );
        let entry = render_url_entry(canonical_base("https://example.com/"), "/blog", "", &[]);
        assert!(
            entry.contains("<loc>https://example.com/blog</loc>"),
            "got {entry}"
        );
    }

    /// An alias may legitimately contain an `&`, and an unescaped one makes the
    /// whole document unparseable rather than one entry wrong.
    #[test]
    fn an_alias_with_xml_syntax_is_escaped() {
        let entry = render_url_entry("https://example.com", "/r&d?a=1&b=2", "", &[]);
        assert!(entry.contains("/r&amp;d?a=1&amp;b=2"), "got {entry}");
        assert!(!entry.contains("?a=1&b=2"));
    }

    /// An item with no lastmod (an unrepresentable timestamp) emits no empty
    /// element rather than one a validator rejects.
    #[test]
    fn an_absent_lastmod_is_omitted() {
        let entry = render_url_entry("https://example.com", "/blog", "", &[]);
        assert!(!entry.contains("<lastmod>"), "got {entry}");
    }

    // --- translations ---

    /// A translated item is a page at more than one address, and each address
    /// names the whole set — including `x-default`, which is what
    /// `build_hreflang_links` adds.
    #[test]
    fn a_translated_entry_carries_absolute_alternates() {
        let translations = vec![
            TranslationLink {
                language: "en".to_string(),
                path: "/blog/hello".to_string(),
            },
            TranslationLink {
                language: "it".to_string(),
                path: "/it/blog/hello".to_string(),
            },
        ];
        let alternates = build_hreflang_links(&translations, "en");
        let entry = render_url_entry(
            "https://example.com",
            "/it/blog/hello",
            "2026-09-18",
            &alternates,
        );

        assert!(entry.contains("<loc>https://example.com/it/blog/hello</loc>"));
        assert!(entry.contains(
            "<xhtml:link rel=\"alternate\" hreflang=\"en\" href=\"https://example.com/blog/hello\"/>"
        ), "got {entry}");
        assert!(entry.contains(
            "<xhtml:link rel=\"alternate\" hreflang=\"it\" href=\"https://example.com/it/blog/hello\"/>"
        ), "got {entry}");
        assert!(
            entry.contains("hreflang=\"x-default\" href=\"https://example.com/blog/hello\""),
            "x-default points at the default language: {entry}"
        );
        assert!(
            !entry.contains("href=\"/blog/hello\""),
            "an alternate href is absolute too"
        );
    }

    /// A monolingual site's sitemap gets no alternates: a lone `xhtml:link`
    /// pointing at the entry's own address says nothing.
    #[test]
    fn a_single_language_entry_has_no_alternates() {
        let entry = render_url_entry("https://example.com", "/blog", "2026-09-18", &[]);
        assert!(!entry.contains("xhtml:link"), "got {entry}");
    }

    // --- reserved paths ---

    /// The behaviour that is kept: a plugin cannot serve `/sitemap.xml`. What
    /// changed is that it is refused with a message naming the plugin instead of
    /// panicking inside `Router::merge`.
    #[test]
    fn a_plugin_claiming_sitemap_is_refused_at_startup() {
        let menus = vec![menu("some_plugin", "/sitemap.xml")];
        let error = ensure_reserved_paths_unclaimed(&menus)
            .expect_err("a plugin claiming /sitemap.xml must be refused");
        let message = error.to_string();
        assert!(message.contains("some_plugin"), "got {message}");
        assert!(message.contains("/sitemap.xml"), "got {message}");
    }

    #[test]
    fn a_plugin_claiming_robots_txt_is_refused_at_startup() {
        let menus = vec![menu("some_plugin", "/robots.txt")];
        assert!(ensure_reserved_paths_unclaimed(&menus).is_err());
    }

    /// Every collision is named at once, so one restart is enough to see them
    /// all.
    #[test]
    fn every_reserved_claim_is_named() {
        let menus = vec![menu("one", "/sitemap.xml"), menu("two", "/robots.txt")];
        let message = ensure_reserved_paths_unclaimed(&menus)
            .expect_err("both claims refused")
            .to_string();
        assert!(message.contains("one"), "got {message}");
        assert!(message.contains("two"), "got {message}");
    }

    /// A trailing slash or different case is the same claim; a path that merely
    /// starts with a reserved one is not.
    #[test]
    fn reserved_matching_is_on_the_whole_path() {
        assert_eq!(reserved_path_claims(&[menu("p", "/sitemap.xml/")]).len(), 1);
        assert_eq!(reserved_path_claims(&[menu("p", "/Sitemap.XML")]).len(), 1);
        assert!(reserved_path_claims(&[menu("p", "/sitemap.xml.gz")]).is_empty());
        assert!(reserved_path_claims(&[menu("p", "/api/sitemap.xml")]).is_empty());
    }

    #[test]
    fn an_ordinary_plugin_menu_is_not_refused() {
        let menus = vec![menu("blog", "/blog"), menu("api", "/api/things")];
        assert!(ensure_reserved_paths_unclaimed(&menus).is_ok());
    }

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
