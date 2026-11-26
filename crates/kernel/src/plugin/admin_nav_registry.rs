//! Registry of the admin sidebar links and dashboard cards plugins declare
//! from `tap_admin_nav`.
//!
//! Built at boot from the tap's JSON output, exactly like
//! [`crate::menu::MenuRegistry`] and
//! [`crate::plugin::permission_registry::PluginPermissionRegistry`], and for
//! the same reason: a declaration is a constant, so it is collected once
//! here rather than asked for during a request.
//!
//! # A disabled plugin contributes nothing
//!
//! Only plugins enabled when the server starts are loaded, so only they are
//! dispatched here: a plugin disabled at boot never enters this registry,
//! and enabling it takes effect at the next start, as its routes do. The
//! sidebar and dashboard still check `enabled_plugins` on every request, the
//! same gate the kernel's own plugin-backed links use, so a plugin the server
//! itself stops running drops out with it.

use std::collections::HashSet;

use tracing::warn;
use trovato_sdk::types::AdminNavDefinition;

/// Longest label, section or description accepted, matching the admin theme's
/// layout assumptions rather than any protocol limit.
const MAX_TEXT_LEN: usize = 255;

/// One sidebar link a plugin declared, with the plugin that declared it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PluginNavLink {
    pub plugin: String,
    pub section: String,
    pub label: String,
    pub href: String,
    pub permission: String,
}

/// One dashboard card a plugin declared, from an [`AdminNavDefinition`] that
/// set `card_description`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PluginNavCard {
    pub plugin: String,
    pub title: String,
    pub description: String,
    pub href: String,
}

/// Why one declaration was dropped.
#[derive(Debug, Clone)]
pub struct AdminNavRejection {
    pub plugin: String,
    pub href: String,
    pub reason: String,
}

/// Every admin sidebar link and dashboard card declared by the plugins that
/// answered `tap_admin_nav`.
#[derive(Debug, Default)]
pub struct AdminNavRegistry {
    links: Vec<PluginNavLink>,
    cards: Vec<PluginNavCard>,
    rejections: Vec<AdminNavRejection>,
}

/// Characters that would break out of the unescaped `href` attribute the
/// sidebar and dashboard templates render with `| safe` (BL-162: that
/// `safe` is sound only because every href is developer-declared, never
/// request text — these are the characters that would make it not).
const UNSAFE_HREF_CHARS: &[char] = &['"', '\'', '<', '>', ' ', '\t', '\n', '\r'];

impl AdminNavRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse the dispatch results, dropping what cannot be shown.
    ///
    /// A rejection is never fatal: one plugin's malformed declaration must
    /// not stop a site booting, so it is recorded, logged once, and skipped.
    pub fn from_tap_results(results: Vec<(String, String)>) -> Self {
        let mut registry = Self::new();
        let mut seen_hrefs: HashSet<String> = HashSet::new();

        for (plugin, json) in results {
            // `#[plugin_tap]` serializes the return value, so an array arrives
            // as a JSON array — but a String-returning tap arrives as a JSON
            // string wrapping one, the same double encoding the view and api
            // paths already accept (G-VIEW-OUTPUT-JSON-ENCODED).
            let parsed = serde_json::from_str::<serde_json::Value>(&json)
                .map(|value| match value {
                    serde_json::Value::String(ref inner) => {
                        serde_json::from_str::<serde_json::Value>(inner).unwrap_or(value)
                    }
                    other => other,
                })
                .and_then(serde_json::from_value::<Vec<AdminNavDefinition>>);

            let definitions = match parsed {
                Ok(definitions) => definitions,
                Err(e) => {
                    registry.reject(&plugin, "<unparsed>", format!("output did not parse: {e}"));
                    continue;
                }
            };

            for def in definitions {
                if let Err(reason) = validate(&def, &seen_hrefs) {
                    registry.reject(&plugin, &def.href, reason);
                    continue;
                }

                seen_hrefs.insert(def.href.clone());
                registry.links.push(PluginNavLink {
                    plugin: plugin.clone(),
                    section: def.section.clone(),
                    label: def.label.clone(),
                    href: def.href.clone(),
                    permission: def.permission.clone(),
                });
                if let Some(description) = def.card_description {
                    registry.cards.push(PluginNavCard {
                        plugin: plugin.clone(),
                        title: def.label,
                        description,
                        href: def.href,
                    });
                }
            }
        }

        for rejection in &registry.rejections {
            warn!(
                plugin = %rejection.plugin,
                href = %rejection.href,
                reason = %rejection.reason,
                "dropping an invalid plugin admin nav declaration"
            );
        }

        registry
    }

    fn reject(&mut self, plugin: &str, href: &str, reason: String) {
        self.rejections.push(AdminNavRejection {
            plugin: plugin.to_string(),
            href: href.to_string(),
            reason,
        });
    }

    /// Every link a plugin declared, whether or not any viewer currently
    /// holds its permission or its plugin is currently enabled — callers
    /// filter per viewer with the same gate the kernel's own links use.
    pub fn links(&self) -> &[PluginNavLink] {
        &self.links
    }

    /// Every card a plugin declared, as [`links`](Self::links).
    pub fn cards(&self) -> &[PluginNavCard] {
        &self.cards
    }

    pub fn rejections(&self) -> &[AdminNavRejection] {
        &self.rejections
    }

    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    pub fn len(&self) -> usize {
        self.links.len()
    }
}

/// Reject a declaration this registry, and the templates that render it,
/// cannot show safely: an empty field, an href outside `/admin`, an href
/// already claimed by the kernel or another plugin, or an href carrying a
/// character that would break out of the unescaped attribute it is rendered
/// into.
fn validate(def: &AdminNavDefinition, seen_hrefs: &HashSet<String>) -> Result<(), String> {
    if def.section.trim().is_empty() {
        return Err("section is empty".to_string());
    }
    if def.label.trim().is_empty() {
        return Err("label is empty".to_string());
    }
    if def.permission.trim().is_empty() {
        return Err("permission is empty".to_string());
    }
    href_stays_under_admin(&def.href)?;
    if def.href.contains(|c| UNSAFE_HREF_CHARS.contains(&c)) {
        return Err("href contains a character unsafe for an unescaped attribute".to_string());
    }
    for (field, value) in [
        ("section", &def.section),
        ("label", &def.label),
        ("href", &def.href),
        ("permission", &def.permission),
    ] {
        if value.len() > MAX_TEXT_LEN {
            return Err(format!("{field} is longer than {MAX_TEXT_LEN} bytes"));
        }
    }
    if let Some(description) = &def.card_description
        && description.len() > MAX_TEXT_LEN
    {
        return Err(format!(
            "card_description is longer than {MAX_TEXT_LEN} bytes"
        ));
    }
    // A kernel link owns its href and every prefix in its `also` list: a
    // plugin link at one of those would tie with the kernel link for the
    // active highlight on every page beneath it.
    if seen_hrefs.contains(&def.href) || crate::routes::admin_nav::kernel_claims(&def.href) {
        return Err("another admin nav link already uses this href".to_string());
    }
    Ok(())
}

/// The href is `/admin/...` and a browser following it stays there: the
/// first segment is exactly `admin` (so not `/administrator`), and no segment
/// is a dot segment, plain or percent-encoded, nor separated by a backslash or
/// an empty segment, any of which a browser would resolve to somewhere else
/// (`/admin/../public` is `/public`).
fn href_stays_under_admin(href: &str) -> Result<(), String> {
    let Some(rest) = href.strip_prefix("/admin/") else {
        return Err("href is not a path under /admin/".to_string());
    };
    if href.contains('\\') {
        return Err("href contains a backslash".to_string());
    }
    // Only the path decides where the link lands; a query or fragment does not.
    let path = rest.split(['?', '#']).next().unwrap_or_default();
    for segment in path.split('/') {
        if segment.is_empty() {
            return Err("href has an empty path segment".to_string());
        }
        let decoded = segment.to_ascii_lowercase().replace("%2e", ".");
        if decoded == "." || decoded == ".." {
            return Err("href contains a dot segment".to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn def(href: &str) -> AdminNavDefinition {
        AdminNavDefinition::new("Structure", "Groups", href, "administer groups")
    }

    #[test]
    fn a_well_formed_declaration_is_kept() {
        let registry = AdminNavRegistry::from_tap_results(vec![(
            "groups".to_string(),
            serde_json::to_string(&vec![def("/admin/groups")]).unwrap(),
        )]);
        assert_eq!(registry.links().len(), 1);
        assert_eq!(registry.links()[0].plugin, "groups");
        assert_eq!(registry.links()[0].href, "/admin/groups");
        assert!(registry.cards().is_empty());
        assert!(registry.rejections().is_empty());
    }

    #[test]
    fn a_card_description_also_yields_a_card() {
        let with_card = def("/admin/groups").card("Manage user groups.");
        let registry = AdminNavRegistry::from_tap_results(vec![(
            "groups".to_string(),
            serde_json::to_string(&vec![with_card]).unwrap(),
        )]);
        assert_eq!(registry.cards().len(), 1);
        assert_eq!(registry.cards()[0].title, "Groups");
        assert_eq!(registry.cards()[0].description, "Manage user groups.");
        assert_eq!(registry.cards()[0].href, "/admin/groups");
    }

    #[test]
    fn an_href_outside_admin_is_rejected() {
        let registry = AdminNavRegistry::from_tap_results(vec![(
            "groups".to_string(),
            serde_json::to_string(&vec![def("/groups")]).unwrap(),
        )]);
        assert!(registry.links().is_empty());
        assert_eq!(registry.rejections().len(), 1);
    }

    /// Every href that starts with the characters `/admin` but lands
    /// somewhere else once a browser resolves it.
    #[test]
    fn an_href_that_leaves_admin_once_resolved_is_rejected() {
        for href in [
            "/admin",
            "/admin/",
            "/administrator",
            "/admin-tools/x",
            "/admin/../public",
            "/admin/groups/../../public",
            "/admin/%2e%2e/public",
            "/admin/%2E./public",
            "/admin/./groups",
            "/admin/groups\\..\\..\\public",
            "/admin//evil.example/x",
            "/admin/groups/",
        ] {
            let registry = AdminNavRegistry::from_tap_results(vec![(
                "groups".to_string(),
                serde_json::to_string(&vec![def(href)]).unwrap(),
            )]);
            assert!(registry.links().is_empty(), "{href} must be rejected");
        }
    }

    #[test]
    fn an_href_under_admin_with_a_query_is_kept() {
        let registry = AdminNavRegistry::from_tap_results(vec![(
            "groups".to_string(),
            serde_json::to_string(&vec![def("/admin/groups/list?sort=name")]).unwrap(),
        )]);
        assert_eq!(registry.links().len(), 1, "{:?}", registry.rejections());
    }

    /// `/admin/structure/tags` is no kernel link's own href, but Categories
    /// lists it in `also`: a plugin link there would tie with Categories for
    /// the highlight on every tag page.
    #[test]
    fn an_href_a_kernel_link_claims_through_also_is_rejected() {
        let claimed = crate::routes::admin_nav::LINKS
            .iter()
            .flat_map(|l| l.also.iter().copied())
            .collect::<Vec<_>>();
        assert!(claimed.contains(&"/admin/structure/tags"));
        for href in claimed {
            let registry = AdminNavRegistry::from_tap_results(vec![(
                "groups".to_string(),
                serde_json::to_string(&vec![def(href)]).unwrap(),
            )]);
            assert!(registry.links().is_empty(), "{href} must be rejected");
        }
    }

    #[test]
    fn an_href_that_collides_with_a_kernel_link_is_rejected() {
        let registry = AdminNavRegistry::from_tap_results(vec![(
            "groups".to_string(),
            serde_json::to_string(&vec![def("/admin/content")]).unwrap(),
        )]);
        assert!(registry.links().is_empty());
        assert_eq!(registry.rejections().len(), 1);
    }

    #[test]
    fn two_plugins_cannot_claim_the_same_href() {
        let registry = AdminNavRegistry::from_tap_results(vec![
            (
                "groups".to_string(),
                serde_json::to_string(&vec![def("/admin/groups")]).unwrap(),
            ),
            (
                "other".to_string(),
                serde_json::to_string(&vec![def("/admin/groups")]).unwrap(),
            ),
        ]);
        assert_eq!(registry.links().len(), 1);
        assert_eq!(registry.links()[0].plugin, "groups");
        assert_eq!(registry.rejections().len(), 1);
    }

    #[test]
    fn an_href_with_a_quote_is_rejected() {
        let registry = AdminNavRegistry::from_tap_results(vec![(
            "groups".to_string(),
            serde_json::to_string(&vec![def("/admin/groups\" onmouseover=alert(1)")]).unwrap(),
        )]);
        assert!(registry.links().is_empty());
        assert_eq!(registry.rejections().len(), 1);
    }

    #[test]
    fn an_empty_permission_is_rejected() {
        let mut d = def("/admin/groups");
        d.permission = String::new();
        let registry = AdminNavRegistry::from_tap_results(vec![(
            "groups".to_string(),
            serde_json::to_string(&vec![d]).unwrap(),
        )]);
        assert!(registry.links().is_empty());
        assert_eq!(registry.rejections().len(), 1);
    }

    #[test]
    fn malformed_json_is_rejected_without_panicking() {
        let registry = AdminNavRegistry::from_tap_results(vec![(
            "groups".to_string(),
            "not json".to_string(),
        )]);
        assert!(registry.links().is_empty());
        assert_eq!(registry.rejections().len(), 1);
    }
}
