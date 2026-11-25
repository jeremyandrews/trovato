//! Navigation as templates get it: config menus loaded once per stage and
//! language and cached, then filtered per viewer and built into trees, beside
//! the plugin entries the menu registry holds.
//!
//! # The cache
//!
//! Every `menu_link` row of a stage is read in one query, every path's alias
//! source in one more, and, for a language other than the site default, every
//! translated target title in a third. The result is held in one `moka` cache
//! with a TTL, keyed by stage and language, so a warm render costs no menu,
//! alias or translation query at all. What varies per viewer (which links they
//! may follow, and which one is active) is decided in memory on every render.
//!
//! The cache is process-wide rather than held by a service, because the writes
//! that change its answer are made in model functions that hold only a pool
//! ([`MenuLink`] and
//! [`UrlAlias`](crate::models::UrlAlias) create, update and delete), and in
//! config import, stage publishing and item translation. Each of them calls
//! [`invalidate_config_menus`] after it writes. A write made from outside the
//! process (`trovato config import` run on its own) is seen when the TTL runs
//! out, which is the same contract the permission cache has.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use moka::sync::Cache;
use serde::Serialize;
use uuid::Uuid;

use super::MenuDefinition;
use crate::metrics::CacheName;
use crate::models::MenuLink;
use crate::state::AppState;
use crate::tap::UserContext;

/// How long a stage's menus are served from the cache without being reread.
///
/// The bound on staleness for a write the process cannot see (an import run as
/// its own process); every in-process write invalidates at once.
const CONFIG_MENUS_TTL: Duration = Duration::from_secs(300);

/// Every config menu of one stage in one language, keyed `(stage, language)`.
static CONFIG_MENUS: LazyLock<Cache<(Uuid, String), Arc<ConfigMenus>>> = LazyLock::new(|| {
    Cache::builder()
        .max_capacity(64)
        .time_to_live(CONFIG_MENUS_TTL)
        .build()
});

/// Drop every cached config menu.
///
/// Called after every write that changes what a menu renders: a menu link, a
/// URL alias or an item translation created, updated or deleted, a config
/// import and a stage publish.
pub fn invalidate_config_menus() {
    clear_local_config_menus();
    crate::cache::generation::bump(crate::cache::generation::CacheKind::Menus);
}

/// Drop this server's cached config menus only: what another server's bump
/// does here.
pub fn clear_local_config_menus() {
    CONFIG_MENUS.invalidate_all();
}

/// One link of a navigation tree, as a template draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MenuTreeLink {
    /// The link text.
    pub title: String,
    /// Where it goes.
    pub path: String,
    /// Whether the page being rendered is this link's page or one under it
    /// (see [`is_active`]).
    pub active: bool,
    /// The links under this one, weight ordered.
    pub children: Vec<MenuTreeLink>,
}

/// One `menu_link` row as the cache holds it.
#[derive(Debug, Clone)]
struct CachedLink {
    /// The row, its path and title already localized for the entry's language.
    link: MenuLink,
    /// The row's path as stored, without query or fragment: what the viewer
    /// filter checks.
    follow_path: String,
}

/// Every config menu of one stage, in one language.
#[derive(Debug, Default)]
pub struct ConfigMenus {
    /// Every link of every menu, ordered by menu name, weight and title.
    links: Vec<CachedLink>,
    /// What each link path that is an alias stands for.
    sources: HashMap<String, String>,
}

impl ConfigMenus {
    /// Whether the stage has a menu called `name` at all, whoever is looking.
    pub fn has_menu(&self, name: &str) -> bool {
        self.links.iter().any(|c| c.link.menu_name == name)
    }

    /// Every link of every menu that `viewer` may follow, localized.
    ///
    /// A link is dropped when [`viewer_may_follow`] refuses its path or the
    /// page its alias stands for, and every link under a dropped one goes with
    /// it. Administrators keep every link.
    pub fn links_viewer_may_follow(&self, state: &AppState, viewer: &UserContext) -> Vec<MenuLink> {
        let refused: HashSet<Uuid> = if viewer.is_admin() {
            HashSet::new()
        } else {
            self.links
                .iter()
                .filter(|cached| {
                    let path = cached.follow_path.as_str();
                    let target = self.sources.get(path).map_or(path, String::as_str);
                    !viewer_may_follow(state, viewer, path)
                        || !viewer_may_follow(state, viewer, target)
                })
                .map(|cached| cached.link.id)
                .collect()
        };
        let links: Vec<MenuLink> = self.links.iter().map(|c| c.link.clone()).collect();
        without_descendants_of(links, refused)
    }
}

/// The config menus of `stage` in `language`, from the cache or loaded into it.
///
/// A load that fails is served as no menus and not cached, so the next render
/// tries again.
pub async fn config_menus(state: &AppState, stage: Uuid, language: &str) -> Arc<ConfigMenus> {
    let key = (stage, language.to_string());
    if let Some(cached) = state
        .metrics()
        .cache_lookup(CacheName::Menu, CONFIG_MENUS.get(&key))
    {
        return cached;
    }
    match load_config_menus(state, stage, language).await {
        Ok(menus) => {
            let menus = Arc::new(menus);
            CONFIG_MENUS.insert(key, Arc::clone(&menus));
            menus
        }
        Err(e) => {
            tracing::warn!(error = %e, %stage, %language, "failed to load config menus");
            Arc::new(ConfigMenus::default())
        }
    }
}

async fn load_config_menus(
    state: &AppState,
    stage: Uuid,
    language: &str,
) -> anyhow::Result<ConfigMenus> {
    let links = MenuLink::find_by_stage(state.db(), stage).await?;

    // One alias lookup for both uses: the viewer filter asks what a path
    // without its query stands for, localization what the path as written does.
    let mut paths: Vec<String> = links
        .iter()
        .flat_map(|link| [strip_query(&link.path).to_string(), link.path.clone()])
        .collect();
    paths.sort();
    paths.dedup();
    let sources = crate::models::UrlAlias::sources_for_aliases(
        state.db(),
        &paths,
        &[crate::models::stage::LIVE_STAGE_ID],
        state.default_language(),
    )
    .await?;

    let follow_paths: Vec<String> = links
        .iter()
        .map(|link| strip_query(&link.path).to_string())
        .collect();
    let links = localize_menu_links(state, links, &sources, language).await?;
    Ok(ConfigMenus {
        links: links
            .into_iter()
            .zip(follow_paths)
            .map(|(link, follow_path)| CachedLink { link, follow_path })
            .collect(),
        sources,
    })
}

/// Point a menu at the pages it has in `language`, and leave the rest alone.
///
/// A site serving `/it/why` rendered its navigation with default-language
/// addresses, so every click out of the menu left the translation. This rewrites
/// a link when, and only when, the page behind it exists in `language`. The one
/// rule for that:
///
/// - a link to an item (its source path, or an alias of one) exists in
///   `language` when the item has a translation into it;
/// - any other link exists in every language when it is a local path served
///   under a language prefix ([`path_in_language`]): a gather page, a term, the
///   front page, a plugin route. An external URL, or a system path language
///   negotiation passes through (`/static`, `/api`), does not.
///
/// A link that exists in `language` gains the `/{language}` prefix, so the click
/// stays in the language. An item link's title is also replaced by the
/// translated item's title **when the link's own title is the target's
/// default-language title**, which is the common case of a menu label mirroring a
/// page title. A label somebody wrote by hand is theirs, and is left as they
/// wrote it.
///
/// An item link whose target has no translation is untouched, address and label
/// both. A translated label on an untranslated page is a promise the click
/// breaks.
///
/// One query for every menu of the stage, made when the cache is filled: `sources`
/// is the alias resolution the loader already holds.
async fn localize_menu_links(
    state: &AppState,
    links: Vec<MenuLink>,
    sources: &HashMap<String, String>,
    language: &str,
) -> anyhow::Result<Vec<MenuLink>> {
    if language == state.default_language() || links.is_empty() {
        return Ok(links);
    }

    // What each link points at. A path is either an item source path already or
    // an alias standing in for one; anything else is not content and cannot be
    // translated.
    let targets: Vec<Option<Uuid>> = links
        .iter()
        .map(|link| {
            item_id_from_source(&link.path)
                .or_else(|| sources.get(&link.path).and_then(|s| item_id_from_source(s)))
        })
        .collect();

    let ids: Vec<Uuid> = targets.iter().flatten().copied().collect();
    let translations = state.items().translated_titles_for(&ids, language).await?;

    Ok(links
        .into_iter()
        .zip(targets)
        .map(|(mut link, target)| {
            let Some(id) = target else {
                // Not an item: a gather page, a term, the front page, a plugin
                // route. Those are served under every language's prefix.
                if let Some(path) = path_in_language(
                    &link.path,
                    language,
                    state.known_languages(),
                    state.default_language(),
                ) {
                    link.path = path;
                }
                return link;
            };
            let Some((translated_title, default_title)) = translations.get(&id) else {
                return link;
            };

            link.path = format!("/{language}{}", link.path);
            if link.title == *default_title && !translated_title.is_empty() {
                link.title = translated_title.clone();
            }
            link
        })
        .collect())
}

/// `links` without `dropped` and without every link under one, however deep.
fn without_descendants_of(links: Vec<MenuLink>, mut dropped: HashSet<Uuid>) -> Vec<MenuLink> {
    loop {
        let before = dropped.len();
        for link in &links {
            if link
                .parent_id
                .is_some_and(|parent| dropped.contains(&parent))
            {
                dropped.insert(link.id);
            }
        }
        if dropped.len() == before {
            break;
        }
    }
    links
        .into_iter()
        .filter(|link| !dropped.contains(&link.id))
        .collect()
}

/// Whether `viewer` could follow a link to `path`, as far as the kernel can tell
/// without making the request.
///
/// Refused when the path serves a gather the viewer may not run (the same
/// [`authorize_gather`](crate::gather::authorize_gather) check the gather's own
/// routes make), or a plugin route whose registered permission they lack.
/// Anything else is assumed reachable: this hides links, it does not gate pages.
pub fn viewer_may_follow(state: &AppState, viewer: &UserContext, path: &str) -> bool {
    if let Some(query) = state.gather().query_for_path(path)
        && crate::gather::authorize_gather(viewer, &query).is_err()
    {
        return false;
    }
    match state.menu_registry().match_path(path) {
        Some(route) => route.menu.permission.is_empty() || viewer.can(&route.menu.permission),
        None => true,
    }
}

/// A link path without its query string or fragment.
fn strip_query(path: &str) -> &str {
    path.split(['?', '#']).next().unwrap_or(path)
}

/// `path` in `language`: the same address behind `/{language}`, or `None` when
/// that would not be an address the site serves.
///
/// `None` for anything that is not a local path (an external URL, a
/// protocol-relative one, a bare fragment), for a path language negotiation
/// does not serve per language
/// ([`is_served_per_language`](crate::middleware::language::is_served_per_language)),
/// and for a path already in a language. `/` becomes `/{language}`, the address
/// the front page route reads a language back from.
fn path_in_language(
    path: &str,
    language: &str,
    known_languages: &[String],
    default_language: &str,
) -> Option<String> {
    if !path.starts_with('/') || path.starts_with("//") {
        return None;
    }
    let bare = strip_query(path);
    if !crate::middleware::language::is_served_per_language(bare)
        || without_language(bare, known_languages, default_language) != bare
    {
        return None;
    }
    let rest = if bare == "/" { &path[1..] } else { path };
    Some(format!("/{language}{rest}"))
}

/// `path` without a leading `/{language}` for a known non-default language: the
/// form the router serves it in, and the form active links are compared in.
fn without_language(path: &str, known_languages: &[String], default_language: &str) -> String {
    crate::middleware::path_alias::strip_language_prefix(path, known_languages, default_language).0
}

/// The item a source path names, or `None` if it names something else.
fn item_id_from_source(path: &str) -> Option<Uuid> {
    path.strip_prefix("/item/")
        .and_then(|id| Uuid::parse_str(id).ok())
}

/// Whether a link to `link_path` is active on the page at `requested`.
///
/// The one active rule: the page is the link's page, or one under it (the
/// requested path starts with the link path followed by `/`), so `/devices`
/// is active on `/devices/new`. `/` is active only on `/`, or it would be
/// active everywhere. A link's query string and fragment are not part of the
/// comparison.
pub fn is_active(link_path: &str, requested: &str) -> bool {
    let link_path = strip_query(link_path);
    if link_path == requested {
        return true;
    }
    let base = link_path.trim_end_matches('/');
    !base.is_empty()
        && requested
            .strip_prefix(base)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The page a tree is built for, as its links are matched against it.
trait Requested {
    /// Whether a link to `link_path` is active on this page.
    fn is_active(&self, link_path: &str) -> bool;
}

/// A path as given, compared as it stands.
impl Requested for str {
    fn is_active(&self, link_path: &str) -> bool {
        is_active(link_path, self)
    }
}

/// The page being rendered, with the languages its address may carry.
///
/// A link and the address asked for are compared without their language
/// prefix, the form the router serves both in. Otherwise a link either side of
/// the prefix never matches: `/plain` (an untranslated item, left unprefixed)
/// on `/it/plain`, or `/it/recipes` on `/recipes` read in Italian by session.
struct RequestedPage<'a> {
    /// The address asked for, without its language prefix.
    path: String,
    known_languages: &'a [String],
    default_language: &'a str,
}

impl<'a> RequestedPage<'a> {
    fn new(requested: &str, known_languages: &'a [String], default_language: &'a str) -> Self {
        Self {
            path: without_language(requested, known_languages, default_language),
            known_languages,
            default_language,
        }
    }
}

impl Requested for RequestedPage<'_> {
    fn is_active(&self, link_path: &str) -> bool {
        let link_path = without_language(
            strip_query(link_path),
            self.known_languages,
            self.default_language,
        );
        is_active(&link_path, &self.path)
    }
}

/// One node handed to [`build_tree`]: a key, the key of its parent, and what
/// the template draws.
struct TreeNode {
    key: String,
    parent: Option<String>,
    title: String,
    path: String,
}

/// The one tree builder: the links under `root` (`None` for the top level),
/// each with the links under it, in the order `nodes` is in.
///
/// Depth is bounded, which keeps a parent cycle written straight into the
/// table from recursing forever.
fn build_tree<R: Requested + ?Sized>(
    nodes: &[TreeNode],
    root: Option<&str>,
    requested: &R,
) -> Vec<MenuTreeLink> {
    fn level<R: Requested + ?Sized>(
        nodes: &[TreeNode],
        parent: Option<&str>,
        requested: &R,
        depth: usize,
    ) -> Vec<MenuTreeLink> {
        const MAX_DEPTH: usize = 16;
        if depth > MAX_DEPTH {
            return Vec::new();
        }
        nodes
            .iter()
            .filter(|node| node.parent.as_deref() == parent)
            .map(|node| MenuTreeLink {
                title: node.title.clone(),
                path: node.path.clone(),
                active: requested.is_active(&node.path),
                children: level(nodes, Some(&node.key), requested, depth + 1),
            })
            .collect()
    }
    level(nodes, root, requested, 0)
}

/// Config links as trees, one per menu name, leaving out hidden links and
/// everything under them.
///
/// A link whose parent is not in the same menu is drawn at the top level, so
/// no link is lost to a parent it cannot be placed under. Every menu the stage
/// has is present, empty when the viewer may follow none of its links.
fn config_trees(
    links: &[MenuLink],
    requested: &RequestedPage<'_>,
) -> BTreeMap<String, Vec<MenuTreeLink>> {
    let hidden: HashSet<Uuid> = links.iter().filter(|l| l.hidden).map(|l| l.id).collect();
    let shown = without_descendants_of(links.to_vec(), hidden);
    let mut by_menu: BTreeMap<String, Vec<TreeNode>> = BTreeMap::new();
    for link in links {
        by_menu.entry(link.menu_name.clone()).or_default();
    }
    for link in &shown {
        let parent = link
            .parent_id
            .filter(|p| {
                shown
                    .iter()
                    .any(|other| other.id == *p && other.menu_name == link.menu_name)
            })
            .map(|p| p.to_string());
        by_menu
            .entry(link.menu_name.clone())
            .or_default()
            .push(TreeNode {
                key: link.id.to_string(),
                parent,
                title: link.title.clone(),
                path: link.path.clone(),
            });
    }
    by_menu
        .into_iter()
        .map(|(name, nodes)| (name, build_tree(&nodes, None, requested)))
        .collect()
}

/// Plugin entries this viewer may see as trees: the root entries, and the
/// entries under every parent path, keyed by that path.
///
/// An entry's own children hang under it, so `/user`'s list carries the
/// entries under each of its entries too.
fn plugin_trees(
    state: &AppState,
    viewer: &UserContext,
    requested: &RequestedPage<'_>,
) -> (Vec<MenuTreeLink>, BTreeMap<String, Vec<MenuTreeLink>>) {
    let registry = state.menu_registry();
    let parents: Vec<String> = registry.parent_paths().map(str::to_string).collect();
    let node = |parent: Option<&str>, (registered, entry): (String, MenuDefinition)| TreeNode {
        key: registered,
        parent: parent.map(str::to_string),
        title: entry.title,
        path: entry.path,
    };
    let mut nodes: Vec<TreeNode> = registry
        .root_entries_for(viewer)
        .into_iter()
        .map(|entry| node(None, entry))
        .collect();
    for parent in &parents {
        nodes.extend(
            registry
                .children_for(parent, viewer)
                .into_iter()
                .map(|entry| node(Some(parent), entry)),
        );
    }
    let roots = build_tree(&nodes, None, requested);
    let by_parent = parents
        .into_iter()
        .map(|parent| {
            let tree = build_tree(&nodes, Some(&parent), requested);
            (parent, tree)
        })
        .collect();
    (roots, by_parent)
}

/// Everything about navigation that a page context carries, built once per
/// render from the cached config menus and the menu registry.
#[derive(Debug, Clone, Serialize)]
pub struct SiteMenus {
    /// The `main` config menu, flat, filtered and localized: the shape themes
    /// have always had.
    pub main_menu: Vec<MenuLink>,
    /// The `footer` config menu, the same way.
    pub footer_menu: Vec<MenuLink>,
    /// Whether the site has a `main` config menu at all, whoever is looking.
    pub main_menu_owned: bool,
    /// Plugin root entries this viewer may see.
    pub menus: Vec<MenuDefinition>,
    /// The same entries as a tree, each with the entries under it and its
    /// active flag.
    pub menus_tree: Vec<MenuTreeLink>,
    /// Every config menu as a tree, by menu name.
    pub menus_by_name: BTreeMap<String, Vec<MenuTreeLink>>,
    /// Plugin entries this viewer may see as trees, by parent path.
    pub menus_by_parent: BTreeMap<String, Vec<MenuTreeLink>>,
}

/// The navigation for one render: `viewer` reading the Live stage in
/// `language`, at `requested` (the address as asked for, which is what a link
/// is matched against, without its language prefix).
///
/// No query on a warm cache: the config menus come from [`config_menus`], and
/// everything per viewer is decided in memory.
pub async fn site_menus(
    state: &AppState,
    viewer: &UserContext,
    language: &str,
    requested: &str,
) -> SiteMenus {
    let config = config_menus(state, crate::models::stage::LIVE_STAGE_ID, language).await;
    let followable = config.links_viewer_may_follow(state, viewer);
    let menu = |name: &str| -> Vec<MenuLink> {
        followable
            .iter()
            .filter(|link| link.menu_name == name)
            .cloned()
            .collect()
    };
    let requested =
        RequestedPage::new(requested, state.known_languages(), state.default_language());
    let (menus_tree, menus_by_parent) = plugin_trees(state, viewer, &requested);
    SiteMenus {
        main_menu: menu("main"),
        footer_menu: menu("footer"),
        main_menu_owned: config.has_menu("main"),
        menus: state.menu_registry().root_menus_for(viewer),
        menus_tree,
        menus_by_name: config_trees(&followable, &requested),
        menus_by_parent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_is_active_on_its_page_and_every_page_under_it() {
        assert!(is_active("/devices", "/devices"));
        assert!(is_active("/devices", "/devices/new"));
        assert!(is_active("/devices/", "/devices/new"));
        assert!(is_active("/devices?sort=name", "/devices/new"));
        assert!(!is_active("/devices", "/devices-old"));
        assert!(!is_active("/devices/new", "/devices"));
    }

    #[test]
    fn home_is_active_only_on_home() {
        assert!(is_active("/", "/"));
        assert!(!is_active("/", "/devices"));
    }

    fn node(key: &str, parent: Option<&str>) -> TreeNode {
        TreeNode {
            key: key.to_string(),
            parent: parent.map(str::to_string),
            title: key.to_string(),
            path: format!("/{key}"),
        }
    }

    #[test]
    fn a_tree_nests_children_under_their_parent_in_order() {
        let nodes = [
            node("a", None),
            node("a1", Some("a")),
            node("b", None),
            node("a2", Some("a")),
        ];
        let tree = build_tree(&nodes, None, "/a2");
        let titles: Vec<&str> = tree.iter().map(|l| l.title.as_str()).collect();
        assert_eq!(titles, ["a", "b"]);
        let children: Vec<&str> = tree[0].children.iter().map(|l| l.title.as_str()).collect();
        assert_eq!(children, ["a1", "a2"]);
        assert!(tree[0].children[1].active);
        assert!(!tree[1].active);
    }

    #[test]
    fn a_parent_cycle_does_not_recurse_forever() {
        let nodes = [node("a", Some("b")), node("b", Some("a"))];
        assert!(build_tree(&nodes, Some("a"), "/").len() == 1);
    }

    fn languages() -> Vec<String> {
        vec!["en".to_string(), "es".to_string()]
    }

    fn in_spanish(path: &str) -> Option<String> {
        path_in_language(path, "es", &languages(), "en")
    }

    #[test]
    fn a_local_page_gains_the_language_prefix() {
        assert_eq!(in_spanish("/recipes").as_deref(), Some("/es/recipes"));
        assert_eq!(
            in_spanish("/recipes?sort=new#top").as_deref(),
            Some("/es/recipes?sort=new#top")
        );
        assert_eq!(in_spanish("/term/x").as_deref(), Some("/es/term/x"));
        assert_eq!(in_spanish("/").as_deref(), Some("/es"));
        assert_eq!(in_spanish("/?a=b").as_deref(), Some("/es?a=b"));
    }

    #[test]
    fn what_is_not_served_per_language_keeps_its_address() {
        for path in [
            "https://example.com/recipes",
            "//cdn.example.com/x",
            "mailto:a@example.com",
            "#top",
            "/static/brochure.pdf",
            "/api/v1/items",
            "/health",
            "/install",
            "/es/recipes",
            "/es",
        ] {
            assert_eq!(in_spanish(path), None, "{path}");
        }
    }

    #[test]
    fn active_compares_link_and_page_without_their_language() {
        let languages = languages();
        let here = RequestedPage::new("/es/recipes", &languages, "en");
        assert!(here.is_active("/es/recipes"));
        assert!(here.is_active("/recipes"));
        assert!(!here.is_active("/es/about"));

        let under = RequestedPage::new("/es/recipes/paella", &languages, "en");
        assert!(under.is_active("/es/recipes"));

        let home = RequestedPage::new("/es", &languages, "en");
        assert!(home.is_active("/es"));
        assert!(!home.is_active("/es/recipes"));
        assert!(RequestedPage::new("/es/", &languages, "en").is_active("/es"));

        let english = RequestedPage::new("/recipes", &languages, "en");
        assert!(english.is_active("/recipes"));
        assert!(!english.is_active("/about"));
        assert!(!RequestedPage::new("/", &languages, "en").is_active("/recipes"));
    }
}
