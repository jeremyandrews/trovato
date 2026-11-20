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
        plugin: Some("comments"),
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
        plugin: Some("categories"),
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
        "System",
        "Site settings",
        "/admin/config/site",
        Permission("administer site"),
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
        .filter(|l| l.gate.admits(viewer))
        .filter(|l| {
            l.plugin
                .is_none_or(|p| enabled_plugins.iter().any(|e| e == p))
        })
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
            hrefs(&sidebar(&v, &["comments".to_string()], "/admin"))
                .contains(&"/admin/content/comments")
        );
    }

    #[test]
    fn the_longest_matching_link_is_active() {
        let admin = UserContext::administrator(Uuid::now_v7(), vec![]);
        let active = |path: &str| -> Vec<&'static str> {
            sidebar(&admin, &["categories".to_string()], path)
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
}
