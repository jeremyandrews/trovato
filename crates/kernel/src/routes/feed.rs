//! RSS 2.0 and Atom 1.0 feeds for gather queries.
//!
//! A gather query declares a feed in its display config
//! ([`GatherFeed`]) and gets an RSS document
//! at the path it names and an Atom document at that path's `.atom` sibling
//! ([`atom_path`]). Routes are built once at startup, the same shape
//! [`crate::routes::gather_routes::build_gather_route_router`] uses for gather
//! route aliases.
//!
//! # Why this is kernel rather than a plugin
//!
//! Feeds are a feature, and the kernel-minimality rule sends features to
//! plugins. The blocker is that a feed is a rendering of a *query result*, and
//! query execution is kernel infrastructure: `execute_query_only` applies the
//! stage filter, the access filter and the D-26 over-fetch bounds for a
//! specific viewer. Plugin space has no seam onto it — the `item-api` host
//! interface offers `query-items`, which is an unordered, unfiltered
//! `SELECT ... LIMIT 100` with no viewer, so a plugin-built feed would leak
//! whatever the access filter exists to withhold. The plugin-facing seam that
//! would fix that is plugin-contract surface, and the contract is frozen before
//! 1.0.
//!
//! The precedent is `crate::routes::sitemap`, which serves `sitemap.xml` and
//! `robots.txt` from the kernel for the same reason.
//!
//! # What a feed shows
//!
//! Whatever the viewer's own execution of the query returns. A feed URL fetched
//! by an anonymous aggregator is an anonymous execution, so it carries exactly
//! the rows an anonymous visitor would see on the query's page. Nothing about a
//! feed is cached on the server: each request runs the query as its viewer.
//!
//! # Feeds with an argument
//!
//! A feed path may carry one argument as a whole segment
//! (`/blog/author/{user}/feed.xml`), and `GatherFeed::paths` lists more
//! addresses for the same feed. The argument is bound by the gather's own
//! `display.routes` mapping with the same segment name and resolved by
//! `gather_routes::resolve_route_params`, the code the gather's page route runs, so a feed
//! and its page cannot disagree about what `/author/{user}` means. An argument
//! that names nothing is a 404. A page reached with an argument advertises the
//! feed with the same argument ([`argument_feed_links`]).
//!
//! # What an entry carries
//!
//! - an id that never changes: `urn:uuid:<item id>`, so a new URL alias does
//!   not make every reader show old posts as new;
//! - the absolute link, from `SITE_URL` and the row's URL alias;
//! - the author's account name (`dc:creator`, Atom `<author>`), never an email;
//! - every taxonomy term on the item (`<category>`);
//! - a plain-text summary: the item's summary field when it has one, otherwise
//!   its body with the markup stripped, cut at a word boundary;
//! - with `full_content`, the body as HTML too.
//!
//! Authors and terms are loaded in one batched query each, whatever the number
//! of entries; the alias is already on the row (the gather service resolves it).
//!
//! # Escaping
//!
//! One rule, the one B123 settled for search snippets: text is extracted from
//! stored markup with `content::page_meta::plain_text` and escaped
//! exactly once ([`xml_escape`]). The only HTML a feed carries is the
//! `full_content` body, rendered through its text format
//! ([`FilterPipeline::for_format`]) and then either wrapped in CDATA (RSS)
//! or escaped once as the text of a `type="html"` element (Atom).
//!
//! # Polling
//!
//! Every response carries an `ETag` and a `Last-Modified`. The `ETag` hashes
//! the entries' ids, their `changed` timestamps, links and author names, a
//! stamp for the taxonomy terms the entries carry (B172), the resolved
//! channel link, and the feed's config. `Last-Modified` is the latest of the
//! newest entry's `changed` (or `created`), the gather's own `changed` (so a
//! title, description or other config edit moves it, not only the `ETag`),
//! and, when the channel link resolves through a `url_alias` row rather than
//! a fixed path, that row's own `changed` (so repointing the link moves it
//! too). A request whose `If-None-Match` (or, with none, `If-Modified-Since`)
//! still matches is answered `304` before terms are loaded or anything is
//! rendered.
//!
//! An item's title, summary and body are not hashed directly: they can only
//! change through an edit to the item, which is the same write that bumps
//! `changed`, so `changed` already stands in for them exactly. An author's
//! account name and a term's label are edited on a different row (`users`,
//! `category_tag`) that an entry's `changed` knows nothing about, which is
//! what B172 fixed: the author name actually fetched for the entry goes into
//! the hash, and so does each printed term's id and label, read in one
//! batched query for the whole response before the `304` check (never one
//! per entry). A global counter such as the `Categories` cache generation
//! would have saved that query, but it moves on any tag write anywhere on the
//! install, so no feed would ever see two polls with the same `ETag` while
//! anyone edits tags; the labels themselves move only the feeds that print
//! them.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tower_sessions::Session;
use uuid::Uuid;

use crate::content::FilterPipeline;
use crate::content::body_field::{BODY_FIELD_NAMES, field_text};
use crate::content::page_meta::{plain_text, truncate_on_word};
use crate::gather::types::{GatherFeed, GatherQuery, GatherRouteParam};
use crate::models::stage::LIVE_STAGE_ID;
use crate::routes::gather::ExecuteParams;
use crate::routes::gather_routes::{RouteParamError, resolve_route_params};
use crate::routes::helpers::xml_escape;
use crate::state::AppState;

/// Largest number of entries a feed will carry, whatever its config asks for.
///
/// A feed is fetched unauthenticated and on a timer by every aggregator
/// subscribed to it, so its cost is not bounded by human patience the way a page
/// is.
const MAX_FEED_ITEMS: u32 = 200;

/// Longest summary cut from a body, in bytes, before the ellipsis.
const SUMMARY_MAX_LEN: usize = 300;

/// `max-age` for a feed fetched without a signed-in session.
const ANONYMOUS_MAX_AGE_SECS: u32 = 300;

/// The fields read as an item's summary, in order, before falling back to its
/// body. `summary` is what row templates show; the rest are the names content
/// types declare for a hand-written teaser.
const SUMMARY_FIELDS: [&str; 4] = [
    "summary",
    "field_summary",
    "field_teaser",
    "field_description",
];

/// The two documents one feed declaration serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FeedFormat {
    Rss,
    Atom,
}

impl FeedFormat {
    fn content_type(self) -> &'static str {
        match self {
            Self::Rss => "application/rss+xml; charset=utf-8",
            Self::Atom => "application/atom+xml; charset=utf-8",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Rss => "rss",
            Self::Atom => "atom",
        }
    }
}

/// One registered feed.
#[derive(Debug, Clone)]
struct FeedConfig {
    /// Gather query to execute.
    query_id: String,
    /// RSS path, as declared (a pattern when the feed takes an argument).
    path: String,
    /// Atom path, derived from `path`.
    atom_path: String,
    /// Channel / feed title.
    title: String,
    /// Channel description / Atom subtitle.
    description: String,
    /// Number of entries to carry.
    items: u32,
    /// Carry the body as HTML as well as the summary.
    full_content: bool,
    /// The page the feed is a feed of: for an argument feed, the gather route
    /// with the same argument when there is one; else the query's canonical
    /// URL; else the outbound URL alias of its `/gather/{query_id}` page, when
    /// one exists; else that page itself.
    home_path: HomePath,
    /// The gather query's own `changed`, so a config edit changes the `ETag`.
    query_changed: i64,
    /// The path argument and the `display.routes` mapping that binds it.
    argument: Option<GatherRouteParam>,
}

/// Where a feed's channel link comes from.
///
/// The first two tiers are literal paths, known at registration from the
/// query's own config (an argument route or `canonical_url`), the same way
/// `display.routes` and `canonical_url` are already read once at startup
/// elsewhere in this file — editing either needs a restart to reach a running
/// feed. The third has no config to read: whether `/gather/{query_id}` has an
/// outbound alias is a fact of the `url_alias` table, resolved per request
/// (through [`crate::gather::GatherService::outbound_alias_for_gather`]'s
/// cache, so this costs no query once warm) so that an alias written after
/// startup reaches the feed without one.
#[derive(Debug, Clone)]
enum HomePath {
    /// The argument's own route, or the query's `canonical_url`.
    Fixed(String),
    /// Neither of the above: resolved at request time to the outbound alias
    /// of `/gather/{query_id}`, else that path itself.
    GatherDefault,
}

/// A feed as a template sees it, for autodiscovery link tags.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FeedLink {
    /// Path the RSS document is served at.
    pub path: String,
    /// Path the Atom document is served at.
    pub atom_path: String,
    /// Human-readable feed title.
    pub title: String,
}

/// Every declared path of a feed: `path`, then `paths`.
fn declared_paths(feed: &GatherFeed) -> impl Iterator<Item = &String> {
    std::iter::once(&feed.path).chain(feed.paths.iter())
}

/// Every feed declared by a gather query that takes no argument, for the
/// `<link rel="alternate">` tags on every page. A feed with an argument is
/// advertised only on the page showing the same argument
/// ([`argument_feed_links`]).
///
/// Sorted by path so the head is stable between restarts rather than following
/// registry iteration order.
pub fn feed_links(queries: &[GatherQuery]) -> Vec<FeedLink> {
    let mut links: Vec<FeedLink> = queries
        .iter()
        .flat_map(|query| {
            let title = feed_title(query);
            query
                .display
                .feed
                .iter()
                .flat_map(declared_paths)
                .filter(|path| matches!(path_argument(path), Ok(None)))
                .filter_map(move |path| {
                    Some(FeedLink {
                        path: path.clone(),
                        atom_path: atom_path(path)?,
                        title: title.clone(),
                    })
                })
        })
        .collect();
    links.sort_by(|a, b| a.path.cmp(&b.path));
    links.dedup_by(|a, b| a.path == b.path);
    links
}

/// The feeds of `query` that take one of the arguments a page of the same
/// gather was reached with, filled in with the page's values: what
/// `/blog/author/jeremy` advertises is `/blog/author/jeremy/feed.xml`.
///
/// `segments` are the page route's raw path segments, before resolution, which
/// is what the feed's own path takes.
pub fn argument_feed_links(
    query: &GatherQuery,
    segments: &HashMap<String, String>,
) -> Vec<FeedLink> {
    let Some(feed) = query.display.feed.as_ref() else {
        return Vec::new();
    };
    let title = feed_title(query);
    declared_paths(feed)
        .filter_map(|path| {
            let segment = path_argument(path).ok()??;
            let value = segments.get(&segment)?;
            binding_for(query, &segment)?;
            let atom = atom_path(path)?;
            Some(FeedLink {
                path: fill(path, &segment, value),
                atom_path: fill(&atom, &segment, value),
                title: argument_title(&title, value),
            })
        })
        .collect()
}

/// The Atom sibling of an RSS feed path: the last segment's extension replaced
/// by `.atom`, or `.atom` appended when it has none.
///
/// `/rss/blog.xml` → `/rss/blog.atom`, `/blog/feed` → `/blog/feed.atom`. Derived
/// rather than declared so that every feed gets Atom with no config change and
/// no second path to keep unique; `None` for a path already ending in `.atom`,
/// which has no sibling distinct from itself.
pub fn atom_path(path: &str) -> Option<String> {
    let (dir, last) = path.rsplit_once('/')?;
    if last.contains('{') || last.contains('}') {
        return None;
    }
    let stem = match last.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => {
            if ext == "atom" {
                return None;
            }
            stem
        }
        _ => last,
    };
    if stem.is_empty() {
        return None;
    }
    Some(format!("{dir}/{stem}.atom"))
}

/// The argument a feed path takes, if any.
///
/// A feed path is absolute and may carry at most one argument, `{name}`, as a
/// whole segment that is not the last: the last segment names the document
/// and gives the Atom sibling. `Err` is a path that cannot be registered.
fn path_argument(path: &str) -> Result<Option<String>, &'static str> {
    if !path.starts_with('/') {
        return Err("a feed path must be absolute");
    }
    let segments: Vec<&str> = path.split('/').skip(1).collect();
    let mut argument = None;
    for (n, segment) in segments.iter().enumerate() {
        if !segment.contains('{') && !segment.contains('}') {
            continue;
        }
        let name = segment
            .strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))
            .filter(|name| {
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
            .ok_or("a feed path argument must be a whole segment, {name}")?;
        if n + 1 == segments.len() {
            return Err("a feed path argument cannot be its last segment");
        }
        if argument.replace(name.to_string()).is_some() {
            return Err("a feed path takes at most one argument");
        }
    }
    Ok(argument)
}

/// The `display.routes` parameter mapping that binds `segment`: the same
/// binding the gather's page route applies to it.
fn binding_for(query: &GatherQuery, segment: &str) -> Option<GatherRouteParam> {
    query
        .display
        .routes
        .iter()
        .flat_map(|route| route.params.iter())
        .find(|param| param.segment == segment)
        .cloned()
}

/// The page an argument feed is a feed of: the gather route taking just that
/// argument, when the gather has one.
fn argument_home_path(query: &GatherQuery, segment: &str) -> Option<String> {
    query
        .display
        .routes
        .iter()
        .find(|route| route.params.len() == 1 && route.params[0].segment == segment)
        .map(|route| route.path.clone())
}

/// `pattern` with `{segment}` replaced by `value`, percent-encoded.
fn fill(pattern: &str, segment: &str, value: &str) -> String {
    pattern.replace(&format!("{{{segment}}}"), &urlencoding::encode(value))
}

/// The title of a feed narrowed by an argument.
fn argument_title(title: &str, value: &str) -> String {
    format!("{title}: {value}")
}

/// Build a router serving every gather query that declares a feed.
///
/// Called once at startup after gather queries are loaded. Entries are skipped
/// with a warning when they are unusable rather than absent: a path that is not
/// absolute or takes more than one argument, an argument the gather's
/// `display.routes` does not bind, or a path that collides with a feed already
/// registered — axum panics on a route conflict, and a config entity must not
/// be able to take the process down by declaring a duplicate.
pub fn build_feed_router(queries: &[GatherQuery]) -> Router<AppState> {
    let mut router = Router::new();
    let mut registered: HashSet<String> = HashSet::new();

    for query in queries {
        let Some(feed) = query.display.feed.as_ref() else {
            continue;
        };

        for path in declared_paths(feed) {
            let argument = match path_argument(path) {
                Ok(argument) => argument,
                Err(reason) => {
                    tracing::warn!(
                        query_id = %query.query_id,
                        path = %path,
                        reason,
                        "skipping feed with an unusable path"
                    );
                    continue;
                }
            };
            let binding = match argument.as_deref() {
                None => None,
                Some(segment) => match binding_for(query, segment) {
                    Some(binding) => Some(binding),
                    None => {
                        tracing::warn!(
                            query_id = %query.query_id,
                            path = %path,
                            segment,
                            "skipping feed whose argument no display.routes parameter binds"
                        );
                        continue;
                    }
                },
            };
            let Some(atom) = atom_path(path) else {
                tracing::warn!(
                    query_id = %query.query_id,
                    path = %path,
                    "skipping feed whose path has no distinct .atom sibling"
                );
                continue;
            };

            // Two patterns that differ only in their argument's name are one
            // route to axum.
            let route_key = |p: &str| match argument.as_deref() {
                Some(segment) => p.replace(&format!("{{{segment}}}"), "{}"),
                None => p.to_string(),
            };
            let (path_key, atom_key) = (route_key(path), route_key(&atom));
            if registered.contains(&path_key) || registered.contains(&atom_key) {
                tracing::warn!(
                    query_id = %query.query_id,
                    path = %path,
                    "skipping duplicate feed path"
                );
                continue;
            }
            registered.insert(path_key);
            registered.insert(atom_key);

            let canonical = query
                .display
                .canonical_url
                .clone()
                .filter(|p| p.starts_with('/'));
            let home_path = argument
                .as_deref()
                .and_then(|segment| argument_home_path(query, segment))
                .or(canonical)
                .map(HomePath::Fixed)
                .unwrap_or(HomePath::GatherDefault);

            let config = Arc::new(FeedConfig {
                query_id: query.query_id.clone(),
                path: path.clone(),
                atom_path: atom.clone(),
                title: feed_title(query),
                description: feed
                    .description
                    .clone()
                    .or_else(|| query.description.clone())
                    .unwrap_or_default(),
                items: feed.items.clamp(1, MAX_FEED_ITEMS),
                full_content: feed.full_content,
                home_path,
                query_changed: query.changed,
                argument: binding,
            });

            tracing::info!(
                query_id = %query.query_id,
                path = %path,
                atom_path = %atom,
                "registering feed"
            );

            for (route_path, format) in [(path, FeedFormat::Rss), (&atom, FeedFormat::Atom)] {
                let config = Arc::clone(&config);
                router = if config.argument.is_some() {
                    router.route(
                        route_path,
                        get(
                            move |state: State<AppState>,
                                  session: Session,
                                  headers: HeaderMap,
                                  Path(segments): Path<HashMap<String, String>>| {
                                let config = Arc::clone(&config);
                                async move {
                                    serve_feed(state, session, headers, &config, format, segments)
                                        .await
                                }
                            },
                        ),
                    )
                } else {
                    router.route(
                        route_path,
                        get(
                            move |state: State<AppState>, session: Session, headers: HeaderMap| {
                                let config = Arc::clone(&config);
                                async move {
                                    serve_feed(
                                        state,
                                        session,
                                        headers,
                                        &config,
                                        format,
                                        HashMap::new(),
                                    )
                                    .await
                                }
                            },
                        ),
                    )
                };
            }
        }
    }

    router
}

/// Whether a feed path can be registered: absolute, at most one argument.
#[cfg(test)]
fn is_valid_feed_path(path: &str) -> bool {
    path_argument(path).is_ok()
}

/// The feed's title: its own, else the query's label.
fn feed_title(query: &GatherQuery) -> String {
    query
        .display
        .feed
        .as_ref()
        .and_then(|f| f.title.clone())
        .unwrap_or_else(|| query.label.clone())
}

/// Log one warning when `SITE_URL` cannot be the address of a public site.
///
/// Every absolute URL a feed (and the sitemap) emits is built from `SITE_URL`;
/// the request's `Host` header is deliberately not consulted, since it is
/// whatever the client sent. Unset, `SITE_URL` defaults to
/// `http://localhost:$PORT`, so every link in every feed points at the reader's
/// own machine. Returns whether it warned.
pub fn warn_if_site_url_unusable(site_url: &str) -> bool {
    if !is_loopback_site_url(site_url) {
        return false;
    }
    tracing::warn!(
        site_url = %site_url,
        "SITE_URL is unset or names a loopback host: feed links, feed ids and \
         the sitemap will point at it. Set SITE_URL to the site's public address."
    );
    true
}

/// Whether `site_url` is missing a usable public host: unparsable, hostless,
/// `localhost`, or a loopback or unspecified IP address.
fn is_loopback_site_url(site_url: &str) -> bool {
    let Ok(url) = url::Url::parse(site_url.trim()) else {
        return true;
    };
    match url.host() {
        None => true,
        Some(url::Host::Domain(domain)) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain == "localhost" || domain.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_unspecified(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback() || ip.is_unspecified(),
    }
}

/// Execute the query as this viewer and render the result as RSS or Atom.
///
/// `segments` are the request's path arguments, empty for a feed that takes
/// none.
async fn serve_feed(
    State(state): State<AppState>,
    session: Session,
    headers: HeaderMap,
    config: &FeedConfig,
    format: FeedFormat,
    segments: HashMap<String, String>,
) -> Response {
    let viewer = crate::routes::item::get_user_context(&session, &state).await;
    // The argument, resolved by the same code the gather's page route uses:
    // an unknown tag or author is a 404, not an empty feed, and so is a tag
    // of a vocabulary this viewer may not read.
    let (url_args, raw_value) = match &config.argument {
        None => (HashMap::new(), None),
        Some(binding) => {
            match resolve_route_params(&state, &viewer, std::slice::from_ref(binding), &segments)
                .await
            {
                Ok(resolved) => (resolved, segments.get(&binding.segment).cloned()),
                Err(RouteParamError::NotFound) => {
                    return (StatusCode::NOT_FOUND, "Feed not found").into_response();
                }
                Err(RouteParamError::Failed) => {
                    return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to build feed")
                        .into_response();
                }
            }
        }
    };
    let filled = |pattern: &str| match (&config.argument, &raw_value) {
        (Some(binding), Some(value)) => fill(pattern, &binding.segment, value),
        _ => pattern.to_string(),
    };

    // A signed-in viewer's feed is that viewer's execution of the query, so no
    // shared cache may keep it; an anonymous one is the same for every
    // anonymous reader.
    let cache_control = if viewer.authenticated {
        "private, no-cache".to_string()
    } else {
        format!("public, max-age={ANONYMOUS_MAX_AGE_SECS}")
    };

    let params = ExecuteParams::new(1, LIVE_STAGE_ID.to_string(), url_args);
    let result = match crate::routes::gather::execute_query_only(
        &state,
        &config.query_id,
        params,
        None,
        viewer,
        // The feed's own size, not the page's: `items: 20` on a gather that
        // shows ten a page used to carry ten.
        Some(config.items),
    )
    .await
    {
        Ok(result) => result,
        // An aggregator has no login page to be sent to: a feed of a gather the
        // viewer may not run is simply refused.
        Err(e) if e.downcast_ref::<crate::gather::GatherDenied>().is_some() => {
            return (StatusCode::FORBIDDEN, "Access denied").into_response();
        }
        Err(e) => {
            tracing::error!(
                query_id = %config.query_id,
                error = %e,
                "failed to execute gather query for feed"
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, "Failed to build feed").into_response();
        }
    };

    // A passed-through argument is not looked up, so the only sign that it
    // names nothing is that nothing matched it. That is a 404 too: an
    // aggregator subscribed to a typo should be told, not fed an empty feed.
    if config
        .argument
        .as_ref()
        .is_some_and(|b| b.tag_category.is_none() && !b.user)
        && result.items.is_empty()
    {
        return (StatusCode::NOT_FOUND, "Feed not found").into_response();
    }

    let site_url = crate::routes::sitemap::canonical_base(&state.runtime().site_url).to_string();
    let rows: Vec<&serde_json::Value> = result.items.iter().take(config.items as usize).collect();
    let item_ids: Vec<Uuid> = rows.iter().filter_map(|row| row_uuid(row)).collect();
    let meta = load_entry_meta(&state, &item_ids).await;
    // The labels the document prints, in one query for the whole response:
    // they go into the validators (see "Polling" above), so they are read
    // before a `304`, not after.
    let term_ids: Vec<Uuid> = {
        let mut seen = HashSet::new();
        rows.iter()
            .flat_map(|row| row_term_ids(row))
            .filter(|id| seen.insert(*id))
            .collect()
    };
    let terms = load_term_labels(&state, &term_ids).await;

    let rss_path = filled(&config.path);
    // Resolved before the validators, not after: the channel link is part of
    // the document, so an alias write that changes it must change the ETag
    // and, through `home_path_changed`, `Last-Modified`.
    let (home_path, home_path_changed) = resolve_home_path(&state, config, &filled).await;
    let validators = Validators::compute(
        config,
        format,
        &site_url,
        &rss_path,
        &home_path,
        home_path_changed,
        &rows,
        &meta,
        &terms,
    );
    let cache_headers = validators.headers(&cache_control);
    if validators.matches(&headers) {
        return (StatusCode::NOT_MODIFIED, cache_headers).into_response();
    }

    let entries: Vec<Entry> = rows
        .iter()
        .map(|row| Entry::from_row(row, &site_url, &meta, &terms, config.full_content))
        .collect();

    let title = match &raw_value {
        Some(value) => argument_title(&config.title, value),
        None => config.title.clone(),
    };
    let channel = Channel {
        title: &title,
        description: &config.description,
        home_url: format!("{site_url}{home_path}"),
        rss_url: format!("{site_url}{rss_path}"),
        atom_url: format!("{site_url}{}", filled(&config.atom_path)),
        updated: validators.last_modified.unwrap_or(config.query_changed),
    };

    let body = match format {
        FeedFormat::Rss => render_rss(&channel, &entries),
        FeedFormat::Atom => render_atom(&channel, &entries),
    };

    let mut response = (StatusCode::OK, cache_headers, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.content_type()),
    );
    response
}

/// The feed's channel link: `config.home_path`, filled with the request's
/// argument when it has a literal pattern, else the outbound URL alias of
/// `/gather/{query_id}` (cached — see
/// [`crate::gather::GatherService::outbound_alias_for_gather`]), else that
/// path itself.
///
/// Also the alias row's own `changed`, when the link came from one: a
/// `Fixed` link is part of the gather's own config (an argument route or
/// `canonical_url`), so editing it already moves `config.query_changed`, and
/// no separate time is needed.
async fn resolve_home_path(
    state: &AppState,
    config: &FeedConfig,
    filled: &impl Fn(&str) -> String,
) -> (String, Option<i64>) {
    match &config.home_path {
        HomePath::Fixed(pattern) => (filled(pattern), None),
        HomePath::GatherDefault => match state
            .gather()
            .outbound_alias_for_gather(&config.query_id)
            .await
        {
            Some((alias, changed)) => (alias, Some(changed)),
            None => (format!("/gather/{}", config.query_id), None),
        },
    }
}

/// What the feed needs from the `item` table for each entry: what the row may
/// not carry (a query with an explicit field list need not select `changed`),
/// and the author's name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct EntryMeta {
    changed: i64,
    author: Option<String>,
}

/// Load `changed` and the author's name for every entry, in one query.
///
/// Only ids the viewer's own execution of the query returned are asked about,
/// and only columns the item page shows anyone who can see the item.
async fn load_entry_meta(state: &AppState, ids: &[Uuid]) -> HashMap<Uuid, EntryMeta> {
    if ids.is_empty() {
        return HashMap::new();
    }
    let rows: Result<Vec<(Uuid, i64, Option<String>)>, _> = sqlx::query_as(
        "SELECT i.id, i.changed, u.name FROM item i \
         LEFT JOIN users u ON u.id = i.author_id \
         WHERE i.id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(state.db())
    .await;
    match rows {
        Ok(rows) => rows
            .into_iter()
            .map(|(id, changed, name)| {
                (
                    id,
                    EntryMeta {
                        changed,
                        author: name.and_then(|n| author_name(&n)),
                    },
                )
            })
            .collect(),
        Err(e) => {
            // A feed without authors is worth more than no feed.
            tracing::warn!(error = %e, "failed to load authors for feed");
            HashMap::new()
        }
    }
}

/// The name to publish for an author: the account name, unless it is empty or
/// an email address, which a feed must never carry.
fn author_name(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || name.contains('@') {
        None
    } else {
        Some(name.to_string())
    }
}

/// Load the labels of every taxonomy term the entries reference, in one query.
async fn load_term_labels(state: &AppState, ids: &[Uuid]) -> HashMap<Uuid, String> {
    if ids.is_empty() {
        return HashMap::new();
    }
    let rows: Result<Vec<(Uuid, String)>, _> =
        sqlx::query_as("SELECT id, label FROM category_tag WHERE id = ANY($1)")
            .bind(ids)
            .fetch_all(state.db())
            .await;
    match rows {
        Ok(rows) => rows.into_iter().collect(),
        Err(e) => {
            tracing::warn!(error = %e, "failed to load terms for feed");
            HashMap::new()
        }
    }
}

/// The row's item id, whatever JSON shape the query's field config produced.
fn row_id(row: &serde_json::Value) -> Option<String> {
    match row.get("id")? {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn row_uuid(row: &serde_json::Value) -> Option<Uuid> {
    row_id(row).and_then(|id| Uuid::parse_str(&id).ok())
}

/// A field as the row carries it: nested under `fields` for a query that
/// selects the whole item, at the top level for one with a field list.
fn row_field<'a>(row: &'a serde_json::Value, name: &str) -> Option<&'a serde_json::Value> {
    row.get("fields")
        .and_then(|fields| fields.get(name))
        .or_else(|| row.get(name))
}

/// A text field's value under either shape (`{"value": …}` or a bare string).
fn row_text(row: &serde_json::Value, name: &str) -> Option<String> {
    let fields = row.get("fields");
    fields
        .and_then(|f| field_text(f, name))
        .or_else(|| field_text(row, name))
}

/// Every uuid a row's field values hold, as candidates for taxonomy terms.
///
/// Term references are stored as uuid strings, one or a list, under whatever
/// name the content type gave the field. Every candidate goes to
/// [`load_term_labels`], which keeps only the ones that are terms, so a
/// reference to another item is not mistaken for one. Only the fields the
/// gather returned to this viewer are read, so a field the field-access seam
/// withheld cannot leak its terms here.
fn row_term_ids(row: &serde_json::Value) -> Vec<Uuid> {
    let mut ids = Vec::new();
    let mut push = |value: &serde_json::Value| {
        let mut one = |v: &serde_json::Value| {
            let s = v
                .as_str()
                .or_else(|| v.get("target_id").and_then(|t| t.as_str()));
            if let Some(id) = s.and_then(|s| Uuid::parse_str(s).ok())
                && !ids.contains(&id)
            {
                ids.push(id);
            }
        };
        match value {
            serde_json::Value::Array(values) => values.iter().for_each(&mut one),
            other => one(other),
        }
    };
    if let Some(fields) = row.get("fields").and_then(|f| f.as_object()) {
        fields.values().for_each(&mut push);
    }
    if let Some(object) = row.as_object() {
        object
            .iter()
            .filter(|(key, _)| key.starts_with("field_"))
            .for_each(|(_, value)| push(value));
    }
    ids
}

/// One feed entry, format-neutral.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    /// `urn:uuid:<item id>`: never changes for the life of the item.
    id: String,
    title: String,
    /// Absolute URL of the item's page.
    link: String,
    /// Plain text.
    summary: String,
    /// HTML, rendered through the body's text format, with `full_content`.
    content_html: Option<String>,
    published: Option<i64>,
    updated: Option<i64>,
    author: Option<String>,
    categories: Vec<String>,
}

impl Entry {
    fn from_row(
        row: &serde_json::Value,
        site_url: &str,
        meta: &HashMap<Uuid, EntryMeta>,
        terms: &HashMap<Uuid, String>,
        full_content: bool,
    ) -> Self {
        let title = row
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled")
            .to_string();

        let raw_id = row_id(row);
        let uuid = raw_id.as_deref().and_then(|id| Uuid::parse_str(id).ok());

        // `url` is the item's alias, resolved by the gather service for the
        // whole result set, or `/item/{id}`.
        let path = row
            .get("url")
            .and_then(|v| v.as_str())
            .filter(|p| p.starts_with('/'))
            .map(str::to_string)
            .or_else(|| raw_id.as_ref().map(|id| format!("/item/{id}")));
        let link = path
            .map(|p| format!("{site_url}{p}"))
            .unwrap_or_else(|| site_url.to_string());

        let id = match uuid {
            Some(uuid) => format!("urn:uuid:{uuid}"),
            None => link.clone(),
        };

        let item_meta = uuid.and_then(|u| meta.get(&u));
        let created = row.get("created").and_then(|v| v.as_i64());
        let updated = item_meta
            .map(|m| m.changed)
            .or_else(|| row.get("changed").and_then(|v| v.as_i64()))
            .or(created);

        let categories = row_term_ids(row)
            .iter()
            .filter_map(|id| terms.get(id).cloned())
            .collect();

        let content_html = full_content
            .then(|| row_body(row))
            .flatten()
            .map(|(text, format)| {
                FilterPipeline::for_format(format.as_deref().unwrap_or("plain_text")).process(&text)
            });

        Entry {
            id,
            title,
            link,
            summary: row_summary(row).unwrap_or_default(),
            content_html,
            published: created,
            updated,
            author: item_meta.and_then(|m| m.author.clone()),
            categories,
        }
    }
}

/// The entry's summary, as plain text: the first summary field the item has,
/// whole, else its body cut at a word boundary.
fn row_summary(row: &serde_json::Value) -> Option<String> {
    if let Some(summary) = SUMMARY_FIELDS
        .iter()
        .filter_map(|name| row_text(row, name))
        .map(|raw| plain_text(&raw))
        .find(|text| !text.is_empty())
    {
        return Some(summary);
    }
    let (body, _) = row_body(row)?;
    let text = plain_text(&body);
    if text.is_empty() {
        return None;
    }
    Some(truncate_on_word(&text, SUMMARY_MAX_LEN))
}

/// The long text body and its text format, under either of its names (BL-21).
fn row_body(row: &serde_json::Value) -> Option<(String, Option<String>)> {
    BODY_FIELD_NAMES.iter().find_map(|name| {
        let text = row_text(row, name)?;
        let format = row_field(row, name)
            .and_then(|v| v.get("format"))
            .and_then(|f| f.as_str())
            .map(str::to_string);
        Some((text, format))
    })
}

/// The validators for one feed response.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Validators {
    etag: String,
    /// The latest of: the newest entry's `changed` (or `created`), the
    /// feed's own `config.query_changed` (so a title, description or other
    /// config edit moves it, not only the `ETag`), and `home_path_changed`
    /// (so does an edit to the `url_alias` row the channel link resolves
    /// through, when it resolves through one). Always `Some`:
    /// `config.query_changed` is always a real timestamp, so a feed with no
    /// entries still has a `Last-Modified`.
    last_modified: Option<i64>,
}

impl Validators {
    /// Hash everything the document is a function of that can change between
    /// polls without a restart: each entry's id, `changed`, link and author
    /// name, in order, the terms it carries, the site URL, and the resolved
    /// channel link — a `HomePath::GatherDefault` feed's is a `url_alias` row
    /// the query's own `changed` knows nothing about, so it must be hashed as
    /// given, already resolved, rather than recomputed from `config`. The
    /// feed's config is in too, with its query's `changed`, so editing the
    /// feed declaration invalidates old copies.
    ///
    /// `home_path_changed` is `home_path`'s own change time when it came from
    /// a `url_alias` row (`None` for a `Fixed` link, whose edits already move
    /// `config.query_changed`); it folds into `last_modified` only, since the
    /// resolved `home_path` string is already in the `ETag` hash.
    ///
    /// `terms` is every printed term id to its label, loaded once by the
    /// caller: a relabelled term changes the hash of exactly the feeds that
    /// print it, and no write anywhere else on the site does.
    #[allow(clippy::too_many_arguments)] // each is a distinct input the hash covers
    fn compute(
        config: &FeedConfig,
        format: FeedFormat,
        site_url: &str,
        request_path: &str,
        home_path: &str,
        home_path_changed: Option<i64>,
        rows: &[&serde_json::Value],
        meta: &HashMap<Uuid, EntryMeta>,
        terms: &HashMap<Uuid, String>,
    ) -> Self {
        let mut hasher = Sha256::new();
        let mut line = |s: &str| {
            hasher.update(s.as_bytes());
            hasher.update(b"\n");
        };
        line(format.name());
        line(site_url);
        line(&config.query_id);
        line(&config.path);
        // The argument's value: two authors' feeds are two documents.
        line(request_path);
        line(home_path);
        line(&config.title);
        line(&config.description);
        line(&config.items.to_string());
        line(&config.full_content.to_string());
        line(&config.query_changed.to_string());

        // The channel's own floor: always a real timestamp, so a feed with no
        // entries still gets a `Last-Modified`.
        let mut newest = config.query_changed;
        if let Some(home_path_changed) = home_path_changed {
            newest = newest.max(home_path_changed);
        }
        for row in rows {
            let uuid = row_uuid(row);
            let entry_meta = uuid.and_then(|u| meta.get(&u));
            let changed = entry_meta
                .map(|m| m.changed)
                .or_else(|| row.get("changed").and_then(|v| v.as_i64()))
                .or_else(|| row.get("created").and_then(|v| v.as_i64()));
            if let Some(changed) = changed {
                newest = newest.max(changed);
            }
            line(&row_id(row).unwrap_or_default());
            line(&changed.unwrap_or_default().to_string());
            line(row.get("url").and_then(|v| v.as_str()).unwrap_or_default());
            // The author's account name, not an id: a feed never prints the
            // id, so an id-only change (impossible) must not matter and a
            // name-only change (an account rename) must.
            line(entry_meta.and_then(|m| m.author.as_deref()).unwrap_or(""));
            // Each term the entry prints, by id and by its label: a tag added
            // or removed moves the ids, a relabel moves the label, and neither
            // needs to touch the item.s `changed`.
            let printed: Vec<String> = row_term_ids(row)
                .iter()
                .map(|id| format!("{id}={}", terms.get(id).map(String::as_str).unwrap_or("")))
                .collect();
            line(&printed.join(","));
        }
        let digest = hasher.finalize();
        let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();

        Validators {
            etag: format!("\"{hex}\""),
            last_modified: Some(newest),
        }
    }

    /// `ETag`, `Last-Modified`, `Cache-Control` and `Vary`, for a `200` and a
    /// `304` alike.
    fn headers(&self, cache_control: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(&self.etag) {
            headers.insert(header::ETAG, v);
        }
        if let Some(v) = self
            .last_modified
            .and_then(http_date)
            .and_then(|d| HeaderValue::from_str(&d).ok())
        {
            headers.insert(header::LAST_MODIFIED, v);
        }
        if let Ok(v) = HeaderValue::from_str(cache_control) {
            headers.insert(header::CACHE_CONTROL, v);
        }
        headers.insert(header::VARY, HeaderValue::from_static("Cookie"));
        headers
    }

    /// Whether the request's conditional headers still match: `If-None-Match`
    /// when it is sent, `If-Modified-Since` only when it is not (RFC 9110
    /// §13.2.2).
    fn matches(&self, request: &HeaderMap) -> bool {
        if let Some(inm) = request.get(header::IF_NONE_MATCH) {
            let Ok(inm) = inm.to_str() else {
                return false;
            };
            return inm
                .split(',')
                .map(str::trim)
                .any(|tag| tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == self.etag);
        }
        let (Some(ims), Some(newest)) =
            (request.get(header::IF_MODIFIED_SINCE), self.last_modified)
        else {
            return false;
        };
        ims.to_str()
            .ok()
            .and_then(|s| chrono::DateTime::parse_from_rfc2822(s).ok())
            .is_some_and(|since| newest <= since.timestamp())
    }
}

/// An HTTP-date (`Sun, 06 Nov 1994 08:49:37 GMT`).
fn http_date(timestamp: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|dt| dt.format("%a, %d %b %Y %H:%M:%S GMT").to_string())
}

/// RFC 822, which is what RSS 2.0 `<pubDate>` wants.
fn rfc822(timestamp: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(timestamp, 0).map(|dt| dt.to_rfc2822())
}

/// RFC 3339, which is what Atom's dates want.
fn rfc3339(timestamp: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// What both envelopes say about the feed itself.
struct Channel<'a> {
    title: &'a str,
    description: &'a str,
    /// The page the feed is a feed of.
    home_url: String,
    rss_url: String,
    atom_url: String,
    /// Newest entry, or the query's own `changed` for an empty feed.
    updated: i64,
}

/// Render an RSS 2.0 document.
fn render_rss(channel: &Channel<'_>, entries: &[Entry]) -> String {
    let mut feed = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <rss version=\"2.0\" xmlns:atom=\"http://www.w3.org/2005/Atom\" \
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:content=\"http://purl.org/rss/1.0/modules/content/\">\n\
         \x20 <channel>\n",
    );

    // Infallible: writing to a String.
    let _ = write!(
        feed,
        "    <title>{}</title>\n\
         \x20   <link>{}</link>\n\
         \x20   <description>{}</description>\n\
         \x20   <atom:link href=\"{}\" rel=\"self\" type=\"application/rss+xml\"/>\n",
        xml_escape(channel.title),
        xml_escape(&channel.home_url),
        xml_escape(channel.description),
        xml_escape(&channel.rss_url),
    );
    if let Some(date) = rfc822(channel.updated) {
        let _ = writeln!(feed, "    <lastBuildDate>{date}</lastBuildDate>");
    }

    for entry in entries {
        feed.push_str(&render_rss_item(entry));
    }

    feed.push_str("  </channel>\n</rss>\n");
    feed
}

/// Render one RSS `<item>`.
fn render_rss_item(entry: &Entry) -> String {
    let mut item = String::from("    <item>\n");
    // Infallible: writing to a String.
    let _ = write!(
        item,
        "      <title>{}</title>\n\
         \x20     <link>{}</link>\n\
         \x20     <guid isPermaLink=\"false\">{}</guid>\n",
        xml_escape(&entry.title),
        xml_escape(&entry.link),
        xml_escape(&entry.id),
    );
    if let Some(author) = &entry.author {
        let _ = writeln!(
            item,
            "      <dc:creator>{}</dc:creator>",
            xml_escape(author)
        );
    }
    for category in &entry.categories {
        let _ = writeln!(item, "      <category>{}</category>", xml_escape(category));
    }
    if !entry.summary.is_empty() {
        let _ = writeln!(
            item,
            "      <description>{}</description>",
            xml_escape(&entry.summary)
        );
    }
    if let Some(html) = &entry.content_html {
        let _ = writeln!(
            item,
            "      <content:encoded><![CDATA[{}]]></content:encoded>",
            cdata_escape(html)
        );
    }
    if let Some(date) = entry.published.and_then(rfc822) {
        let _ = writeln!(item, "      <pubDate>{date}</pubDate>");
    }
    item.push_str("    </item>\n");
    item
}

/// Render an Atom 1.0 document.
fn render_atom(channel: &Channel<'_>, entries: &[Entry]) -> String {
    let mut feed = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <feed xmlns=\"http://www.w3.org/2005/Atom\">\n",
    );
    // Infallible: writing to a String.
    let _ = write!(
        feed,
        "  <title type=\"text\">{}</title>\n\
         \x20 <id>{}</id>\n\
         \x20 <link rel=\"self\" type=\"application/atom+xml\" href=\"{}\"/>\n\
         \x20 <link rel=\"alternate\" type=\"text/html\" href=\"{}\"/>\n\
         \x20 <updated>{}</updated>\n",
        xml_escape(channel.title),
        xml_escape(&channel.atom_url),
        xml_escape(&channel.atom_url),
        xml_escape(&channel.home_url),
        rfc3339(channel.updated).unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string()),
    );
    if !channel.description.is_empty() {
        let _ = writeln!(
            feed,
            "  <subtitle type=\"text\">{}</subtitle>",
            xml_escape(channel.description)
        );
    }
    // Atom requires an author on every entry or on the feed; the feed's own
    // title stands in for an entry whose author has no publishable name.
    if entries.iter().any(|e| e.author.is_none()) {
        let _ = writeln!(
            feed,
            "  <author><name>{}</name></author>",
            xml_escape(channel.title)
        );
    }

    for entry in entries {
        feed.push_str(&render_atom_entry(entry, channel.updated));
    }
    feed.push_str("</feed>\n");
    feed
}

/// Render one Atom `<entry>`.
fn render_atom_entry(entry: &Entry, fallback_updated: i64) -> String {
    let mut out = String::from("  <entry>\n");
    let updated = entry
        .updated
        .or(entry.published)
        .unwrap_or(fallback_updated);
    // Infallible: writing to a String.
    let _ = write!(
        out,
        "    <title type=\"text\">{}</title>\n\
         \x20   <link rel=\"alternate\" type=\"text/html\" href=\"{}\"/>\n\
         \x20   <id>{}</id>\n\
         \x20   <updated>{}</updated>\n",
        xml_escape(&entry.title),
        xml_escape(&entry.link),
        xml_escape(&entry.id),
        rfc3339(updated).unwrap_or_default(),
    );
    if let Some(date) = entry.published.and_then(rfc3339) {
        let _ = writeln!(out, "    <published>{date}</published>");
    }
    if let Some(author) = &entry.author {
        let _ = writeln!(
            out,
            "    <author><name>{}</name></author>",
            xml_escape(author)
        );
    }
    for category in &entry.categories {
        let _ = writeln!(out, "    <category term=\"{}\"/>", xml_escape(category));
    }
    if !entry.summary.is_empty() {
        let _ = writeln!(
            out,
            "    <summary type=\"text\">{}</summary>",
            xml_escape(&entry.summary)
        );
    }
    if let Some(html) = &entry.content_html {
        let _ = writeln!(
            out,
            "    <content type=\"html\">{}</content>",
            xml_escape(html)
        );
    }
    out.push_str("  </entry>\n");
    out
}

/// Make a string safe inside a CDATA section.
///
/// A CDATA section ends at the first `]]>`, so content containing that sequence
/// would otherwise break out of it and inject markup into the document. The
/// sequence is split across two sections, which is the standard encoding and
/// preserves the text exactly.
fn cdata_escape(input: &str) -> String {
    input.replace("]]>", "]]]]><![CDATA[>")
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::gather::types::{GatherFeed, QueryDefinition, QueryDisplay};

    const ID: &str = "01234567-89ab-7def-8123-456789abcdef";
    const TAG_A: &str = "0193a5a0-0000-7000-8000-00000000000a";
    const TAG_B: &str = "0193a5a0-0000-7000-8000-00000000000b";

    fn query(query_id: &str, label: &str, feed: Option<GatherFeed>) -> GatherQuery {
        GatherQuery {
            query_id: query_id.to_string(),
            label: label.to_string(),
            description: Some("A description".to_string()),
            definition: QueryDefinition::default(),
            display: QueryDisplay {
                feed,
                ..QueryDisplay::default()
            },
            plugin: "core".to_string(),
            created: 0,
            changed: 0,
        }
    }

    fn feed(path: &str) -> GatherFeed {
        GatherFeed {
            path: path.to_string(),
            title: None,
            description: None,
            paths: Vec::new(),
            items: 20,
            full_content: false,
        }
    }

    fn config() -> FeedConfig {
        FeedConfig {
            query_id: "q".to_string(),
            path: "/rss/q.xml".to_string(),
            atom_path: "/rss/q.atom".to_string(),
            title: "Q".to_string(),
            description: "D".to_string(),
            items: 20,
            full_content: false,
            home_path: HomePath::Fixed("/".to_string()),
            query_changed: 0,
            argument: None,
        }
    }

    fn meta(author: Option<&str>, changed: i64) -> HashMap<Uuid, EntryMeta> {
        HashMap::from([(
            Uuid::parse_str(ID).unwrap(),
            EntryMeta {
                changed,
                author: author.map(str::to_string),
            },
        )])
    }

    fn terms() -> HashMap<Uuid, String> {
        HashMap::from([
            (Uuid::parse_str(TAG_A).unwrap(), "Rust".to_string()),
            (
                Uuid::parse_str(TAG_B).unwrap(),
                "Feeds & Readers".to_string(),
            ),
        ])
    }

    fn entry(row: &serde_json::Value) -> Entry {
        Entry::from_row(
            row,
            "https://example.com",
            &meta(Some("jeremy"), 1_700_000_500),
            &terms(),
            false,
        )
    }

    fn blog_row() -> serde_json::Value {
        serde_json::json!({
            "id": ID,
            "title": "A Post",
            "url": "/blog/a-post",
            "created": 1_700_000_000,
            "fields": {
                "field_body": {"value": "<p>Body text.</p>", "format": "filtered_html"},
                "field_tags": [TAG_A, TAG_B],
            },
        })
    }

    fn channel() -> Channel<'static> {
        Channel {
            title: "My Feed",
            description: "Desc",
            home_url: "https://example.com/blog".to_string(),
            rss_url: "https://example.com/rss.xml".to_string(),
            atom_url: "https://example.com/rss.atom".to_string(),
            updated: 1_700_000_500,
        }
    }

    #[test]
    fn only_queries_declaring_a_feed_get_one() {
        let queries = vec![
            query("with", "With Feed", Some(feed("/rss/with.xml"))),
            query("without", "No Feed", None),
        ];

        assert_eq!(
            feed_links(&queries),
            vec![FeedLink {
                path: "/rss/with.xml".to_string(),
                atom_path: "/rss/with.atom".to_string(),
                title: "With Feed".to_string(),
            }]
        );
    }

    #[test]
    fn the_atom_path_is_the_rss_paths_sibling() {
        assert_eq!(
            atom_path("/rss/blog.xml").as_deref(),
            Some("/rss/blog.atom")
        );
        assert_eq!(atom_path("/blog/feed").as_deref(), Some("/blog/feed.atom"));
        assert_eq!(atom_path("/feed.rss").as_deref(), Some("/feed.atom"));
        assert_eq!(atom_path("/a.b/feed").as_deref(), Some("/a.b/feed.atom"));
        assert_eq!(atom_path("/rss/blog.atom"), None, "no distinct sibling");
        assert_eq!(atom_path("/"), None);
    }

    #[test]
    fn a_feed_title_falls_back_to_the_query_label() {
        let mut with_title = feed("/rss/a.xml");
        with_title.title = Some("Custom Title".to_string());

        assert_eq!(
            feed_title(&query("a", "Label", Some(with_title))),
            "Custom Title"
        );
        assert_eq!(
            feed_title(&query("b", "Label", Some(feed("/rss/b.xml")))),
            "Label"
        );
    }

    /// A feed path is config, so a bad one must be skipped rather than
    /// registered — a relative path or a route pattern would panic axum or
    /// capture segments nothing can fill.
    #[test]
    fn an_unusable_feed_path_is_refused() {
        for path in ["rss/relative.xml", "", "/rss/{slug}.xml"] {
            assert!(!is_valid_feed_path(path), "{path} must be refused");
        }
        assert!(is_valid_feed_path("/rss/blog.xml"));
    }

    /// Two queries claiming one path would panic axum at startup; so would one
    /// query's RSS path landing on another's Atom sibling.
    #[test]
    fn a_duplicate_feed_path_is_registered_once() {
        let queries = vec![
            query("first", "First", Some(feed("/rss/same.xml"))),
            query("second", "Second", Some(feed("/rss/same.xml"))),
            query("third", "Third", Some(feed("/rss/same.rss"))),
        ];

        assert_eq!(feed_links(&queries).len(), 2);
        // Building the router must not panic either.
        let _router = build_feed_router(&queries);
    }

    #[test]
    fn autodiscovery_links_are_ordered_by_path() {
        let queries = vec![
            query("b", "B", Some(feed("/rss/b.xml"))),
            query("a", "A", Some(feed("/rss/a.xml"))),
        ];

        let paths: Vec<String> = feed_links(&queries).into_iter().map(|f| f.path).collect();

        assert_eq!(paths, vec!["/rss/a.xml", "/rss/b.xml"]);
    }

    #[test]
    fn a_loopback_or_missing_site_url_is_warned_about() {
        for url in [
            "http://localhost:3000",
            "http://127.0.0.1:3000",
            "http://[::1]:3000",
            "http://0.0.0.0",
            "http://app.localhost",
            "not a url",
            "",
        ] {
            assert!(warn_if_site_url_unusable(url), "{url} must warn");
        }
        for url in ["https://example.com", "http://192.168.1.20:3000"] {
            assert!(!warn_if_site_url_unusable(url), "{url} must not warn");
        }
    }

    /// B144: a site reached by IP address still builds absolute feed links —
    /// `SITE_URL` feeds `Entry::from_row` as a plain string, with no dependency
    /// on WebAuthn's effective-domain rule.
    #[test]
    fn a_feed_entry_link_is_absolute_under_an_ip_site_url() {
        let entry = Entry::from_row(
            &blog_row(),
            "http://10.20.250.7",
            &meta(Some("jeremy"), 1_700_000_500),
            &terms(),
            false,
        );
        assert_eq!(entry.link, "http://10.20.250.7/blog/a-post");
    }

    /// The defect: an entry carried title, link, description and date only.
    #[test]
    fn an_entry_has_a_creator_categories_and_a_uuid_id() {
        let entry = entry(&blog_row());

        assert_eq!(entry.id, format!("urn:uuid:{ID}"));
        assert_eq!(entry.link, "https://example.com/blog/a-post");
        assert_eq!(entry.author.as_deref(), Some("jeremy"));
        assert_eq!(entry.categories, vec!["Rust", "Feeds & Readers"]);
        assert_eq!(entry.updated, Some(1_700_000_500));

        let rss = render_rss_item(&entry);
        assert!(rss.contains("<dc:creator>jeremy</dc:creator>"), "{rss}");
        assert!(rss.contains("<category>Rust</category>"), "{rss}");
        assert!(
            rss.contains("<category>Feeds &amp; Readers</category>"),
            "{rss}"
        );
        assert!(
            rss.contains(&format!("<guid isPermaLink=\"false\">urn:uuid:{ID}</guid>")),
            "{rss}"
        );

        let atom = render_atom_entry(&entry, 0);
        assert!(
            atom.contains("<author><name>jeremy</name></author>"),
            "{atom}"
        );
        assert!(atom.contains("<category term=\"Rust\"/>"), "{atom}");
        assert!(atom.contains(&format!("<id>urn:uuid:{ID}</id>")), "{atom}");
    }

    /// The defect: the guid was the link, so a new alias re-announced the post.
    #[test]
    fn an_alias_change_leaves_the_id_unchanged() {
        let before = entry(&blog_row());
        let mut moved = blog_row();
        moved["url"] = serde_json::json!("/blog/2026/a-post-renamed");
        let after = entry(&moved);

        assert_ne!(before.link, after.link);
        assert_eq!(before.id, after.id);
    }

    #[test]
    fn an_email_is_never_an_author_name() {
        assert_eq!(author_name("someone@example.com"), None);
        assert_eq!(author_name("  "), None);
        assert_eq!(author_name("jeremy").as_deref(), Some("jeremy"));
    }

    #[test]
    fn a_reference_that_is_not_a_term_is_not_a_category() {
        let mut row = blog_row();
        row["fields"]["field_related"] = serde_json::json!(Uuid::now_v7().to_string());
        assert_eq!(entry(&row).categories, vec!["Rust", "Feeds & Readers"]);
    }

    #[test]
    fn the_summary_field_is_the_summary_when_set() {
        let mut row = blog_row();
        row["fields"]["field_summary"] =
            serde_json::json!({"value": "<em>Short</em> &amp; sweet."});
        assert_eq!(entry(&row).summary, "Short & sweet.");

        let top_level = serde_json::json!({"id": ID, "title": "T", "summary": "Given."});
        assert_eq!(entry(&top_level).summary, "Given.");
    }

    /// The defect: with no summary field the whole body went out, markup and all.
    #[test]
    fn without_a_summary_the_body_is_stripped_and_trimmed_on_a_word() {
        let long = format!("<p>{}</p>", "word ".repeat(200));
        let mut row = blog_row();
        row["fields"]["field_body"] = serde_json::json!({"value": long});

        let summary = entry(&row).summary;
        assert!(summary.len() <= SUMMARY_MAX_LEN, "{}", summary.len());
        assert!(summary.ends_with("word..."), "{summary}");
        assert!(!summary.contains('<'), "{summary}");

        let mut page = blog_row();
        page["fields"] = serde_json::json!({"body": {"value": "<p>Under the other name.</p>"}});
        assert_eq!(entry(&page).summary, "Under the other name.");
    }

    #[test]
    fn full_content_carries_the_body_through_its_text_format() {
        let mut row = blog_row();
        row["fields"]["field_body"] = serde_json::json!({
            "value": "<p>Hi <script>alert(1)</script>]]> there</p>",
            "format": "filtered_html",
        });
        let without = entry(&row);
        assert_eq!(without.content_html, None);

        let with = Entry::from_row(&row, "https://example.com", &HashMap::new(), &terms(), true);
        let html = with.content_html.clone().unwrap();
        assert!(html.contains("<p>"), "{html}");
        assert!(!html.contains("<script>"), "{html}");

        let rss = render_rss_item(&with);
        assert!(rss.contains("<content:encoded><![CDATA["), "{rss}");
        assert!(
            !rss.contains("]]> there"),
            "the terminator must be split: {rss}"
        );
        let atom = render_atom_entry(&with, 0);
        assert!(atom.contains("<content type=\"html\">&lt;p&gt;"), "{atom}");
    }

    #[test]
    fn xml_special_characters_in_a_title_are_escaped_once() {
        let row = serde_json::json!({"id": "x", "title": "Salt & <Pepper>"});
        let rss = render_rss_item(&entry(&row));
        assert!(
            rss.contains("<title>Salt &amp; &lt;Pepper&gt;</title>"),
            "{rss}"
        );
    }

    #[test]
    fn a_row_without_an_alias_links_its_item_path() {
        let row = serde_json::json!({"id": ID, "title": "T"});
        assert_eq!(entry(&row).link, format!("https://example.com/item/{ID}"));
    }

    #[test]
    fn the_envelopes_carry_self_links_and_updated() {
        let entries = vec![entry(&blog_row())];

        let rss = render_rss(&channel(), &entries);
        assert!(rss.starts_with("<?xml version=\"1.0\""));
        assert!(rss.contains(
            "<atom:link href=\"https://example.com/rss.xml\" rel=\"self\" \
             type=\"application/rss+xml\"/>"
        ));
        assert!(
            rss.contains("<link>https://example.com/blog</link>"),
            "{rss}"
        );
        assert!(rss.contains("<lastBuildDate>"), "{rss}");
        assert!(rss.ends_with("</channel>\n</rss>\n"));

        let atom = render_atom(&channel(), &entries);
        assert!(
            atom.contains(
                "<link rel=\"self\" type=\"application/atom+xml\" \
                 href=\"https://example.com/rss.atom\"/>"
            ),
            "{atom}"
        );
        assert!(
            atom.contains("<updated>2023-11-14T22:21:40Z</updated>"),
            "{atom}"
        );
        assert!(
            !atom.contains("<author><name>My Feed"),
            "every entry has an author, so the feed needs none: {atom}"
        );
    }

    /// [`Validators::compute`] with no term labels loaded, for tests not about
    /// the labels themselves.
    fn compute(
        config: &FeedConfig,
        format: FeedFormat,
        site_url: &str,
        request_path: &str,
        home_path: &str,
        home_path_changed: Option<i64>,
        rows: &[&serde_json::Value],
        meta: &HashMap<Uuid, EntryMeta>,
    ) -> Validators {
        Validators::compute(
            config,
            format,
            site_url,
            request_path,
            home_path,
            home_path_changed,
            rows,
            meta,
            &HashMap::new(),
        )
    }

    #[test]
    fn the_etag_follows_the_entries_and_the_config() {
        let row = blog_row();
        let rows = vec![&row];
        let base = compute(
            &config(),
            FeedFormat::Rss,
            "https://e",
            "/rss/q.xml",
            "/blog",
            None,
            &rows,
            &meta(None, 10),
        );

        assert_eq!(
            base,
            compute(
                &config(),
                FeedFormat::Rss,
                "https://e",
                "/rss/q.xml",
                "/blog",
                None,
                &rows,
                &meta(None, 10)
            ),
            "the same feed must give the same validators"
        );
        assert_eq!(base.last_modified, Some(10));
        assert_ne!(
            base.etag,
            compute(
                &config(),
                FeedFormat::Rss,
                "https://e",
                "/rss/q.xml",
                "/blog",
                None,
                &rows,
                &meta(None, 11)
            )
            .etag,
            "an edited entry changes the etag"
        );
        assert_ne!(
            base.etag,
            compute(
                &config(),
                FeedFormat::Atom,
                "https://e",
                "/rss/q.xml",
                "/blog",
                None,
                &rows,
                &meta(None, 10)
            )
            .etag,
            "RSS and Atom are different representations"
        );
        let mut edited = config();
        edited.full_content = true;
        assert_ne!(
            base.etag,
            compute(
                &edited,
                FeedFormat::Rss,
                "https://e",
                "/rss/q.xml",
                "/blog",
                None,
                &rows,
                &meta(None, 10)
            )
            .etag,
            "a config change changes the etag"
        );
        // The defect: an alias write that moves the channel link does not touch
        // `query.changed`, so without this the ETag would not follow it.
        assert_ne!(
            base.etag,
            compute(
                &config(),
                FeedFormat::Rss,
                "https://e",
                "/rss/q.xml",
                "/gather/q",
                None,
                &rows,
                &meta(None, 10)
            )
            .etag,
            "a changed channel link changes the etag"
        );
        // The defect this change fixes: a title/description edit (`query.changed`)
        // or an alias edit (`home_path_changed`) left `Last-Modified` where it
        // was, so a reader polling with only `If-Modified-Since` kept the old
        // channel values.
        let mut config_edited = config();
        config_edited.query_changed = 20;
        assert_eq!(
            compute(
                &config_edited,
                FeedFormat::Rss,
                "https://e",
                "/rss/q.xml",
                "/blog",
                None,
                &rows,
                &meta(None, 10),
            )
            .last_modified,
            Some(20),
            "a config edit newer than the entries moves Last-Modified"
        );
        assert_eq!(
            compute(
                &config(),
                FeedFormat::Rss,
                "https://e",
                "/rss/q.xml",
                "/blog",
                Some(30),
                &rows,
                &meta(None, 10),
            )
            .last_modified,
            Some(30),
            "an alias edit newer than the entries moves Last-Modified"
        );
    }

    /// B172: an account rename or a tag relabel leaves every entry's id,
    /// `changed` and link untouched, so the hash must carry the printed name
    /// and a stamp for the printed labels directly, not only derive them from
    /// `changed`.
    #[test]
    fn a_renamed_author_changes_the_etag_though_nothing_else_moved() {
        let row = blog_row();
        let rows = vec![&row];

        let before = compute(
            &config(),
            FeedFormat::Rss,
            "https://e",
            "/rss/q.xml",
            "/blog",
            None,
            &rows,
            &meta(Some("jeremy"), 10),
        );
        let renamed = compute(
            &config(),
            FeedFormat::Rss,
            "https://e",
            "/rss/q.xml",
            "/blog",
            None,
            &rows,
            &meta(Some("jeremy-dev"), 10),
        );
        assert_ne!(
            before.etag, renamed.etag,
            "the account name changed, so the document now reads differently"
        );
        assert_eq!(
            before.last_modified, renamed.last_modified,
            "a rename is not an edit to the item"
        );

        let no_name = compute(
            &config(),
            FeedFormat::Rss,
            "https://e",
            "/rss/q.xml",
            "/blog",
            None,
            &rows,
            &meta(None, 10),
        );
        assert_ne!(
            before.etag, no_name.etag,
            "an author gaining or losing a publishable name changes the document"
        );
    }

    /// A term's label lives on `category_tag`, not on the item, so relabelling
    /// one moves neither the entry's `changed` nor its term ids: the label
    /// itself has to be in the hash.
    #[test]
    fn a_relabelled_term_changes_the_etag_though_the_terms_are_the_same() {
        let row = blog_row();
        let rows = vec![&row];
        let tag_a: Uuid = TAG_A.parse().unwrap();
        let tag_b: Uuid = TAG_B.parse().unwrap();
        let labels = |a: &str| -> HashMap<Uuid, String> {
            [(tag_a, a.to_string()), (tag_b, "Other".to_string())]
                .into_iter()
                .collect()
        };
        let with = |terms: &HashMap<Uuid, String>| {
            Validators::compute(
                &config(),
                FeedFormat::Rss,
                "https://e",
                "/rss/q.xml",
                "/blog",
                None,
                &rows,
                &meta(None, 10),
                terms,
            )
        };

        let before = with(&labels("Before"));
        assert_eq!(
            before,
            with(&labels("Before")),
            "the same labels give the same validators"
        );
        let after = with(&labels("After"));
        assert_ne!(
            before.etag, after.etag,
            "the document prints the new label, so it is a new document"
        );
        assert_eq!(
            before.last_modified, after.last_modified,
            "relabelling a term is not an edit to the item"
        );
    }

    /// An entry's own term ids are hashed directly (free: already parsed off
    /// the row), so gaining or losing a tag changes the `ETag` even if nothing
    /// relabels anything.
    #[test]
    fn an_entrys_changed_term_ids_change_the_etag() {
        let with_both = blog_row();
        let mut one_tag = blog_row();
        one_tag["fields"]["field_tags"] = serde_json::json!([TAG_A]);

        let rows_both = vec![&with_both];
        let rows_one = vec![&one_tag];
        let before = compute(
            &config(),
            FeedFormat::Rss,
            "https://e",
            "/rss/q.xml",
            "/blog",
            None,
            &rows_both,
            &meta(None, 10),
        );
        let after = compute(
            &config(),
            FeedFormat::Rss,
            "https://e",
            "/rss/q.xml",
            "/blog",
            None,
            &rows_one,
            &meta(None, 10),
        );
        assert_ne!(before.etag, after.etag);
    }

    #[test]
    fn a_matching_if_none_match_or_if_modified_since_is_not_modified() {
        let v = Validators {
            etag: "\"abc\"".to_string(),
            last_modified: Some(1_700_000_000),
        };
        let with = |name: header::HeaderName, value: &str| {
            let mut h = HeaderMap::new();
            h.insert(name, HeaderValue::from_str(value).unwrap());
            h
        };

        assert!(v.matches(&with(header::IF_NONE_MATCH, "\"abc\"")));
        assert!(v.matches(&with(header::IF_NONE_MATCH, "\"x\", W/\"abc\"")));
        assert!(!v.matches(&with(header::IF_NONE_MATCH, "\"other\"")));
        assert!(!v.matches(&HeaderMap::new()));

        let at = http_date(1_700_000_000).unwrap();
        let before = http_date(1_699_999_999).unwrap();
        assert!(v.matches(&with(header::IF_MODIFIED_SINCE, &at)));
        assert!(!v.matches(&with(header::IF_MODIFIED_SINCE, &before)));
    }
    /// A blog gather the way `trovato_blog` declares it: routes binding `user`
    /// and `tag`, one feed declaration with three paths.
    fn blog_query() -> GatherQuery {
        let mut q = query("blog", "Blog", None);
        q.display.routes = serde_json::from_value(serde_json::json!([
            {"path": "/blog/author/{user}", "params": [{"segment": "user", "param": "author", "user": true}]},
            {"path": "/blog/tag/{tag}", "params": [{"segment": "tag", "param": "tag", "tag_category": "tags"}]},
        ]))
        .unwrap();
        q.display.feed = Some(
            serde_json::from_value(serde_json::json!({
                "path": "/blog/feed.xml",
                "paths": ["/blog/author/{user}/feed.xml", "/blog/tag/{tag}/feed.xml"],
            }))
            .unwrap(),
        );
        q
    }

    #[test]
    fn a_feed_path_takes_at_most_one_whole_segment_argument() {
        assert_eq!(path_argument("/blog/feed.xml"), Ok(None));
        assert_eq!(
            path_argument("/blog/{user}/feed").unwrap().as_deref(),
            Some("user")
        );
        assert_eq!(
            path_argument("/tag/{term}/rss.xml").unwrap().as_deref(),
            Some("term")
        );
        for refused in [
            "/rss/{slug}.xml",
            "/rss/{slug}",
            "/a/{x}/{y}/feed",
            "/a/x{y}/feed",
            "/a/{}/feed",
            "/a/{x-y}/feed",
            "relative/{x}/feed",
        ] {
            assert!(path_argument(refused).is_err(), "{refused} must be refused");
        }
    }

    #[test]
    fn an_argument_feed_is_not_advertised_site_wide() {
        let links = feed_links(&[blog_query()]);
        let paths: Vec<&str> = links.iter().map(|l| l.path.as_str()).collect();
        assert_eq!(paths, vec!["/blog/feed.xml"]);
    }

    #[test]
    fn a_page_with_an_argument_advertises_the_matching_feed() {
        let q = blog_query();
        let author = HashMap::from([("user".to_string(), "jeremy".to_string())]);
        assert_eq!(
            argument_feed_links(&q, &author),
            vec![FeedLink {
                path: "/blog/author/jeremy/feed.xml".to_string(),
                atom_path: "/blog/author/jeremy/feed.atom".to_string(),
                title: "Blog: jeremy".to_string(),
            }]
        );

        let tag = HashMap::from([("tag".to_string(), "rust lang".to_string())]);
        let links = argument_feed_links(&q, &tag);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].path, "/blog/tag/rust%20lang/feed.xml");

        assert!(argument_feed_links(&q, &HashMap::new()).is_empty());
    }

    #[test]
    fn an_argument_feed_binds_through_the_gathers_route_mapping() {
        let q = blog_query();
        let binding = binding_for(&q, "user").unwrap();
        assert_eq!(binding.param, "author");
        assert!(binding.user);
        assert_eq!(
            binding_for(&q, "tag").unwrap().tag_category.as_deref(),
            Some("tags")
        );
        assert!(binding_for(&q, "nothing").is_none());
        assert_eq!(
            argument_home_path(&q, "user").as_deref(),
            Some("/blog/author/{user}")
        );
    }

    /// An argument no route binds, and two patterns differing only in the
    /// argument's name, would otherwise reach axum: one as a route nothing can
    /// fill, the other as a conflict that panics.
    #[test]
    fn an_unbound_or_conflicting_argument_feed_is_skipped_not_registered() {
        let mut q = blog_query();
        if let Some(feed) = q.display.feed.as_mut() {
            feed.paths.push("/blog/author/{who}/feed.xml".to_string());
            feed.paths.push("/blog/tag/{user}/feed.xml".to_string());
        }
        let _router = build_feed_router(&[q]);
    }
}
