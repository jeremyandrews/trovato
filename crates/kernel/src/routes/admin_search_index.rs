//! What the admin search finds, grouped by kind.
//!
//! One function, [`search`], answers both the results page
//! (`GET /admin/search?q=`) and the command palette
//! (`GET /admin/search/suggest?q=`). The palette therefore shows exactly
//! what the page shows.
//!
//! The groups, in order:
//! - **Pages**: the registry's pages (`admin_nav::search`).
//! - **Settings**: every individual setting field, each linking to the field
//!   itself on its form (`#field-id`).
//! - **Content**: item titles.
//! - **People**: user names and email addresses.
//! - **Plugins**: the plugins on disk, and the pages the enabled ones
//!   declare.
//! - **Log messages**: recent site log messages.
//!
//! Every group is filtered by the viewer's access. A group is searched only
//! when the viewer may open the page it belongs to. A setting is offered only
//! when its form is offered. Nothing is cached: each search reads the
//! registry, the enabled plugins and the database as they are now, so a
//! disabled plugin's pages, or a deleted item, are gone from the very next
//! search.

use serde::Serialize;

use crate::i18n::UiMessage;
use crate::state::AppState;
use crate::tap::UserContext;

use super::admin_list::ListQuery;
use super::admin_nav;

/// Rows a data group (content, people, log) shows; the full listing, filtered
/// by the same words, is one link away.
const DATA_LIMIT: i64 = 8;

/// One result.
#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    /// What the result is called: an English source the template translates
    /// (`translate`), or data shown as it is.
    pub label: String,
    pub translate: bool,
    /// One line of context: where it lives, or what it is.
    pub context: UiMessage,
    pub href: String,
}

/// One group of results under its heading.
#[derive(Debug, Clone, Serialize)]
pub struct HitGroup {
    /// A stable key, for ids.
    pub key: &'static str,
    /// The heading: an English source.
    pub title: &'static str,
    pub hits: Vec<Hit>,
    /// A link to every match, when the group shows only the first few.
    pub more: Option<String>,
}

/// One setting field the search can land on.
#[derive(Debug, Clone, Copy)]
pub struct Setting {
    /// The form's page.
    pub page: &'static str,
    /// The field's id on that page.
    pub anchor: &'static str,
    /// The field's label and hint: English sources, as the form shows them.
    pub label: &'static str,
    pub hint: &'static str,
    /// More words someone might search for.
    pub synonyms: &'static str,
}

const fn setting(
    page: &'static str,
    anchor: &'static str,
    label: &'static str,
    hint: &'static str,
    synonyms: &'static str,
) -> Setting {
    Setting {
        page,
        anchor,
        label,
        hint,
        synonyms,
    }
}

const SITE: &str = "/admin/config/site";
const SEARCH_SETTINGS: &str = "/admin/config/search";
const RECOVERY: &str = "/admin/recovery";
const PREFERENCES: &str = "/admin/preferences";

/// The setting fields of the settings forms. A test opens each page and
/// checks that its anchor is a field id there.
pub const SETTINGS: &[Setting] = &[
    setting(SITE, "site_name", "Site name", "", "title name of the site"),
    setting(SITE, "site_slogan", "Site slogan", "", "tagline motto"),
    setting(
        SITE,
        "site_mail",
        "Site email",
        "Used as the From address for system emails.",
        "email address from sender",
    ),
    setting(
        SITE,
        "front_page",
        "Default front page",
        "",
        "home page homepage start",
    ),
    setting(
        SITE,
        "items_per_page",
        "Items per page",
        "",
        "paging page size",
    ),
    setting(
        SITE,
        "reg_open",
        "User registration",
        "",
        "sign up signup register accounts",
    ),
    setting(
        SITE,
        "notify_admin_on_register",
        "Notify admin when new users register",
        "",
        "email notification registration",
    ),
    setting(
        SITE,
        "update_check",
        "Check for Trovato releases",
        "",
        "updates version",
    ),
    setting(
        SITE,
        "count_content_views",
        "Count content views",
        "",
        "statistics reads hits",
    ),
    setting(
        SEARCH_SETTINGS,
        "expand",
        "Query expansion: related terms searched alongside the query",
        "",
        "ai synonyms",
    ),
    setting(
        SEARCH_SETTINGS,
        "summarize",
        "Summary: an AI answer above the results",
        "",
        "ai answer",
    ),
    setting(
        SEARCH_SETTINGS,
        "followup",
        "Follow-up questions after the summary",
        "",
        "ai",
    ),
    setting(
        SEARCH_SETTINGS,
        "analytics_enabled",
        "Record search analytics",
        "",
        "statistics tracking",
    ),
    setting(
        SEARCH_SETTINGS,
        "sentiment_enabled",
        "Classify follow-up sentiment",
        "",
        "ai",
    ),
    setting(
        RECOVERY,
        "email-enabled",
        "Emailed recovery code",
        "Sends a one-time code to the address on the account. Phishable.",
        "password reset lost account",
    ),
    setting(
        RECOVERY,
        "codes-enabled",
        "Saved recovery codes",
        "Users generate codes and keep them. No email, no second device.",
        "backup codes lost account",
    ),
    setting(
        PREFERENCES,
        "theme",
        "Colour theme",
        "System follows your device's light or dark setting.",
        "color dark mode light mode appearance",
    ),
    setting(
        PREFERENCES,
        "admin-language",
        "Administration language",
        "The language these pages are shown in. It does not change the language of your content.",
        "locale translation",
    ),
    setting(
        PREFERENCES,
        "search_slash",
        "Open search with /",
        "",
        "keyboard shortcut command palette",
    ),
    setting(
        PREFERENCES,
        "search_key",
        "Another key to open search",
        "",
        "keyboard shortcut command palette",
    ),
];

/// Every setting the search knows: the list above, and each of the search
/// form's numeric settings, named as that form names them.
pub fn settings() -> Vec<Setting> {
    let mut all = SETTINGS.to_vec();
    for range in crate::search::settings::NUMBER_RANGES {
        all.push(setting(
            SEARCH_SETTINGS,
            range.field,
            range.label,
            "",
            "ranking weight",
        ));
    }
    all
}

/// Whether every word of `words` appears in one of `texts` (lower case).
fn all_words(words: &[String], texts: &[&str]) -> bool {
    let haystack = texts.join(" ").to_lowercase();
    words.iter().all(|w| haystack.contains(w.as_str()))
}

/// Search for `query` as `viewer`, in the viewer's admin language `lang`.
///
/// `translate` turns an English source into the viewer's language, so a
/// translated word finds its page too.
pub async fn search(
    state: &AppState,
    viewer: &UserContext,
    query: &str,
    translate: &(dyn Fn(&str) -> String + Sync),
) -> Vec<HitGroup> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut enabled: Vec<String> = state.enabled_plugins().into_iter().collect();
    enabled.sort();
    let nav = state.plugin_admin_nav();
    let offered = admin_nav::offered_pages(viewer, &enabled, nav.links(), nav.cards());
    let may_open = |href: &str| offered.iter().any(|p| p.href == href);
    let page_title = |href: &str| {
        offered
            .iter()
            .find(|p| p.href == href)
            .map(|p| translate(&p.label))
            .unwrap_or_default()
    };

    let mut groups = Vec::new();

    // Pages, and the pages plugins declare (shown under Plugins).
    let pages = admin_nav::search(viewer, &enabled, nav.links(), nav.cards(), query, translate);
    let (plugin_pages, kernel_pages): (Vec<_>, Vec<_>) = pages.into_iter().partition(|hit| {
        offered
            .iter()
            .any(|p| p.href == hit.href && p.plugin.is_some())
    });
    let as_hit = |hit: admin_nav::SearchHit| Hit {
        context: match &hit.group {
            Some(group) => {
                UiMessage::new("Administration › {group}").arg("group", translate(group))
            }
            None => UiMessage::new("Administration"),
        },
        label: hit.label,
        translate: true,
        href: hit.href,
    };
    groups.push(HitGroup {
        key: "pages",
        title: "Pages",
        hits: kernel_pages.into_iter().map(as_hit).collect(),
        more: None,
    });

    // Settings: the field itself, on a form the viewer may open.
    let settings: Vec<Hit> = settings()
        .into_iter()
        .filter(|s| may_open(s.page))
        .filter(|s| {
            all_words(
                &words,
                &[
                    s.label,
                    &translate(s.label),
                    s.hint,
                    &if s.hint.is_empty() {
                        String::new()
                    } else {
                        translate(s.hint)
                    },
                    s.synonyms,
                ],
            )
        })
        .map(|s| Hit {
            label: s.label.to_string(),
            translate: true,
            context: UiMessage::new("Setting on {page}").arg("page", page_title(s.page)),
            href: format!("{}#{}", s.page, s.anchor),
        })
        .collect();
    groups.push(HitGroup {
        key: "settings",
        title: "Settings",
        hits: settings,
        more: None,
    });

    let list =
        ListQuery::from_params(&[("q".to_string(), query.to_string())].into_iter().collect());
    let like = list.like();
    let encoded = urlencoding::encode(query.trim()).into_owned();

    // Content: titles, for whoever may open the content listing. As the
    // listing does: `edit any content` sees every item and its admin edit
    // form; whoever holds only the overview sees the items they may view,
    // linked to the edit form they may use, or to the item itself.
    if may_open("/admin/content") {
        let (items, total) = state
            .items()
            .admin_page(None, None, like.as_deref(), "changed DESC", DATA_LIMIT, 0)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "admin search: content");
                (Vec::new(), 0)
            });
        let editor = viewer.can("edit any content");
        let allowed = |item: &crate::models::Item, op: &'static str| {
            let item = item.clone();
            async move {
                matches!(
                    state.items().check_access(&item, op, viewer).await,
                    Ok(true)
                )
            }
        };
        let mut hits = Vec::new();
        for item in items {
            let href = if editor {
                format!("/admin/content/{}/edit", item.id)
            } else if allowed(&item, "edit").await {
                format!("/item/{}/edit", item.id)
            } else if allowed(&item, "view").await {
                format!("/item/{}", item.id)
            } else {
                continue;
            };
            hits.push(Hit {
                context: if item.status == 1 {
                    UiMessage::new("{type}, published").arg("type", item.item_type.clone())
                } else {
                    UiMessage::new("{type}, unpublished").arg("type", item.item_type.clone())
                },
                label: item.title,
                translate: false,
                href,
            });
        }
        groups.push(HitGroup {
            key: "content",
            title: "Content",
            hits,
            more: (total > DATA_LIMIT).then(|| format!("/admin/content?q={encoded}")),
        });
    }

    // People: names and addresses, for whoever may open the people listing.
    if may_open("/admin/people") {
        let (users, total) = state
            .users()
            .admin_page(like.as_deref(), None, "name ASC", DATA_LIMIT, 0)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "admin search: people");
                (Vec::new(), 0)
            });
        groups.push(HitGroup {
            key: "people",
            title: "People",
            hits: users
                .into_iter()
                .filter(|u| !u.id.is_nil())
                .map(|u| Hit {
                    label: u.name,
                    translate: false,
                    context: UiMessage::new("Account, {mail}").arg("mail", u.mail),
                    href: format!("/admin/people/{}/edit", u.id),
                })
                .collect(),
            more: (total > DATA_LIMIT).then(|| format!("/admin/people?q={encoded}")),
        });
    }

    // Plugins: those on disk, by name and description, and the pages the
    // enabled ones declare.
    let mut plugin_hits: Vec<Hit> = plugin_pages.into_iter().map(as_hit).collect();
    if may_open("/admin/plugins") {
        // The plugins page's rows: those on disk, and those installed but
        // no longer on disk.
        let discovered =
            crate::plugin::runtime::PluginRuntime::discover_plugins(state.plugins_dirs()).await;
        let mut known: Vec<(String, String)> = discovered
            .iter()
            .map(|(name, (info, _))| (name.clone(), info.description.clone()))
            .collect();
        for installed in crate::plugin::status::get_all_statuses(state.db())
            .await
            .unwrap_or_default()
        {
            if !discovered.contains_key(&installed.name) {
                known.push((installed.name, String::new()));
            }
        }
        known.sort();
        for (name, description) in &known {
            if all_words(&words, &[name, description]) {
                plugin_hits.push(Hit {
                    label: name.clone(),
                    translate: false,
                    context: if state.is_plugin_enabled(name) {
                        UiMessage::new("Plugin, enabled")
                    } else {
                        UiMessage::new("Plugin, not enabled")
                    },
                    href: format!("/admin/plugins#plugin-{name}"),
                });
            }
        }
    }
    groups.push(HitGroup {
        key: "plugins",
        title: "Plugins",
        hits: plugin_hits,
        more: None,
    });

    // Log messages: the latest that match, for whoever may read the log.
    if may_open("/admin/reports/log") {
        let filter = crate::system_log::LogFilter {
            message_like: like.clone(),
            ..Default::default()
        };
        let (entries, total) = crate::system_log::list(state.db(), &filter, None, DATA_LIMIT, 0)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "admin search: log");
                (Vec::new(), 0)
            });
        groups.push(HitGroup {
            key: "log",
            title: "Log messages",
            hits: entries
                .into_iter()
                .map(|e| Hit {
                    label: shorten(&e.message, 120),
                    translate: false,
                    context: UiMessage::new("{level} in {type}")
                        .arg("level", e.level)
                        .arg("type", e.log_type),
                    href: format!(
                        "/admin/reports/log?q={}",
                        urlencoding::encode(&e.message.chars().take(60).collect::<String>())
                    ),
                })
                .collect(),
            more: (total > DATA_LIMIT).then(|| format!("/admin/reports/log?q={encoded}")),
        });
    }

    groups.retain(|g| !g.hits.is_empty());
    groups
}

/// `text` cut to `max` characters, with an ellipsis when it was longer.
pub(crate) fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max - 1).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

/// How many results the groups hold.
pub fn count(groups: &[HitGroup]) -> usize {
    groups.iter().map(|g| g.hits.len()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_log_message_is_shortened_with_an_ellipsis() {
        assert_eq!(shorten("short", 120), "short");
        let long = "word ".repeat(40);
        let cut = shorten(&long, 120);
        assert!(cut.chars().count() <= 120);
        assert!(cut.ends_with("word…"), "{cut}");
    }

    #[test]
    fn every_setting_names_a_page_and_an_anchor() {
        for s in settings() {
            assert!(s.page.starts_with("/admin"), "{s:?}");
            assert!(!s.anchor.is_empty() && !s.anchor.contains(' '), "{s:?}");
            assert!(!s.label.is_empty(), "{s:?}");
        }
    }

    #[test]
    fn words_must_all_appear() {
        let w = vec!["site".to_string(), "mail".to_string()];
        assert!(all_words(&w, &["Site email"]));
        assert!(!all_words(&w, &["Site name"]));
    }
}
