//! The admin sidebar: every administration screen, the permission its page
//! asks for, and the filter that shows a viewer only the links they may open.
//!
//! The sidebar used to be a hand written list in `templates/page--admin.html`
//! shown to everyone admitted to `/admin`, so a delegated role saw a column of
//! links that answered 403 (BL-117). The gate of each link here is the one its
//! GET handler checks; when a screen's permission changes, this table changes
//! with it.

use serde::Serialize;

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
        "Mail log",
        "/admin/reports/mail",
        Permission("administer site"),
    ),
    link(
        "System",
        "Blog mail",
        "/admin/mail",
        Permission("view own blog mail"),
    ),
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
            ("Mail log", "/admin/reports/mail"),
            ("Blog mail", "/admin/mail"),
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
    pub title: &'static str,
    pub description: &'static str,
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

/// The dashboard cards `viewer` may use: each card's links filtered exactly as
/// the sidebar filters them, and a card with none left dropped, so the
/// dashboard never offers a screen that answers 403 or 404.
pub fn dashboard(viewer: &UserContext, enabled_plugins: &[String]) -> Vec<DashboardCard> {
    CARDS
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
                    label,
                    href,
                    active: false,
                })
                .collect();
            (!links.is_empty()).then_some(DashboardCard {
                title: card.title,
                description: card.description,
                links,
            })
        })
        .collect()
}

/// A link as the template draws it.
#[derive(Debug, Clone, Serialize)]
pub struct NavLink {
    pub label: &'static str,
    pub href: &'static str,
    pub active: bool,
}

/// A section heading and its links, as the template draws it.
#[derive(Debug, Clone, Serialize)]
pub struct NavSection {
    /// `None` for the links above the first heading.
    pub title: Option<&'static str>,
    pub links: Vec<NavLink>,
}

/// Whether `path` is `prefix` or below it.
fn under(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The links `viewer` may open, grouped by section, with the one for `path`
/// marked active: the link whose address is the longest prefix of `path`, so
/// `/admin/content/files/3` is Files rather than Content, and `/admin` itself
/// only for the dashboard. A section with no link left is dropped.
pub fn sidebar(viewer: &UserContext, enabled_plugins: &[String], path: &str) -> Vec<NavSection> {
    let visible: Vec<&Link> = LINKS
        .iter()
        .filter(|l| offered(l, viewer, enabled_plugins))
        .collect();

    let active = visible
        .iter()
        .filter_map(|l| {
            std::iter::once(l.href)
                .chain(l.also.iter().copied())
                .filter(|prefix| {
                    if *prefix == "/admin" {
                        path == "/admin"
                    } else {
                        under(path, prefix)
                    }
                })
                .map(str::len)
                .max()
                .map(|len| (len, l.href))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, href)| href);

    let mut sections: Vec<NavSection> = Vec::new();
    for l in visible {
        let nav = NavLink {
            label: l.label,
            href: l.href,
            active: active == Some(l.href),
        };
        match sections.last_mut() {
            Some(section) if section.title == l.section => section.links.push(nav),
            _ => sections.push(NavSection {
                title: l.section,
                links: vec![nav],
            }),
        }
    }
    sections
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

    fn hrefs(sections: &[NavSection]) -> Vec<&'static str> {
        sections
            .iter()
            .flat_map(|s| s.links.iter().map(|l| l.href))
            .collect()
    }

    #[test]
    fn a_content_editor_sees_content_and_nothing_that_answers_403() {
        let nav = sidebar(
            &viewer(&["access administration pages", "edit any content"]),
            &[],
            "/admin/content",
        );
        assert_eq!(hrefs(&nav), vec!["/admin", "/admin/content"]);
        assert!(nav[1].links[0].active);
        assert_eq!(nav[1].title, Some("Content"));
    }

    #[test]
    fn a_plugin_screen_needs_its_plugin() {
        let v = viewer(&["access administration pages", "administer comments"]);
        assert!(!hrefs(&sidebar(&v, &[], "/admin")).contains(&"/admin/content/comments"));
        assert!(
            hrefs(&sidebar(&v, &[COMMENTS.to_string()], "/admin"))
                .contains(&"/admin/content/comments")
        );
    }

    #[test]
    fn the_longest_matching_link_is_active() {
        let admin = UserContext::administrator(Uuid::now_v7(), vec![]);
        let active = |path: &str| -> Vec<&'static str> {
            sidebar(&admin, &[CATEGORIES.to_string()], path)
                .iter()
                .flat_map(|s| s.links.iter().filter(|l| l.active).map(|l| l.href))
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
        assert!(!hrefs(&sidebar(&holder, &[], "/admin")).contains(&"/admin/plugins"));
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
        let links = hrefs(&sidebar(&admin, &enabled, "/admin"));
        assert!(links.contains(&"/admin/content/comments"), "{links:?}");
        assert!(links.contains(&"/admin/structure/categories"), "{links:?}");
        let none = hrefs(&sidebar(&admin, &[], "/admin"));
        assert!(!none.contains(&"/admin/content/comments"));
        assert!(!none.contains(&"/admin/structure/categories"));
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

    fn card_hrefs(cards: &[DashboardCard]) -> Vec<&'static str> {
        cards
            .iter()
            .flat_map(|c| c.links.iter().map(|l| l.href))
            .collect()
    }

    #[test]
    fn the_dashboard_offers_only_what_the_viewer_may_open() {
        let editor = viewer(&["access administration pages", "edit any content"]);
        let cards = dashboard(&editor, &[gated("_comments")]);
        assert_eq!(
            cards.iter().map(|c| c.title).collect::<Vec<_>>(),
            vec!["Content"]
        );
        assert_eq!(card_hrefs(&cards), vec!["/admin/content"]);

        let moderator = viewer(&["access administration pages", "administer comments"]);
        assert!(dashboard(&moderator, &[]).is_empty());
        assert_eq!(
            card_hrefs(&dashboard(&moderator, &[gated("_comments")])),
            vec!["/admin/content/comments"]
        );

        let admin = UserContext::administrator(Uuid::now_v7(), vec![]);
        let all = dashboard(&admin, &[gated("_comments"), gated("_categories")]);
        assert_eq!(all.len(), CARDS.len(), "the administrator sees every card");
    }
}
