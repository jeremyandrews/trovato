//! The admin sidebar: every administration screen, the permission its page
//! asks for, and the filter that shows a viewer only the links they may open.
//!
//! The sidebar used to be a hand written list in `templates/page--admin.html`
//! shown to everyone admitted to `/admin`, so a delegated role saw a column of
//! links that answered 403 (BL-117). The gate of each link here is the one its
//! GET handler checks; when a screen's permission changes, this table changes
//! with it.
//!
//! The kernel's own screens are [`LINKS`] and [`CARDS`], fixed at compile
//! time. A plugin's are [`crate::plugin::admin_nav_registry::PluginNavLink`]
//! and `PluginNavCard`, declared through `tap_admin_nav` and collected once at
//! boot into [`crate::state::AppState::plugin_admin_nav`] — before that
//! existed, a plugin's own admin page could not appear in this sidebar at all
//! (BL-162), and a site worked around the gap by hand-copying this list into
//! its own template, shown to anyone admitted to `/admin` regardless of which
//! links they could actually open. [`sidebar`] and [`dashboard`] filter both
//! sources through the same permission and plugin-enablement gate, so the two
//! cannot drift apart.

use serde::Serialize;

use crate::plugin::admin_nav_registry::{PluginNavCard, PluginNavLink};
use crate::tap::UserContext;

/// What a viewer must hold to open a screen.
#[derive(Debug, Clone, Copy)]
pub enum Gate {
    /// A permission, by name, as `require_permission` asks for it.
    Permission(&'static str),
    /// The site administrator (`users.is_admin`), as `require_admin` asks.
    Administrator,
}

impl Gate {
    fn admits(self, viewer: &UserContext) -> bool {
        match self {
            Self::Permission(permission) => viewer.can(permission),
            Self::Administrator => viewer.is_admin(),
        }
    }
}

/// One sidebar link.
#[derive(Debug, Clone, Copy)]
pub struct Link {
    /// The section heading it sits under; `None` for the dashboard.
    pub section: Option<&'static str>,
    /// The link text.
    pub label: &'static str,
    /// Where it goes.
    pub href: &'static str,
    /// What its page requires.
    pub gate: Gate,
    /// A plugin that must be enabled for the screen to exist.
    pub plugin: Option<&'static str>,
    /// Other path prefixes that belong to this screen.
    pub also: &'static [&'static str],
}

const fn link(section: &'static str, label: &'static str, href: &'static str, gate: Gate) -> Link {
    Link {
        section: Some(section),
        label,
        href,
        gate,
        plugin: None,
        also: &[],
    }
}

use Gate::{Administrator, Permission};

/// The comments plugin, by the name `AppState::enabled_plugins` holds it under.
///
/// The sidebar once tested `"comments"` and `"categories"`, names no plugin
/// has, so Comments and Categories were hidden from everyone; its own tests
/// passed the same wrong names and passed. A test below holds every plugin
/// named here to the list the route gate is registered from.
const COMMENTS: &str = "trovato_comments";
/// The categories plugin, as [`COMMENTS`].
const CATEGORIES: &str = "trovato_categories";
/// The audit log plugin, as [`COMMENTS`].
const AUDIT_LOG: &str = "trovato_audit_log";

/// The sidebar, in display order.
pub const LINKS: &[Link] = &[
    Link {
        section: None,
        label: "Dashboard",
        href: "/admin",
        gate: Permission("access administration pages"),
        plugin: None,
        also: &[],
    },
    link(
        "Content",
        "Content",
        "/admin/content",
        Permission("edit any content"),
    ),
    link(
        "Content",
        "Add content",
        "/admin/content/add",
        Permission("create content"),
    ),
    Link {
        plugin: Some(COMMENTS),
        ..link(
            "Content",
            "Comments",
            "/admin/content/comments",
            Permission("administer comments"),
        )
    },
    link(
        "Content",
        "Files",
        "/admin/content/files",
        Permission("access files"),
    ),
    link(
        "Content",
        "Media",
        "/admin/media",
        Permission("access files"),
    ),
    link(
        "Structure",
        "Content types",
        "/admin/structure/types",
        Permission("administer site"),
    ),
    Link {
        plugin: Some(CATEGORIES),
        also: &["/admin/structure/tags"],
        ..link(
            "Structure",
            "Categories",
            "/admin/structure/categories",
            Permission("administer categories"),
        )
    },
    link(
        "Structure",
        "URL aliases",
        "/admin/structure/aliases",
        Permission("administer site"),
    ),
    link(
        "Structure",
        "Gather queries",
        "/admin/gather",
        Permission("administer site"),
    ),
    link(
        "Structure",
        "Tiles",
        "/admin/structure/tiles",
        Permission("administer site"),
    ),
    link(
        "Structure",
        "Appearance",
        "/admin/appearance",
        Permission("administer themes"),
    ),
    link(
        "Structure",
        "Menus",
        "/admin/structure/menus",
        Permission("administer site"),
    ),
    link(
        "Structure",
        "Stages",
        "/admin/structure/stages",
        Permission("administer site"),
    ),
    link(
        "People",
        "Users",
        "/admin/people",
        Permission("administer users"),
    ),
    link(
        "People",
        "Roles",
        "/admin/people/roles",
        Permission("administer users"),
    ),
    link(
        "People",
        "Permissions",
        "/admin/people/permissions",
        Permission("administer users"),
    ),
    link(
        "People",
        "Access rules",
        "/admin/people/rules",
        Permission("administer access rules"),
    ),
    link(
        "System",
        "Site settings",
        "/admin/config/site",
        Permission("administer site"),
    ),
    link(
        "System",
        "Text formats",
        "/admin/config/formats",
        Permission("administer text formats"),
    ),
    link(
        "System",
        "Search settings",
        "/admin/config/search",
        Permission("administer site"),
    ),
    link(
        "System",
        "Search report",
        "/admin/reports/search",
        Permission("administer site"),
    ),
    link(
        "System",
        "Site log",
        "/admin/reports/log",
        Permission("administer site"),
    ),
    Link {
        plugin: Some(AUDIT_LOG),
        ..link(
            "System",
            "Audit log",
            "/admin/reports/audit",
            Permission("view audit log"),
        )
    },
    link(
        "System",
        "Cron",
        "/admin/reports/cron",
        Permission("administer site"),
    ),
    link("System", "Plugins", "/admin/plugins", Administrator),
    link(
        "System",
        "AI Providers",
        "/admin/system/ai-providers",
        Permission("configure ai"),
    ),
    link(
        "System",
        "AI Budgets",
        "/admin/system/ai-budgets",
        Permission("view ai usage"),
    ),
    link(
        "System",
        "AI Chat",
        "/admin/system/ai-chat",
        Permission("configure ai"),
    ),
    link(
        "System",
        "AI Assistant",
        "/admin/system/ai-assistant",
        Permission("configure ai"),
    ),
];

/// One dashboard card: an administration area and the screens it links to.
#[derive(Debug, Clone, Copy)]
pub struct Card {
    /// The card heading.
    pub title: &'static str,
    /// One line on what the area is for.
    pub description: &'static str,
    /// `(label, href)` for each screen; every `href` is a [`LINKS`] entry, and
    /// is shown under the same gate and plugin check the sidebar applies.
    pub links: &'static [(&'static str, &'static str)],
}

/// The dashboard, in display order: one card per administration area.
pub const CARDS: &[Card] = &[
    Card {
        title: "Content",
        description: "Find, edit and publish items.",
        links: &[
            ("Content", "/admin/content"),
            ("Add content", "/admin/content/add"),
            ("Files", "/admin/content/files"),
            ("Media", "/admin/media"),
        ],
    },
    Card {
        title: "Comments",
        description: "Moderate comments and set how new ones are published.",
        links: &[("Comments", "/admin/content/comments")],
    },
    Card {
        title: "People",
        description: "User accounts.",
        links: &[
            ("Users", "/admin/people"),
            ("Access rules", "/admin/people/rules"),
        ],
    },
    Card {
        title: "Roles and permissions",
        description: "What each role may do.",
        links: &[
            ("Roles", "/admin/people/roles"),
            ("Permissions", "/admin/people/permissions"),
        ],
    },
    Card {
        title: "Structure",
        description: "Content types, their fields, and stages.",
        links: &[
            ("Content types", "/admin/structure/types"),
            ("Stages", "/admin/structure/stages"),
        ],
    },
    Card {
        title: "Categories",
        description: "Vocabularies and their tags.",
        links: &[("Categories", "/admin/structure/categories")],
    },
    Card {
        title: "Appearance",
        description: "The site's theme, and the administration theme.",
        links: &[("Appearance", "/admin/appearance")],
    },
    Card {
        title: "Menus",
        description: "Navigation menus and their links.",
        links: &[("Menus", "/admin/structure/menus")],
    },
    Card {
        title: "URL aliases",
        description: "Readable addresses for items and pages.",
        links: &[("URL aliases", "/admin/structure/aliases")],
    },
    Card {
        title: "Gathers",
        description: "Listings, feeds and their filters and sorts.",
        links: &[("Gather queries", "/admin/gather")],
    },
    Card {
        title: "Tiles",
        description: "What appears in each page region.",
        links: &[("Tiles", "/admin/structure/tiles")],
    },
    Card {
        title: "Plugins",
        description: "Enable and disable plugins.",
        links: &[("Plugins", "/admin/plugins")],
    },
    Card {
        title: "Site settings",
        description: "Site name, front page, search and AI providers.",
        links: &[
            ("Site settings", "/admin/config/site"),
            ("Search settings", "/admin/config/search"),
            ("Text formats", "/admin/config/formats"),
            ("AI Providers", "/admin/system/ai-providers"),
        ],
    },
    Card {
        title: "Reports",
        description: "What the site has been doing.",
        links: &[
            ("Site log", "/admin/reports/log"),
            ("Audit log", "/admin/reports/audit"),
            ("Cron", "/admin/reports/cron"),
            ("Search report", "/admin/reports/search"),
            ("AI Budgets", "/admin/system/ai-budgets"),
        ],
    },
];

/// A card as the template draws it.
#[derive(Debug, Clone, Serialize)]
pub struct DashboardCard {
    pub title: String,
    pub description: String,
    pub links: Vec<NavLink>,
}

/// Whether `link`'s screen exists on this site and `viewer` may open it: the
/// one test both the sidebar and the dashboard apply.
fn offered(link: &Link, viewer: &UserContext, enabled_plugins: &[String]) -> bool {
    link.gate.admits(viewer)
        && link
            .plugin
            .is_none_or(|p| enabled_plugins.iter().any(|e| e == p))
}

/// [`offered`], for a plugin-declared link: the viewer holds its permission,
/// and its plugin is presently enabled. Unlike a kernel link naming a plugin,
/// a plugin link's plugin is never absent — it is the plugin that declared
/// it — so there is no "no plugin required" case to fall through.
fn offered_plugin(link: &PluginNavLink, viewer: &UserContext, enabled_plugins: &[String]) -> bool {
    viewer.can(&link.permission) && enabled_plugins.iter().any(|e| e == &link.plugin)
}

/// The dashboard cards `viewer` may use: each card's links filtered exactly as
/// the sidebar filters them, and a card with none left dropped, so the
/// dashboard never offers a screen that answers 403 or 404. Plugin-declared
/// cards follow the same rule, appended after the kernel's own.
pub fn dashboard(
    viewer: &UserContext,
    enabled_plugins: &[String],
    plugin_links: &[PluginNavLink],
    plugin_cards: &[PluginNavCard],
) -> Vec<DashboardCard> {
    let mut cards: Vec<DashboardCard> = CARDS
        .iter()
        .filter_map(|card| {
            let links: Vec<NavLink> = card
                .links
                .iter()
                .filter(|(_, href)| {
                    LINKS
                        .iter()
                        .find(|l| l.href == *href)
                        .is_some_and(|l| offered(l, viewer, enabled_plugins))
                })
                .map(|(label, href)| NavLink {
                    label: (*label).to_string(),
                    href: (*href).to_string(),
                    active: false,
                })
                .collect();
            (!links.is_empty()).then_some(DashboardCard {
                title: card.title.to_string(),
                description: card.description.to_string(),
                links,
            })
        })
        .collect();

    for card in plugin_cards {
        let visible = plugin_links
            .iter()
            .find(|l| l.plugin == card.plugin && l.href == card.href)
            .is_some_and(|l| offered_plugin(l, viewer, enabled_plugins));
        if visible {
            cards.push(DashboardCard {
                title: card.title.clone(),
                description: card.description.clone(),
                links: vec![NavLink {
                    label: card.title.clone(),
                    href: card.href.clone(),
                    active: false,
                }],
            });
        }
    }

    cards
}

/// A link as the template draws it.
#[derive(Debug, Clone, Serialize)]
pub struct NavLink {
    pub label: String,
    pub href: String,
    pub active: bool,
}

/// A section heading and its links, as the template draws it.
#[derive(Debug, Clone, Serialize)]
pub struct NavSection {
    /// `None` for the links above the first heading.
    pub title: Option<String>,
    pub links: Vec<NavLink>,
}

/// Whether `path` is `prefix` or below it.
fn under(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// One link ready to be grouped into a section and checked for activeness,
/// whichever of [`LINKS`] or a plugin's declarations it came from.
struct Entry<'a> {
    section: Option<&'a str>,
    label: &'a str,
    href: &'a str,
    /// Other path prefixes [`under`] which this entry is still active; a
    /// plugin link has none of its own.
    also: &'a [&'a str],
}

/// The links `viewer` may open, grouped by section, with the one for `path`
/// marked active: the link whose address is the longest prefix of `path`, so
/// `/admin/content/files/3` is Files rather than Content, and `/admin` itself
/// only for the dashboard. A section with no link left is dropped.
///
/// `plugin_links` joins the kernel's own [`LINKS`] before grouping, under the
/// section name it declares: an existing kernel section by that name gets a
/// new entry, and a name no kernel section uses starts one of its own, placed
/// after every kernel section in declaration order.
pub fn sidebar(
    viewer: &UserContext,
    enabled_plugins: &[String],
    path: &str,
    plugin_links: &[PluginNavLink],
) -> Vec<NavSection> {
    let mut entries: Vec<Entry> = LINKS
        .iter()
        .filter(|l| offered(l, viewer, enabled_plugins))
        .map(|l| Entry {
            section: l.section,
            label: l.label,
            href: l.href,
            also: l.also,
        })
        .collect();
    entries.extend(
        plugin_links
            .iter()
            .filter(|l| offered_plugin(l, viewer, enabled_plugins))
            .map(|l| Entry {
                section: Some(l.section.as_str()),
                label: l.label.as_str(),
                href: l.href.as_str(),
                also: &[],
            }),
    );

    let active = entries
        .iter()
        .filter_map(|e| {
            std::iter::once(e.href)
                .chain(e.also.iter().copied())
                .filter(|prefix| {
                    if *prefix == "/admin" {
                        path == "/admin"
                    } else {
                        under(path, prefix)
                    }
                })
                .map(str::len)
                .max()
                .map(|len| (len, e.href))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, href)| href);

    // Grouped by section title, in the order each title first appears, so a
    // plugin link joins its named kernel section wherever that section sits
    // rather than always trailing at the end.
    let mut sections: Vec<NavSection> = Vec::new();
    let mut index: std::collections::HashMap<Option<&str>, usize> =
        std::collections::HashMap::new();
    for e in entries {
        let nav = NavLink {
            label: e.label.to_string(),
            href: e.href.to_string(),
            active: active == Some(e.href),
        };
        match index.get(&e.section) {
            Some(&i) => sections[i].links.push(nav),
            None => {
                index.insert(e.section, sections.len());
                sections.push(NavSection {
                    title: e.section.map(str::to_string),
                    links: vec![nav],
                });
            }
        }
    }
    sections
}

/// Build this viewer's sidebar for `path` and insert it into `context` as
/// `admin_nav`, with the sorted `enabled_plugins` list beside it.
///
/// This is the one step every admin page takes to get the kernel's sidebar —
/// a kernel route (`helpers::render_admin_template`), the themed error page
/// (`middleware::error_page`), and a plugin's own admin route
/// (`routes::plugin_api::themed_response`) alike — through this helper so the
/// three cannot drift apart. Before it existed the third did not call either
/// of the other two's inline version of this block at all, so a plugin's
/// admin page rendered `page--admin.html` with no sidebar (BL-162).
pub fn insert_context(
    state: &crate::state::AppState,
    viewer: &UserContext,
    path: &str,
    context: &mut tera::Context,
) {
    let mut enabled: Vec<String> = state.enabled_plugins().into_iter().collect();
    enabled.sort();
    context.insert(
        "admin_nav",
        &sidebar(viewer, &enabled, path, state.plugin_admin_nav().links()),
    );
    context.insert("enabled_plugins", &enabled);
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn viewer(permissions: &[&str]) -> UserContext {
        UserContext::authenticated(
            Uuid::now_v7(),
            permissions.iter().map(|p| p.to_string()).collect(),
        )
    }

    fn hrefs(sections: &[NavSection]) -> Vec<String> {
        sections
            .iter()
            .flat_map(|s| s.links.iter().map(|l| l.href.clone()))
            .collect()
    }

    /// A plugin link in the "Structure" section, needing a permission no
    /// kernel link uses, from a plugin named "groups" — the example this
    /// module's docs and the plugin authoring guide both use.
    fn groups_link() -> PluginNavLink {
        PluginNavLink {
            plugin: "groups".to_string(),
            section: "Structure".to_string(),
            label: "Groups".to_string(),
            href: "/admin/groups".to_string(),
            permission: "administer groups".to_string(),
        }
    }

    fn groups_card() -> PluginNavCard {
        PluginNavCard {
            plugin: "groups".to_string(),
            title: "Groups".to_string(),
            description: "Manage user groups.".to_string(),
            href: "/admin/groups".to_string(),
        }
    }

    #[test]
    fn a_content_editor_sees_content_and_nothing_that_answers_403() {
        let nav = sidebar(
            &viewer(&["access administration pages", "edit any content"]),
            &[],
            "/admin/content",
            &[],
        );
        assert_eq!(hrefs(&nav), vec!["/admin", "/admin/content"]);
        assert!(nav[1].links[0].active);
        assert_eq!(nav[1].title, Some("Content".to_string()));
    }

    #[test]
    fn a_plugin_screen_needs_its_plugin() {
        let v = viewer(&["access administration pages", "administer comments"]);
        assert!(
            !hrefs(&sidebar(&v, &[], "/admin", &[]))
                .contains(&"/admin/content/comments".to_string())
        );
        assert!(
            hrefs(&sidebar(&v, &[COMMENTS.to_string()], "/admin", &[]))
                .contains(&"/admin/content/comments".to_string())
        );
    }

    #[test]
    fn the_longest_matching_link_is_active() {
        let admin = UserContext::administrator(Uuid::now_v7(), vec![]);
        let active = |path: &str| -> Vec<String> {
            sidebar(&admin, &[CATEGORIES.to_string()], path, &[])
                .iter()
                .flat_map(|s| s.links.iter().filter(|l| l.active).map(|l| l.href.clone()))
                .collect()
        };
        assert_eq!(active("/admin"), vec!["/admin"]);
        assert_eq!(
            active("/admin/content/files/3"),
            vec!["/admin/content/files"]
        );
        assert_eq!(active("/admin/content/42/edit"), vec!["/admin/content"]);
        assert_eq!(
            active("/admin/people/roles/add"),
            vec!["/admin/people/roles"]
        );
        assert_eq!(
            active("/admin/structure/tags/9"),
            vec!["/admin/structure/categories"]
        );
        assert!(active("/admin/somewhere-else").is_empty());
    }

    #[test]
    fn plugins_is_for_the_site_administrator_only() {
        let holder = viewer(&["administer site", "access administration pages"]);
        assert!(
            !hrefs(&sidebar(&holder, &[], "/admin", &[])).contains(&"/admin/plugins".to_string())
        );
    }
    /// Every plugin a link waits for is one the route gate knows, by the name
    /// `AppState::enabled_plugins` holds. `"comments"` and `"categories"` were
    /// not, and hid both screens from everyone.
    #[test]
    fn every_link_plugin_is_a_real_gated_plugin_name() {
        for l in LINKS {
            if let Some(p) = l.plugin {
                assert!(
                    crate::plugin::gate::GATED_ROUTE_PLUGINS
                        .iter()
                        .any(|g| g.name == p),
                    "{} waits for {p:?}, which no gated plugin is called",
                    l.href
                );
            }
        }
    }

    /// The names come from the gate's own registry, not from literals here.
    fn gated(name_suffix: &str) -> String {
        crate::plugin::gate::GATED_ROUTE_PLUGINS
            .iter()
            .find(|g| g.name.ends_with(name_suffix))
            .map(|g| g.name.to_string())
            .expect("a gated plugin with that name")
    }

    #[test]
    fn comments_and_categories_show_when_their_plugins_are_enabled() {
        let admin = UserContext::administrator(Uuid::now_v7(), vec![]);
        let enabled = vec![gated("_comments"), gated("_categories")];
        let links = hrefs(&sidebar(&admin, &enabled, "/admin", &[]));
        assert!(
            links.contains(&"/admin/content/comments".to_string()),
            "{links:?}"
        );
        assert!(
            links.contains(&"/admin/structure/categories".to_string()),
            "{links:?}"
        );
        let none = hrefs(&sidebar(&admin, &[], "/admin", &[]));
        assert!(!none.contains(&"/admin/content/comments".to_string()));
        assert!(!none.contains(&"/admin/structure/categories".to_string()));
    }

    #[test]
    fn every_card_link_is_a_sidebar_link() {
        for card in CARDS {
            for (_, href) in card.links {
                assert!(
                    LINKS.iter().any(|l| l.href == *href),
                    "card {} links to {href}, which the sidebar does not gate",
                    card.title
                );
            }
        }
    }

    fn card_hrefs(cards: &[DashboardCard]) -> Vec<String> {
        cards
            .iter()
            .flat_map(|c| c.links.iter().map(|l| l.href.clone()))
            .collect()
    }

    #[test]
    fn the_dashboard_offers_only_what_the_viewer_may_open() {
        let editor = viewer(&["access administration pages", "edit any content"]);
        let cards = dashboard(&editor, &[gated("_comments")], &[], &[]);
        assert_eq!(
            cards.iter().map(|c| c.title.clone()).collect::<Vec<_>>(),
            vec!["Content"]
        );
        assert_eq!(card_hrefs(&cards), vec!["/admin/content"]);

        let moderator = viewer(&["access administration pages", "administer comments"]);
        assert!(dashboard(&moderator, &[], &[], &[]).is_empty());
        assert_eq!(
            card_hrefs(&dashboard(&moderator, &[gated("_comments")], &[], &[])),
            vec!["/admin/content/comments"]
        );

        let admin = UserContext::administrator(Uuid::now_v7(), vec![]);
        let all = dashboard(
            &admin,
            &[gated("_comments"), gated("_categories")],
            &[],
            &[],
        );
        assert_eq!(all.len(), CARDS.len(), "the administrator sees every card");
    }

    // --- plugin-declared links and cards (BL-162) ---

    #[test]
    fn a_plugin_link_joins_its_named_section() {
        let admin = UserContext::administrator(Uuid::now_v7(), vec![]);
        let nav = sidebar(&admin, &["groups".to_string()], "/admin", &[groups_link()]);
        let structure = nav
            .iter()
            .find(|s| s.title.as_deref() == Some("Structure"))
            .expect("a Structure section");
        assert!(
            structure.links.iter().any(|l| l.href == "/admin/groups"),
            "{structure:?}"
        );
    }

    #[test]
    fn a_viewer_without_the_plugin_links_permission_does_not_see_it() {
        let holder = viewer(&["access administration pages"]);
        let nav = sidebar(&holder, &["groups".to_string()], "/admin", &[groups_link()]);
        assert!(!hrefs(&nav).contains(&"/admin/groups".to_string()));
    }

    #[test]
    fn a_plugin_link_needs_its_plugin_enabled_even_with_the_permission() {
        let holder = viewer(&["access administration pages", "administer groups"]);
        assert!(
            !hrefs(&sidebar(&holder, &[], "/admin", &[groups_link()]))
                .contains(&"/admin/groups".to_string())
        );
        assert!(
            hrefs(&sidebar(
                &holder,
                &["groups".to_string()],
                "/admin",
                &[groups_link()]
            ))
            .contains(&"/admin/groups".to_string())
        );
    }

    #[test]
    fn a_plugin_link_can_be_the_active_one() {
        let holder = viewer(&["access administration pages", "administer groups"]);
        let nav = sidebar(
            &holder,
            &["groups".to_string()],
            "/admin/groups/42",
            &[groups_link()],
        );
        let active: Vec<&str> = nav
            .iter()
            .flat_map(|s| s.links.iter().filter(|l| l.active).map(|l| l.href.as_str()))
            .collect();
        assert_eq!(active, vec!["/admin/groups"]);
    }

    #[test]
    fn kernel_links_and_cards_are_unchanged_by_an_unrelated_plugin_declaration() {
        let admin = UserContext::administrator(Uuid::now_v7(), vec![]);
        let enabled = vec![
            gated("_comments"),
            gated("_categories"),
            "groups".to_string(),
        ];
        let with_plugin = sidebar(&admin, &enabled, "/admin", &[groups_link()]);
        let without_plugin = sidebar(&admin, &enabled, "/admin", &[]);
        let kernel_hrefs = |nav: &[NavSection]| -> Vec<String> {
            hrefs(nav)
                .into_iter()
                .filter(|h| h != "/admin/groups")
                .collect()
        };
        assert_eq!(kernel_hrefs(&with_plugin), kernel_hrefs(&without_plugin));

        let cards_with = dashboard(&admin, &enabled, &[groups_link()], &[groups_card()]);
        let cards_without = dashboard(&admin, &enabled, &[], &[]);
        assert_eq!(cards_with.len(), cards_without.len() + 1);
    }

    #[test]
    fn a_plugin_card_needs_its_permission_and_plugin_like_its_link() {
        let holder = viewer(&["access administration pages"]);
        assert!(
            dashboard(
                &holder,
                &["groups".to_string()],
                &[groups_link()],
                &[groups_card()]
            )
            .is_empty()
        );

        let granted = viewer(&["access administration pages", "administer groups"]);
        let cards = dashboard(
            &granted,
            &["groups".to_string()],
            &[groups_link()],
            &[groups_card()],
        );
        assert_eq!(card_hrefs(&cards), vec!["/admin/groups"]);
        assert_eq!(cards[0].title, "Groups");
        assert_eq!(cards[0].description, "Manage user groups.");

        assert!(
            dashboard(&granted, &[], &[groups_link()], &[groups_card()]).is_empty(),
            "disabling the plugin drops the card too"
        );
    }
}
