//! The admin registry: every administration page, the task group it belongs
//! to, the permission its page asks for, and the filter that shows a viewer
//! only the pages they may open.
//!
//! One table feeds the sidebar, the group landing pages, the breadcrumbs,
//! the dashboard and admin search, so the five cannot drift apart. Before
//! B199 there were two hand kept arrays, a sidebar list (`LINKS`, 30 links in
//! five sections) and a dashboard list (`CARDS`), and about 116 admin paths,
//! so pathauto, record types, account recovery, sessions and translation were
//! reachable from neither.
//!
//! The gate of each entry is the one its GET handler checks (BL-117: a
//! delegated role used to see links that answered 403); when a page's
//! permission changes, this table changes with it. The sidebar and search
//! are display only: every route still checks its own permission.
//!
//! A plugin's pages are [`crate::plugin::admin_nav_registry::PluginNavLink`]
//! and `PluginNavCard`, declared through `tap_admin_nav` and collected once at
//! boot into [`crate::state::AppState::plugin_admin_nav`] (BL-162). Their
//! declared section names the group they join ([`group_for_section`]).
//! [`sidebar`], [`dashboard`], [`landing`] and [`search`] filter both sources
//! through the same permission and plugin-enablement gate.
//!
//! `docs/admin/design.md` is the design this implements and
//! `docs/admin/inventory.md` the route by route table it is tested against.

use serde::Serialize;

use crate::plugin::admin_nav_registry::{PluginNavCard, PluginNavLink};
use crate::tap::UserContext;

/// A task group: one of the seven top level areas of the admin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Group {
    Content,
    Structure,
    Appearance,
    People,
    Extend,
    Configuration,
    Reports,
}

/// A group's name, address and purpose.
#[derive(Debug, Clone, Copy)]
pub struct GroupInfo {
    pub group: Group,
    /// The address segment: the landing page is `/admin/{slug}/overview`.
    pub slug: &'static str,
    /// The heading, in the source language.
    pub title: &'static str,
    /// One sentence on what the group is for: its landing page intro and its
    /// dashboard card line.
    pub description: &'static str,
    /// Its icon, one of [`ICONS`]: on its sidebar heading, its dashboard
    /// card, and every page in it that has none of its own.
    pub icon: &'static str,
}

/// Every icon `templates/admin/macros/icons.html` draws, by the object it
/// shows. A group or page names one of these; a test renders each and holds
/// each to being used.
pub const ICONS: &[&str] = &[
    "document",
    "blocks",
    "eye",
    "people",
    "layers",
    "sliders",
    "clipboard",
    "users",
    "badge",
    "lock",
    "image",
    "archive",
    "speech",
    "plug",
    "log",
    "hourglass",
];

/// The groups, in sidebar order.
pub const GROUPS: &[GroupInfo] = &[
    GroupInfo {
        group: Group::Content,
        slug: "content",
        title: "Content",
        description: "Write, find, review and publish what the site says, and the files it uses.",
        icon: "document",
    },
    GroupInfo {
        group: Group::Structure,
        slug: "structure",
        title: "Structure",
        description: "Shape the site: content types and fields, listings, menus, regions, categories and addresses.",
        icon: "blocks",
    },
    GroupInfo {
        group: Group::Appearance,
        slug: "appearance",
        title: "Appearance",
        description: "Choose how the site and this administration look.",
        icon: "eye",
    },
    GroupInfo {
        group: Group::People,
        slug: "people",
        title: "People",
        description: "Accounts, roles, what each role may do, and who may register.",
        icon: "people",
    },
    GroupInfo {
        group: Group::Extend,
        slug: "extend",
        title: "Extend",
        description: "Plugins, the pages they add, and the AI services the site may call.",
        icon: "layers",
    },
    GroupInfo {
        group: Group::Configuration,
        slug: "config",
        title: "Configuration",
        description: "Site wide settings: identity, text formats, search, editorial stages and account recovery.",
        icon: "sliders",
    },
    GroupInfo {
        group: Group::Reports,
        slug: "reports",
        title: "Reports",
        description: "What the site has been doing: its log, audit trail, scheduled tasks and searches.",
        icon: "clipboard",
    },
];

impl Group {
    /// This group's [`GroupInfo`].
    pub fn info(self) -> &'static GroupInfo {
        GROUPS
            .iter()
            .find(|g| g.group == self)
            .unwrap_or(&GROUPS[0])
    }

    /// The landing page address.
    pub fn landing_href(self) -> String {
        format!("/admin/{}/overview", self.info().slug)
    }

    /// The group whose slug is `slug`.
    pub fn from_slug(slug: &str) -> Option<Self> {
        GROUPS.iter().find(|g| g.slug == slug).map(|g| g.group)
    }
}

/// The group a plugin's declared `section` joins: a group's title, in any
/// case, or `System`, the pre-B199 name of what is now Configuration. Any
/// other name joins Extend, which holds what plugins add: the admin has seven
/// groups, and a plugin does not get to start an eighth.
pub fn group_for_section(section: &str) -> Group {
    let section = section.trim();
    if section.eq_ignore_ascii_case("system") {
        return Group::Configuration;
    }
    GROUPS
        .iter()
        .find(|g| g.title.eq_ignore_ascii_case(section) || g.slug.eq_ignore_ascii_case(section))
        .map(|g| g.group)
        .unwrap_or(Group::Extend)
}

/// What a viewer must hold to open a page.
#[derive(Debug, Clone, Copy)]
pub enum Gate {
    /// A permission, by name, as `require_permission` asks for it.
    Permission(&'static str),
    /// Any one of these permissions.
    AnyPermission(&'static [&'static str]),
    /// Whoever may create some content type: `create content`, or one
    /// `create {type} content`, as the "Add content" chooser asks.
    CreatesContent,
    /// The site administrator (`users.is_admin`), as `require_admin` asks.
    Administrator,
}

impl Gate {
    fn admits(self, viewer: &UserContext) -> bool {
        match self {
            Self::Permission(permission) => viewer.can(permission),
            Self::AnyPermission(permissions) => permissions.iter().any(|p| viewer.can(p)),
            Self::CreatesContent => {
                viewer.can("create content")
                    || viewer
                        .permissions
                        .iter()
                        .any(|p| p.starts_with("create ") && p.ends_with(" content"))
            }
            Self::Administrator => viewer.is_admin(),
        }
    }

    /// The permissions that open the page, for "Who can do this?": empty for
    /// the site administrator alone.
    pub fn permissions(self) -> Vec<&'static str> {
        match self {
            Self::Permission(p) => vec![p],
            Self::AnyPermission(ps) => ps.to_vec(),
            // And any `create {type} content`, which no static list can name.
            Self::CreatesContent => vec!["create content"],
            Self::Administrator => Vec::new(),
        }
    }
}

/// One administration page.
#[derive(Debug, Clone, Copy)]
pub struct Link {
    /// Its group; `None` for the personal pages (Dashboard, preferences).
    pub group: Option<Group>,
    /// Its title, in the source language: the link text and the heading.
    pub label: &'static str,
    /// One sentence on what it is for.
    pub description: &'static str,
    /// Where it is.
    pub href: &'static str,
    /// What its page requires.
    pub gate: Gate,
    /// A plugin that must be enabled for the page to exist.
    pub plugin: Option<&'static str>,
    /// Other path prefixes that belong to this page.
    pub also: &'static [&'static str],
    /// Other words a person might search for it by.
    pub synonyms: &'static [&'static str],
    /// Whether the sidebar lists it. Every entry is on its landing page and
    /// in search either way.
    pub sidebar: bool,
    /// Its own icon, one of [`ICONS`]; `None` takes its group's, and a page
    /// in no group has none.
    pub icon: Option<&'static str>,
    /// The state its landing tile shows ([`status_line`]), if any.
    pub status: Option<Status>,
    /// Whether it is its group's main action: a filled button beside the
    /// landing page's heading and on the dashboard card. At most one a group.
    pub primary: bool,
}

impl Link {
    /// The icon this page shows: its own, else its group's, else none.
    pub fn resolved_icon(&self) -> &'static str {
        match (self.icon, self.group) {
            (Some(icon), _) => icon,
            (None, Some(group)) => group.info().icon,
            (None, None) => "",
        }
    }
}

const fn page(
    group: Group,
    label: &'static str,
    href: &'static str,
    gate: Gate,
    description: &'static str,
) -> Link {
    Link {
        group: Some(group),
        label,
        description,
        href,
        gate,
        plugin: None,
        also: &[],
        synonyms: &[],
        sidebar: true,
        icon: None,
        status: None,
        primary: false,
    }
}

use Gate::{Administrator, AnyPermission, CreatesContent, Permission};
use Group::{Appearance, Configuration, Content, Extend, People, Reports, Structure};

/// The comments plugin, by the name `AppState::enabled_plugins` holds it under.
///
/// The sidebar once tested `"comments"` and `"categories"`, names no plugin
/// has, so Comments and Categories were hidden from everyone; its own tests
/// passed the same wrong names and passed. A test below holds every plugin
/// named here to the list the route gate is registered from.
const COMMENTS: &str = "trovato_comments";
/// The categories plugin, as [`COMMENTS`].
const CATEGORIES: &str = "trovato_categories";
/// The redirects plugin, as [`COMMENTS`].
const REDIRECTS: &str = "trovato_redirects";
/// The audit log plugin, as [`COMMENTS`].
const AUDIT_LOG: &str = "trovato_audit_log";

/// The address of the Dashboard.
pub const DASHBOARD: &str = "/admin";
/// The address of the viewer's own admin preferences.
pub const PREFERENCES: &str = "/admin/preferences";
/// The address of admin search.
pub const SEARCH: &str = "/admin/search";

/// Every kernel administration page, grouped, in display order.
pub const LINKS: &[Link] = &[
    Link {
        group: None,
        label: "Dashboard",
        description: "What needs your attention, and a way into every area you may use.",
        href: DASHBOARD,
        gate: Permission("access administration pages"),
        plugin: None,
        also: &[],
        synonyms: &["home", "start", "overview"],
        sidebar: true,
        icon: None,
        status: None,
        primary: false,
    },
    Link {
        group: None,
        label: "Your admin preferences",
        description: "The colour theme and language this administration uses for you.",
        href: PREFERENCES,
        gate: Permission("access administration pages"),
        plugin: None,
        also: &[],
        synonyms: &["dark mode", "light mode", "theme", "language", "display"],
        sidebar: false,
        icon: None,
        status: None,
        primary: false,
    },
    Link {
        group: None,
        label: "Search the administration",
        description: "Find any administration page by name or purpose.",
        href: SEARCH,
        gate: Permission("access administration pages"),
        plugin: None,
        also: &[],
        synonyms: &["find"],
        sidebar: false,
        icon: None,
        status: None,
        primary: false,
    },
    // --- Content ---
    Link {
        status: Some(Status::Unpublished),
        also: &["/admin/content/bulk"],
        synonyms: &[
            "items",
            "pages",
            "posts",
            "articles",
            "edit",
            "translate",
            "revisions",
        ],
        ..page(
            Content,
            "Content",
            "/admin/content",
            AnyPermission(&["access content overview", "edit any content"]),
            "Find, filter and edit every item on the site.",
        )
    },
    Link {
        primary: true,
        synonyms: &["new", "create", "write"],
        ..page(
            Content,
            "Add content",
            "/admin/content/add",
            CreatesContent,
            "Start a new item of any content type you may create.",
        )
    },
    Link {
        synonyms: &["drafts", "review", "latest version", "pending"],
        ..page(
            Content,
            "Moderated content",
            "/admin/content/moderated",
            AnyPermission(&["view latest version", "edit any content"]),
            "Items with a newer version waiting to be reviewed or published.",
        )
    },
    Link {
        status: Some(Status::CommentsWaiting),
        icon: Some("speech"),
        plugin: Some(COMMENTS),
        synonyms: &["moderation", "spam", "replies"],
        ..page(
            Content,
            "Comments",
            "/admin/content/comments",
            Permission("administer comments"),
            "Approve, edit and remove comments, and set how new ones are published.",
        )
    },
    Link {
        icon: Some("archive"),
        synonyms: &["uploads", "attachments", "alt text"],
        ..page(
            Content,
            "Files",
            "/admin/content/files",
            Permission("access files"),
            "Every uploaded file, with its details and alternative text.",
        )
    },
    Link {
        icon: Some("image"),
        synonyms: &["images", "pictures", "library"],
        ..page(
            Content,
            "Media",
            "/admin/media",
            Permission("access files"),
            "Browse images and other media by file name.",
        )
    },
    // --- Structure ---
    Link {
        synonyms: &["fields", "bundles", "schema", "search fields"],
        ..page(
            Structure,
            "Content types",
            "/admin/structure/types",
            Permission("administer site"),
            "The kinds of content the site holds, and the fields each one has.",
        )
    },
    Link {
        primary: true,
        sidebar: false,
        synonyms: &["new type", "create type", "bundle"],
        ..page(
            Structure,
            "Add content type",
            "/admin/structure/types/add",
            Permission("administer site"),
            "Create a new kind of content, then give it its fields.",
        )
    },
    Link {
        synonyms: &["records", "data"],
        ..page(
            Structure,
            "Record types",
            "/admin/structure/records",
            Permission("administer site"),
            "Structured records plugins keep, and the records of each type.",
        )
    },
    Link {
        synonyms: &["views", "listings", "queries", "feeds", "lists"],
        ..page(
            Structure,
            "Gathers",
            "/admin/gather",
            Permission("administer site"),
            "Listings and feeds built from content, with their filters and sorts.",
        )
    },
    Link {
        synonyms: &["navigation", "links"],
        ..page(
            Structure,
            "Menus",
            "/admin/structure/menus",
            Permission("administer site"),
            "Navigation menus and the links in them.",
        )
    },
    Link {
        synonyms: &["blocks", "regions", "sidebar", "widgets"],
        ..page(
            Structure,
            "Tiles",
            "/admin/structure/tiles",
            Permission("administer site"),
            "What appears in each region of the site's pages.",
        )
    },
    Link {
        plugin: Some(CATEGORIES),
        also: &["/admin/structure/tags"],
        synonyms: &["taxonomy", "vocabularies", "tags", "terms"],
        ..page(
            Structure,
            "Categories",
            "/admin/structure/categories",
            AnyPermission(&[
                "administer categories",
                "create category terms",
                "edit category terms",
                "delete category terms",
            ]),
            "Vocabularies and the tags in each, for classifying content.",
        )
    },
    Link {
        synonyms: &["paths", "urls", "addresses", "redirects", "slugs"],
        ..page(
            Structure,
            "URL aliases",
            "/admin/structure/aliases",
            Permission("administer site"),
            "Readable addresses for items and pages.",
        )
    },
    Link {
        synonyms: &["pathauto", "automatic aliases", "patterns", "slugs"],
        ..page(
            Structure,
            "URL alias patterns",
            "/admin/config/pathauto",
            Permission("administer site"),
            "Patterns that give new content a readable address automatically.",
        )
    },
    // --- Appearance ---
    Link {
        synonyms: &["themes", "look", "design", "admin theme", "skin"],
        ..page(
            Appearance,
            "Themes",
            "/admin/appearance",
            Permission("administer themes"),
            "The theme visitors see, and the one this administration uses.",
        )
    },
    // --- People ---
    Link {
        status: Some(Status::BlockedAccounts),
        icon: Some("users"),
        also: &["/admin/users"],
        synonyms: &["accounts", "members", "sessions", "devices", "block"],
        ..page(
            People,
            "Users",
            "/admin/people",
            Permission("administer users"),
            "Every account: find, edit, block, and sign out of devices.",
        )
    },
    Link {
        primary: true,
        sidebar: false,
        synonyms: &["new user", "create account", "invite"],
        ..page(
            People,
            "Add user",
            "/admin/people/add",
            Permission("administer users"),
            "Create an account and choose its roles.",
        )
    },
    Link {
        icon: Some("badge"),
        synonyms: &["groups"],
        ..page(
            People,
            "Roles",
            "/admin/people/roles",
            Permission("administer users"),
            "Named sets of permissions that accounts are given.",
        )
    },
    Link {
        icon: Some("lock"),
        synonyms: &["access", "rights", "grants", "who can"],
        ..page(
            People,
            "Permissions",
            "/admin/people/permissions",
            Permission("administer users"),
            "What each role may do, as a grid of roles and permissions.",
        )
    },
    Link {
        sidebar: false,
        synonyms: &["who can", "which roles", "access"],
        ..page(
            People,
            "Who can do this?",
            "/admin/people/who-can",
            Permission("administer users"),
            "Which permission opens each administration page, and the roles that hold it.",
        )
    },
    Link {
        synonyms: &["ban", "block", "allow", "deny", "registration"],
        ..page(
            People,
            "Access rules",
            "/admin/people/rules",
            Permission("administer access rules"),
            "Names, addresses and hosts that may or may not register and sign in.",
        )
    },
    // --- Extend ---
    Link {
        icon: Some("plug"),
        also: &["/admin/plugins"],
        synonyms: &["modules", "extensions", "add-ons", "enable", "disable"],
        ..page(
            Extend,
            "Plugins",
            "/admin/plugins",
            Administrator,
            "Turn plugins on and off; each adds features and pages of its own.",
        )
    },
    Link {
        synonyms: &["openai", "anthropic", "llm", "api keys", "models"],
        ..page(
            Extend,
            "AI providers",
            "/admin/system/ai-providers",
            Permission("configure ai"),
            "The AI services the site may call, and which one does each job.",
        )
    },
    Link {
        synonyms: &["tokens", "usage", "limits", "spending", "cost"],
        ..page(
            Extend,
            "AI budgets",
            "/admin/system/ai-budgets",
            Permission("view ai usage"),
            "How much AI each role and person may use, and how much they have used.",
        )
    },
    Link {
        synonyms: &["chatbot", "assistant", "conversation"],
        ..page(
            Extend,
            "AI chat",
            "/admin/system/ai-chat",
            Permission("configure ai"),
            "How the site's chat answers visitors, and what it may draw on.",
        )
    },
    Link {
        synonyms: &["writing assistant", "copilot", "help"],
        ..page(
            Extend,
            "AI assistant",
            "/admin/system/ai-assistant",
            Permission("configure ai"),
            "The assistant that helps editors write and administer the site.",
        )
    },
    // --- Configuration ---
    Link {
        synonyms: &[
            "name",
            "slogan",
            "front page",
            "email",
            "maintenance",
            "mail",
        ],
        ..page(
            Configuration,
            "Site settings",
            "/admin/config/site",
            Permission("administer site"),
            "The site's name, front page, email and maintenance mode.",
        )
    },
    Link {
        synonyms: &["cache", "clear cache", "flush", "speed", "rebuild"],
        ..page(
            Configuration,
            "Performance",
            "/admin/config/performance",
            AnyPermission(&["clear caches", "administer site"]),
            "Clear every cached copy on every server.",
        )
    },
    Link {
        synonyms: &[
            "language",
            "translation",
            "multilingual",
            "locale",
            "prefix",
        ],
        ..page(
            Configuration,
            "Languages",
            "/admin/config/languages",
            Permission("administer languages"),
            "The languages content is written in, their order and address prefixes.",
        )
    },
    Link {
        synonyms: &["html", "filters", "markdown", "allowed tags", "editor"],
        ..page(
            Configuration,
            "Text formats",
            "/admin/config/formats",
            Permission("administer text formats"),
            "Which HTML each kind of text may contain, and who may use each format.",
        )
    },
    Link {
        also: &["/admin/embed"],
        synonyms: &[
            "search",
            "indexing",
            "embeddings",
            "semantic search",
            "pagefind",
        ],
        ..page(
            Configuration,
            "Search settings",
            "/admin/config/search",
            Permission("administer site"),
            "How the site's search works and what it covers.",
        )
    },
    Link {
        also: &["/admin/stage"],
        synonyms: &["workflow", "editorial", "drafts", "live", "publishing"],
        ..page(
            Configuration,
            "Stages",
            "/admin/structure/stages",
            Permission("administer site"),
            "The editorial stages content moves through before it is live.",
        )
    },
    Link {
        plugin: Some(REDIRECTS),
        synonyms: &["moved", "old urls", "legacy paths", "301", "forwarding"],
        ..page(
            Configuration,
            "Redirects",
            "/admin/config/redirects",
            Permission("view redirects"),
            "Old addresses that send visitors on to where the page lives now.",
        )
    },
    Link {
        synonyms: &[
            "password reset",
            "lost access",
            "recovery codes",
            "passkeys",
        ],
        ..page(
            Configuration,
            "Account recovery",
            "/admin/recovery",
            Permission("administer users"),
            "The ways people may get back into an account they have lost access to.",
        )
    },
    // --- Reports ---
    Link {
        status: Some(Status::LogErrors),
        icon: Some("log"),
        synonyms: &["errors", "watchdog", "messages", "logs", "warnings"],
        ..page(
            Reports,
            "Recent log messages",
            "/admin/reports/log",
            AnyPermission(&["access site reports", "administer site"]),
            "Errors, warnings and notices the site has recorded, newest first.",
        )
    },
    Link {
        plugin: Some(AUDIT_LOG),
        synonyms: &["history", "who changed", "trail", "security"],
        ..page(
            Reports,
            "Audit log",
            "/admin/reports/audit",
            Permission("view audit log"),
            "Who changed what, and when.",
        )
    },
    Link {
        synonyms: &[
            "requirements",
            "health",
            "versions",
            "system information",
            "security",
        ],
        ..page(
            Reports,
            "Status report",
            "/admin/reports/status",
            Permission("administer site"),
            "Versions, the database, the cache server, cron, the file system and security settings, checked now.",
        )
    },
    Link {
        status: Some(Status::CronLastRun),
        icon: Some("hourglass"),
        synonyms: &["scheduled tasks", "jobs", "queue", "background"],
        ..page(
            Reports,
            "Cron",
            "/admin/reports/cron",
            AnyPermission(&["run cron", "administer site"]),
            "Scheduled background tasks, when each last ran and whether it worked.",
        )
    },
    Link {
        synonyms: &["queries", "searches", "top searches", "no results"],
        ..page(
            Reports,
            "Search report",
            "/admin/reports/search",
            Permission("administer site"),
            "What visitors search for, and which searches find nothing.",
        )
    },
    Link {
        synonyms: &[
            "a11y",
            "alt text",
            "alternative text",
            "wcag",
            "screen reader",
        ],
        ..page(
            Reports,
            "Accessibility",
            "/admin/reports/accessibility",
            Permission("administer site"),
            "Images that lack a text alternative, and how this administration is checked.",
        )
    },
    Link {
        synonyms: &["dlq", "dead letter", "failed jobs", "retry", "discard"],
        ..page(
            Reports,
            "Dead jobs",
            "/admin/reports/dead-jobs",
            Permission("administer site"),
            "Plugin-queue jobs that failed every retry, kept so you can inspect, retry or discard them.",
        )
    },
];

/// A state a landing tile or dashboard card shows in one sentence: what
/// waits, or how something last went. Each is carried by exactly one
/// [`LINKS`] entry and computed by [`status_line`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Status {
    /// Content: items not published.
    Unpublished,
    /// Comments: comments waiting for moderation.
    CommentsWaiting,
    /// Users: blocked accounts, which include registrations awaiting
    /// verification or approval.
    BlockedAccounts,
    /// Recent log messages: errors in the last 24 hours.
    LogErrors,
    /// Cron: when it last ran.
    CronLastRun,
}

impl Status {
    /// Every status, once each.
    pub const ALL: &'static [Status] = &[
        Status::Unpublished,
        Status::CommentsWaiting,
        Status::BlockedAccounts,
        Status::LogErrors,
        Status::CronLastRun,
    ];
}

/// The statuses a Dashboard panel already reports (`admin_dashboard`):
/// "Waiting for you" counts the comments, "Recent errors" the errors, and
/// "Scheduled tasks" says how cron last went. The Dashboard's cards skip
/// them, so no state is said twice on one page.
pub const DASHBOARD_PANEL_STATUSES: &[Status] = &[
    Status::CommentsWaiting,
    Status::LogErrors,
    Status::CronLastRun,
];

/// One status, as a template draws it (`admin/macros/status.html`): a
/// sentence with at most one plural count, and whether it marks work
/// waiting, which draws the line itself as a badge.
#[derive(Debug, Clone, Serialize)]
pub struct StatusLine {
    pub message: crate::i18n::UiMessage,
    pub waiting: bool,
}

/// Whether `viewer` may see `status`: they pass the gate of the page that
/// carries it. A count must never reveal what the viewer may not open.
fn may_see(status: Status, viewer: &UserContext) -> bool {
    LINKS
        .iter()
        .find(|l| l.status == Some(status))
        .is_some_and(|l| l.gate.admits(viewer))
}

/// The line `status` shows `viewer` now, or `None` when they may not see it
/// or it cannot be read. Each count is one indexed statement, run in the
/// request's own task so the request profile counts it, and never cached:
/// a cleared queue shows as cleared at once. Cron's is one Redis read.
pub async fn status_line(
    status: Status,
    viewer: &UserContext,
    state: &crate::state::AppState,
) -> Option<StatusLine> {
    status_line_at(status, viewer, state, chrono::Utc::now().timestamp()).await
}

/// [`status_line`] as at `now` (Unix seconds), so a caller that lists what
/// a line counts can list it with the same cutoff.
pub async fn status_line_at(
    status: Status,
    viewer: &UserContext,
    state: &crate::state::AppState,
    now: i64,
) -> Option<StatusLine> {
    use crate::i18n::UiMessage;
    if !may_see(status, viewer) {
        return None;
    }
    let count = |sql: &'static str| sqlx::query_scalar::<_, i64>(sql);
    let failed = |e: sqlx::Error| {
        tracing::warn!(error = %e, ?status, "failed to count an admin status");
    };
    match status {
        Status::Unpublished => {
            // For anyone else the content list filters row by row, so a
            // total would count items they cannot open.
            if !viewer.can(super::admin_content::EDIT_ANY_CONTENT) {
                return None;
            }
            let n = count("SELECT COUNT(*) FROM item WHERE status = 0")
                .fetch_one(state.db())
                .await
                .map_err(failed)
                .ok()?;
            Some(StatusLine {
                message: UiMessage::new("{count} unpublished item")
                    .plural("{count} unpublished items", n),
                waiting: false,
            })
        }
        Status::CommentsWaiting => {
            let n = count("SELECT COUNT(*) FROM comment WHERE status = $1")
                .bind(crate::models::comment::CommentStatus::Pending.as_i16())
                .fetch_one(state.db())
                .await
                .map_err(failed)
                .ok()?;
            Some(StatusLine {
                message: UiMessage::new("{count} comment waiting for moderation")
                    .plural("{count} comments waiting for moderation", n),
                waiting: n > 0,
            })
        }
        Status::BlockedAccounts => {
            let n = count("SELECT COUNT(*) FROM users WHERE status = 0")
                .fetch_one(state.db())
                .await
                .map_err(failed)
                .ok()?;
            Some(StatusLine {
                message: UiMessage::new("{count} blocked account")
                    .plural("{count} blocked accounts", n),
                waiting: false,
            })
        }
        Status::LogErrors => {
            let since = now - 24 * 60 * 60;
            let n =
                count("SELECT COUNT(*) FROM system_log WHERE level = 'error' AND created >= $1")
                    .bind(since)
                    .fetch_one(state.db())
                    .await
                    .map_err(failed)
                    .ok()?;
            Some(StatusLine {
                message: UiMessage::new("{count} error in the last 24 hours")
                    .plural("{count} errors in the last 24 hours", n),
                waiting: n > 0,
            })
        }
        Status::CronLastRun => match state.cron().last_run().await {
            Err(e) => {
                tracing::warn!(error = %e, "failed to read the last cron run");
                None
            }
            // `cron:last_run` expires a day after the run it records.
            Ok(None) => Some(StatusLine {
                message: UiMessage::new("No run in the last 24 hours"),
                waiting: true,
            }),
            Ok(Some(run)) => Some(StatusLine {
                message: UiMessage::new("Last run {when} UTC")
                    .arg("when", super::admin_dashboard::when(run.timestamp)),
                waiting: false,
            }),
        },
    }
}

/// Whether `href` is a kernel page's own address, one of its `also`
/// prefixes, or a landing page: a plugin link there would tie with the
/// kernel's for the highlight, or shadow a page.
pub fn kernel_claims(href: &str) -> bool {
    LINKS
        .iter()
        .any(|l| l.href == href || l.also.contains(&href))
        || GROUPS.iter().any(|g| g.group.landing_href() == href)
}

/// A group's main action, drawn as a filled button beside a landing page's
/// heading and on its dashboard card.
#[derive(Debug, Clone, Serialize)]
pub struct PrimaryAction {
    pub label: String,
    pub href: String,
    /// Its resolved icon ([`Link::resolved_icon`]).
    pub icon: String,
}

/// A dashboard card as the template draws it: a group's chip, its title
/// linking to its landing page, its status lines and its main action; or a
/// plugin's card, with the Extend chip, its title linking to its page, and
/// its description.
#[derive(Debug, Clone, Serialize)]
pub struct DashboardCard {
    pub title: String,
    /// A plugin card's line; empty for a group card, whose landing page
    /// says what the group is for.
    pub description: String,
    /// The group's icon, or Extend's for a plugin card.
    pub icon: String,
    /// The group's landing page, or the plugin card's page.
    pub href: String,
    /// The group, for a group card; `None` for a plugin card.
    #[serde(skip)]
    pub group: Option<Group>,
    /// One line per status of the group's offered pages, except those a
    /// Dashboard panel reports ([`DASHBOARD_PANEL_STATUSES`]); filled by
    /// the handler, which awaits them.
    pub status_lines: Vec<StatusLine>,
    /// The group's main action, when the viewer may open it.
    pub primary: Option<PrimaryAction>,
}

/// Whether `link`'s page exists on this site and `viewer` may open it: the
/// one test every surface applies.
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

/// One page the viewer may open, from either source.
#[derive(Debug, Clone, Serialize)]
pub struct PageEntry {
    pub group: Option<Group>,
    pub label: String,
    pub description: String,
    pub href: String,
    /// Prefixes below which this entry is still the current page.
    #[serde(skip)]
    pub also: Vec<String>,
    pub synonyms: Vec<String>,
    pub sidebar: bool,
    /// The plugin that declared it, for a plugin page.
    pub plugin: Option<String>,
    /// Its icon: its own or its group's, resolved once here.
    pub icon: String,
    /// The state its tile shows; `None` for a plugin page.
    #[serde(skip)]
    pub status: Option<Status>,
    /// Whether it is its group's main action.
    #[serde(skip)]
    pub primary: bool,
}

/// Every page `viewer` may open: the kernel's, then the plugins', in
/// declaration order. The one filtered list every surface is built from.
pub fn offered_pages(
    viewer: &UserContext,
    enabled_plugins: &[String],
    plugin_links: &[PluginNavLink],
    plugin_cards: &[PluginNavCard],
) -> Vec<PageEntry> {
    let mut pages: Vec<PageEntry> = LINKS
        .iter()
        .filter(|l| offered(l, viewer, enabled_plugins))
        .map(|l| PageEntry {
            group: l.group,
            label: l.label.to_string(),
            description: l.description.to_string(),
            href: l.href.to_string(),
            also: l.also.iter().map(|s| s.to_string()).collect(),
            synonyms: l.synonyms.iter().map(|s| s.to_string()).collect(),
            sidebar: l.sidebar,
            plugin: None,
            icon: l.resolved_icon().to_string(),
            status: l.status,
            primary: l.primary,
        })
        .collect();
    pages.extend(
        plugin_links
            .iter()
            .filter(|l| offered_plugin(l, viewer, enabled_plugins))
            .map(|l| PageEntry {
                group: Some(group_for_section(&l.section)),
                label: l.label.clone(),
                description: plugin_cards
                    .iter()
                    .find(|c| c.plugin == l.plugin && c.href == l.href)
                    .map(|c| c.description.clone())
                    .unwrap_or_default(),
                href: l.href.clone(),
                also: Vec::new(),
                synonyms: std::iter::once(l.section.clone())
                    .chain(l.keywords.iter().cloned())
                    .collect(),
                sidebar: true,
                plugin: Some(l.plugin.clone()),
                icon: group_for_section(&l.section).info().icon.to_string(),
                status: None,
                primary: false,
            }),
    );
    pages
}

/// The main action of `group` among `pages`, if the viewer may open it.
fn primary_of(pages: &[PageEntry], group: Group) -> Option<PrimaryAction> {
    pages
        .iter()
        .find(|p| p.group == Some(group) && p.primary)
        .map(|p| PrimaryAction {
            label: p.label.clone(),
            href: p.href.clone(),
            icon: p.icon.clone(),
        })
}

/// The dashboard cards `viewer` may use: one per group with at least one
/// sidebar page the viewer may open (a plugin page without a card of its
/// own counts for its group), leading to its landing page, then one per
/// plugin-declared card, under the same rule, so the dashboard never
/// offers a page that answers 403 or 404. A group card no longer lists its
/// pages: its landing page does.
pub fn dashboard(
    viewer: &UserContext,
    enabled_plugins: &[String],
    plugin_links: &[PluginNavLink],
    plugin_cards: &[PluginNavCard],
) -> Vec<DashboardCard> {
    let pages: Vec<PageEntry> = offered_pages(viewer, enabled_plugins, &[], &[]);
    // A plugin page with a card of its own is offered by that card below. One
    // without a card is reached through its group's landing page, which lists
    // it, so it earns its group a card like any page in the group.
    let uncarded: Vec<PluginNavLink> = plugin_links
        .iter()
        .filter(|l| {
            !plugin_cards
                .iter()
                .any(|c| c.plugin == l.plugin && c.href == l.href)
        })
        .cloned()
        .collect();
    let reached: Vec<PageEntry> = offered_pages(viewer, enabled_plugins, &uncarded, &[])
        .into_iter()
        .filter(|p| p.plugin.is_some())
        .collect();
    let mut cards: Vec<DashboardCard> = GROUPS
        .iter()
        .filter(|g| {
            pages
                .iter()
                .chain(&reached)
                .any(|p| p.group == Some(g.group) && p.sidebar)
        })
        .map(|g| DashboardCard {
            title: g.title.to_string(),
            description: String::new(),
            icon: g.icon.to_string(),
            href: g.group.landing_href(),
            group: Some(g.group),
            status_lines: Vec::new(),
            primary: primary_of(&pages, g.group),
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
                icon: Group::Extend.info().icon.to_string(),
                href: card.href.clone(),
                group: None,
                status_lines: Vec::new(),
                primary: None,
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

/// A sidebar group (or, untitled, the Dashboard link above the groups), as
/// the template draws it.
#[derive(Debug, Clone, Serialize)]
pub struct NavSection {
    /// The group's title; `None` for the links above the first group.
    pub title: Option<String>,
    /// The group's slug, for the disclosure's id and remembered state.
    pub slug: Option<String>,
    /// The group's landing page.
    pub href: Option<String>,
    /// Whether the current page is the landing page.
    pub landing_active: bool,
    /// Whether the current page is in this group. Such a group is always
    /// open, and its state is never saved.
    pub current: bool,
    /// Whether the group is drawn open: it holds the current page, or the
    /// viewer opened it by hand ([`Preferences::expanded`]). Every other
    /// group is collapsed, so a first visit shows one group's links.
    pub open: bool,
    /// The group's icon; empty for the links above the first group.
    pub icon: String,
    pub links: Vec<NavLink>,
}

/// Whether `path` is `prefix` or below it.
fn under(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The page `path` belongs to among `pages`: the one whose address (or an
/// `also` prefix) is the longest prefix of `path`, so `/admin/content/files/3`
/// is Files rather than Content, and `/admin` itself only for the Dashboard.
/// On a tie the earlier entry keeps it: kernel entries come first, so a
/// plugin link can never take the highlight from one.
fn current_page<'a>(pages: &'a [PageEntry], path: &str) -> Option<&'a PageEntry> {
    pages
        .iter()
        .filter_map(|e| {
            std::iter::once(e.href.as_str())
                .chain(e.also.iter().map(String::as_str))
                .filter(|prefix| {
                    if *prefix == DASHBOARD {
                        path == DASHBOARD
                    } else {
                        under(path, prefix)
                    }
                })
                .map(str::len)
                .max()
                .map(|len| (len, e))
        })
        .fold(
            None,
            |best: Option<(usize, &PageEntry)>, candidate| match best {
                Some(b) if b.0 >= candidate.0 => Some(b),
                _ => Some(candidate),
            },
        )
        .map(|(_, e)| e)
}

/// The group whose landing page `path` is.
pub fn landing_group(path: &str) -> Option<Group> {
    let slug = path.strip_prefix("/admin/")?.strip_suffix("/overview")?;
    Group::from_slug(slug)
}

/// The sidebar for `viewer` at `path`: the Dashboard link, then each group
/// with at least one page the viewer may open (its sidebar pages, plugin
/// pages joining the group their section names), with the current page
/// marked active. A group with nothing left is dropped.
pub fn sidebar(
    viewer: &UserContext,
    enabled_plugins: &[String],
    path: &str,
    plugin_links: &[PluginNavLink],
) -> Vec<NavSection> {
    let pages = offered_pages(viewer, enabled_plugins, plugin_links, &[]);
    // A group's main action kept out of the sidebar (Add content type, Add
    // user) is the add form of the list above it, so that list is marked
    // current. Any other page kept out (Who can do this?, search,
    // preferences) marks no link: no shown link is the page.
    let active_href = current_page(&pages, path).map(|e| {
        if e.sidebar || !e.primary {
            return e.href.clone();
        }
        let shown: Vec<PageEntry> = pages.iter().filter(|p| p.sidebar).cloned().collect();
        current_page(&shown, &e.href).map_or_else(|| e.href.clone(), |p| p.href.clone())
    });
    let landing = landing_group(path);

    let nav = |e: &PageEntry| NavLink {
        label: e.label.clone(),
        href: e.href.clone(),
        active: active_href.as_deref() == Some(e.href.as_str()) && landing.is_none(),
    };

    let mut sections: Vec<NavSection> = Vec::new();
    let personal: Vec<NavLink> = pages
        .iter()
        .filter(|e| e.group.is_none() && e.sidebar)
        .map(nav)
        .collect();
    if !personal.is_empty() {
        sections.push(NavSection {
            title: None,
            slug: None,
            href: None,
            landing_active: false,
            current: false,
            open: false,
            icon: String::new(),
            links: personal,
        });
    }

    let current_group = landing.or_else(|| current_page(&pages, path).and_then(|e| e.group));
    for g in GROUPS {
        let links: Vec<NavLink> = pages
            .iter()
            .filter(|e| e.group == Some(g.group) && e.sidebar)
            .map(nav)
            .collect();
        if links.is_empty() {
            continue;
        }
        sections.push(NavSection {
            title: Some(g.title.to_string()),
            slug: Some(g.slug.to_string()),
            href: Some(g.group.landing_href()),
            landing_active: landing == Some(g.group),
            current: current_group == Some(g.group),
            open: current_group == Some(g.group),
            icon: g.icon.to_string(),
            links,
        });
    }
    sections
}

/// One tile of a group's landing page.
#[derive(Debug, Clone, Serialize)]
pub struct LandingItem {
    pub label: String,
    pub description: String,
    pub href: String,
    pub icon: String,
    /// The state the tile shows, if its page carries one.
    #[serde(skip)]
    pub status: Option<Status>,
    /// That state now, filled by the handler, which awaits it.
    pub status_line: Option<StatusLine>,
}

/// A group's landing page: a tile for every page in the group the viewer
/// may open, sidebar or not, kernel and plugin, except the group's main
/// action, which sits beside the heading as a button.
#[derive(Debug, Clone, Serialize)]
pub struct Landing {
    pub items: Vec<LandingItem>,
    pub primary: Option<PrimaryAction>,
}

impl Landing {
    /// Whether the viewer may open nothing in the group, which the route
    /// answers with 404.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.primary.is_none()
    }
}

/// The landing page of `group` for `viewer`.
pub fn landing(
    viewer: &UserContext,
    enabled_plugins: &[String],
    group: Group,
    plugin_links: &[PluginNavLink],
    plugin_cards: &[PluginNavCard],
) -> Landing {
    let pages: Vec<PageEntry> = offered_pages(viewer, enabled_plugins, plugin_links, plugin_cards)
        .into_iter()
        .filter(|e| e.group == Some(group))
        .collect();
    let primary = primary_of(&pages, group);
    let items = pages
        .into_iter()
        .filter(|e| !e.primary)
        .map(|e| LandingItem {
            label: e.label,
            description: e.description,
            href: e.href,
            icon: e.icon,
            status: e.status,
            status_line: None,
        })
        .collect();
    Landing { items, primary }
}

/// One breadcrumb.
#[derive(Debug, Clone, Serialize)]
pub struct Crumb {
    pub label: String,
    pub href: String,
}

/// Where `path` sits, for the shell: its group, the registry entry it belongs
/// to, the breadcrumbs above it, and its intro when the registry has one.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Placement {
    /// The group's title, for the `<title>` and the breadcrumb.
    pub group: Option<String>,
    /// The ancestors of this page, from Administration down; the page itself
    /// is the trail's last item, drawn by the template from its heading.
    pub crumbs: Vec<Crumb>,
    /// The registry description, when `path` is the entry's own address or
    /// a landing page.
    pub intro: Option<String>,
    /// The entry's title, when `path` is the entry's own address: the
    /// heading a template that sets none falls back to.
    pub title: Option<String>,
    /// Whether this is the Dashboard.
    pub dashboard: bool,
    /// The permissions that open the page (empty for administrator only),
    /// for "Who can do this?".
    pub permissions: Vec<String>,
    /// Whether the registry knows the page (the Dashboard, a landing page,
    /// or an entry or a page below one), so `permissions` is its gate.
    pub known: bool,
    /// The name of the registry entry the page belongs to.
    pub entry: Option<String>,
}

/// Where `path` sits for `viewer`. Computed from every kernel entry and
/// every plugin link, unfiltered: the trail of a page the viewer has opened
/// is not a list of offers, so it does not depend on their permissions.
pub fn placement(
    path: &str,
    plugin_links: &[PluginNavLink],
    plugin_cards: &[PluginNavCard],
) -> Placement {
    let everyone = UserContext::administrator(uuid::Uuid::nil(), Vec::new());
    let all_plugins: Vec<String> = plugin_links.iter().map(|l| l.plugin.clone()).collect();
    let mut enabled: Vec<String> = LINKS
        .iter()
        .filter_map(|l| l.plugin.map(str::to_string))
        .collect();
    enabled.extend(all_plugins);
    let pages = offered_pages(&everyone, &enabled, plugin_links, plugin_cards);

    let mut placement = Placement {
        dashboard: path == DASHBOARD,
        ..Placement::default()
    };
    if placement.dashboard {
        placement.title = Some("Dashboard".to_string());
        placement.intro = LINKS.first().map(|l| l.description.to_string());
        placement.permissions = vec!["access administration pages".to_string()];
        placement.known = true;
        placement.entry = placement.title.clone();
        return placement;
    }
    let root = Crumb {
        label: "Administration".to_string(),
        href: DASHBOARD.to_string(),
    };

    if let Some(group) = landing_group(path) {
        let info = group.info();
        placement.group = Some(info.title.to_string());
        placement.crumbs = vec![root];
        placement.intro = Some(info.description.to_string());
        placement.title = Some(info.title.to_string());
        placement.permissions = vec!["access administration pages".to_string()];
        placement.known = true;
        placement.entry = placement.title.clone();
        return placement;
    }

    placement.crumbs.push(root);
    if let Some(entry) = current_page(&pages, path) {
        placement.known = true;
        placement.entry = Some(entry.label.clone());
        if let Some(group) = entry.group {
            let info = group.info();
            placement.group = Some(info.title.to_string());
            placement.crumbs.push(Crumb {
                label: info.title.to_string(),
                href: group.landing_href(),
            });
        }
        if entry.href == path {
            if !entry.description.is_empty() {
                placement.intro = Some(entry.description.clone());
            }
            placement.title = Some(entry.label.clone());
        } else {
            placement.crumbs.push(Crumb {
                label: entry.label.clone(),
                href: entry.href.clone(),
            });
        }
        placement.permissions = match entry.plugin {
            Some(_) => plugin_links
                .iter()
                .find(|l| l.href == entry.href)
                .map(|l| vec![l.permission.clone()])
                .unwrap_or_default(),
            None => LINKS
                .iter()
                .find(|l| l.href == entry.href)
                .map(|l| {
                    l.gate
                        .permissions()
                        .into_iter()
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        };
    }
    placement
}

/// One admin search result.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub label: String,
    pub description: String,
    pub href: String,
    /// The group's title, `None` for a personal page.
    pub group: Option<String>,
}

/// The pages `viewer` may open that match `query`, best first: every word of
/// the query must appear in the page's title, description, group or
/// synonyms, in the source language or in `lang`. A title match ranks above
/// a description or synonym match.
pub fn search(
    viewer: &UserContext,
    enabled_plugins: &[String],
    plugin_links: &[PluginNavLink],
    plugin_cards: &[PluginNavCard],
    query: &str,
    translate: &dyn Fn(&str) -> String,
) -> Vec<SearchHit> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(u32, usize, SearchHit)> =
        offered_pages(viewer, enabled_plugins, plugin_links, plugin_cards)
            .into_iter()
            .enumerate()
            .filter_map(|(order, e)| {
                let group_title = e.group.map(|g| g.info().title.to_string());
                let title = format!("{} {}", e.label, translate(&e.label)).to_lowercase();
                let mut rest = format!("{} {}", e.description, translate(&e.description));
                if let Some(g) = &group_title {
                    rest.push(' ');
                    rest.push_str(g);
                    rest.push(' ');
                    rest.push_str(&translate(g));
                }
                for s in &e.synonyms {
                    rest.push(' ');
                    rest.push_str(s);
                }
                let rest = rest.to_lowercase();
                let mut score = 0u32;
                for w in &words {
                    if title.contains(w.as_str()) {
                        score += 3;
                    } else if rest.contains(w.as_str()) {
                        score += 1;
                    } else {
                        return None;
                    }
                }
                Some((
                    score,
                    order,
                    SearchHit {
                        label: e.label,
                        description: e.description,
                        href: e.href,
                        group: group_title,
                    },
                ))
            })
            .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, hit)| hit).collect()
}

/// The admin preferences a viewer has saved.
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Preferences {
    /// `system`, `light` or `dark`.
    #[serde(default)]
    pub theme: Option<String>,
    /// The admin language, when not the site default.
    #[serde(default)]
    pub language: Option<String>,
    /// The slugs of the sidebar groups the viewer opened by hand. A group
    /// holding the current page is open whatever this says, and every other
    /// group is collapsed unless named here. The `collapsed` list saved
    /// before B245 is ignored, so every viewer starts at that default.
    #[serde(default)]
    pub expanded: Vec<String>,
    /// Whether the viewer turned off the single-key `/` search shortcut
    /// (WCAG 2.1.4: a one-character shortcut can be turned off).
    #[serde(default)]
    pub search_slash_off: bool,
    /// The other key that opens search, if the viewer chose one: `ctrl-k`
    /// or `alt-k`.
    #[serde(default)]
    pub search_key: Option<String>,
}

impl Preferences {
    /// The other key that opens search: `ctrl-k`, `alt-k`, or empty.
    pub fn search_key(&self) -> &str {
        match self.search_key.as_deref() {
            Some("ctrl-k") => "ctrl-k",
            Some("alt-k") => "alt-k",
            _ => "",
        }
    }

    /// The colour theme, one of `system`, `light`, `dark`.
    pub fn theme(&self) -> &str {
        match self.theme.as_deref() {
            Some("light") => "light",
            Some("dark") => "dark",
            _ => "system",
        }
    }

    /// The saved preferences of `user`, default when none or unreadable.
    pub async fn load(pool: &sqlx::PgPool, user: uuid::Uuid) -> Self {
        let row: Option<(serde_json::Value,)> =
            sqlx::query_as("SELECT prefs FROM admin_preference WHERE user_id = $1")
                .bind(user)
                .fetch_optional(pool)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "failed to read admin preferences");
                    None
                });
        row.and_then(|(v,)| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    /// Store the fields in `changes` (a JSON object with some of `theme`,
    /// `language`, `search_slash_off`, `search_key`) as `user`'s, leaving the
    /// others as they are. The sidebar's opened groups are saved by
    /// [`Self::toggle_group`], never here.
    ///
    /// The merge happens in the one statement, not by reading the row, changing
    /// it and writing it all back: the preferences form and the sidebar's
    /// remembered groups are saved by different requests, and a whole-row write
    /// from one would put back the other's old value if both read first.
    pub async fn merge(
        pool: &sqlx::PgPool,
        user: uuid::Uuid,
        changes: serde_json::Value,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(changes.is_object(), "preference changes are a JSON object");
        sqlx::query(
            "INSERT INTO admin_preference (user_id, prefs, changed) VALUES ($1, $2, now()) \
             ON CONFLICT (user_id) DO UPDATE \
             SET prefs = admin_preference.prefs || EXCLUDED.prefs, changed = now()",
        )
        .bind(user)
        .bind(changes)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Record that `user` opened (`open`) or closed the sidebar group
    /// `slug`, leaving every other preference as it is.
    ///
    /// One statement, for the reason [`Self::merge`] gives: an upsert, so a
    /// viewer with no row gets one, that appends the slug to `expanded` when
    /// it is not there yet, or removes it, starting from an empty list when
    /// the key is absent so `expanded` is never null (a stored object that
    /// does not deserialize would lose every preference in [`Self::load`]).
    /// The `collapsed` list saved before B245 is dropped on the way.
    pub async fn toggle_group(
        pool: &sqlx::PgPool,
        user: uuid::Uuid,
        slug: &str,
        open: bool,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO admin_preference (user_id, prefs, changed) \
             VALUES ($1, jsonb_build_object('expanded', \
                 CASE WHEN $3 THEN jsonb_build_array($2::text) ELSE '[]'::jsonb END), now()) \
             ON CONFLICT (user_id) DO UPDATE SET prefs = jsonb_set( \
                 admin_preference.prefs - 'collapsed', '{expanded}', \
                 CASE \
                     WHEN NOT $3 THEN COALESCE(admin_preference.prefs->'expanded', '[]') - $2::text \
                     WHEN COALESCE(admin_preference.prefs->'expanded', '[]') ? $2::text \
                         THEN COALESCE(admin_preference.prefs->'expanded', '[]') \
                     ELSE COALESCE(admin_preference.prefs->'expanded', '[]') || to_jsonb($2::text) \
                 END), \
             changed = now()",
        )
        .bind(user)
        .bind(slug)
        .bind(open)
        .execute(pool)
        .await?;
        Ok(())
    }
}

/// The language the admin renders in for these preferences: the saved one
/// when the site knows it (or it is a pseudo locale and those are enabled),
/// else the site default.
pub fn admin_language(state: &crate::state::AppState, prefs: &Preferences) -> String {
    match prefs.language.as_deref() {
        Some(lang) if crate::i18n::is_pseudo(lang) && crate::i18n::pseudo_locales_enabled() => {
            lang.to_string()
        }
        Some(lang) if state.known_languages().iter().any(|l| l == lang) => lang.to_string(),
        _ => state.default_language().to_string(),
    }
}

/// What a render of the admin shell needs once the context is built.
#[derive(Debug, Clone)]
pub struct AdminShell {
    /// The language `t` translates into for this render.
    pub lang: String,
}

impl AdminShell {
    /// Render with `f` in this shell's language, then finish the page: the
    /// heading's text copied into the `<title>` and the breadcrumb.
    pub fn render(&self, f: impl FnOnce() -> tera::Result<String>) -> tera::Result<String> {
        crate::i18n::scope(&self.lang, f).map(|html| finish_page(&html))
    }
}

/// Build this viewer's admin shell for `path` into `context`: the sidebar
/// (`admin_nav`), the sorted `enabled_plugins` list, where the page sits
/// (`admin_place`), the viewer's preferences (`admin_prefs`), and the admin
/// language and direction (`admin_lang`, `admin_dir`).
///
/// This is the one step every admin page takes to get the shell — a kernel
/// route (`helpers::render_admin_template`), the themed error page
/// (`middleware::error_page`), and a plugin's own admin route
/// (`routes::plugin_api::themed_response`) alike — so the three cannot drift
/// apart. Before it existed the third did not build a sidebar at all, so a
/// plugin's admin page rendered `page--admin.html` with no sidebar (BL-162).
pub async fn insert_context(
    state: &crate::state::AppState,
    viewer: &UserContext,
    path: &str,
    context: &mut tera::Context,
) -> AdminShell {
    let mut enabled: Vec<String> = state.enabled_plugins().into_iter().collect();
    enabled.sort();
    let nav = state.plugin_admin_nav();
    let mut sections = sidebar(viewer, &enabled, path, nav.links());

    let prefs = if viewer.authenticated {
        Preferences::load(state.db(), viewer.id).await
    } else {
        Preferences::default()
    };
    for section in &mut sections {
        if let Some(slug) = &section.slug {
            // Open when the current page is in it or the viewer opened it.
            section.open = section.current || prefs.expanded.contains(slug);
        }
    }
    let lang = admin_language(state, &prefs);
    if let Some(locale) = state.locale() {
        locale.ensure_loaded(&lang).await;
    }

    context.insert("admin_nav", &sections);
    context.insert("enabled_plugins", &enabled);
    let place = placement(path, nav.links(), nav.cards());
    context.insert("admin_place", &place);
    context.insert("admin_prefs", &prefs);
    context.insert("admin_theme", prefs.theme());
    context.insert("admin_lang", &lang);
    context.insert(
        "admin_dir",
        crate::middleware::language::text_direction_for_language(&lang),
    );
    context.insert("admin_current_path", path);
    // "Who can do this?" for someone who may change who can: the page
    // itself recomputes the answer from the path.
    context.insert(
        "admin_who_can",
        &(viewer.can(super::admin_who_can::PERMISSION)
            && place.known
            && path != super::admin_who_can::PATH),
    );
    context.insert("admin_theme_stylesheets", &admin_theme_stylesheets(state));
    if !context.contains_key("site_name") {
        let site_name = crate::models::SiteConfig::site_name(state.db())
            .await
            .unwrap_or_else(|_| "Trovato".to_string());
        context.insert("site_name", &site_name);
    }
    AdminShell { lang }
}

/// The stylesheets of the theme chosen as the admin theme (B108): every
/// `.css` file in its `static/css/`, in name order, at the address the
/// theme's static files are served from. None for the stock theme.
///
/// Before B199 the admin page extended `base.html`, so an admin theme that
/// replaced `base.html` to add its stylesheet restyled the admin. The admin
/// shell is now its own document; linking the theme's stylesheets here keeps
/// that setting doing what it says.
fn admin_theme_stylesheets(state: &crate::state::AppState) -> Vec<String> {
    let engine = state.theme();
    let active = engine.active();
    let Some(theme) = engine.theme(&active.admin) else {
        return Vec::new();
    };
    let Some(css_dir) = theme.static_dir().map(|d| d.join("css")) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&css_dir) else {
        return Vec::new();
    };
    let mut files: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.ends_with(".css") && crate::theme::registry::is_safe_relative(name))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|f| format!("/static/themes/{}/css/{f}", theme.machine_name))
        .collect()
}

/// The marker `page--admin.html` writes where the page's heading text goes:
/// the `<title>` and the breadcrumb's last item.
pub const TITLE_MARKER: &str = "<!--trovato:admin-title-->";

/// Copy the text of the page's `h1` into every [`TITLE_MARKER`], so the
/// `<title>` and the breadcrumb always say what the heading says: the
/// template writes the heading once. With no `h1`, the site name stands in.
pub fn finish_page(html: &str) -> String {
    if !html.contains(TITLE_MARKER) {
        return html.to_string();
    }
    let heading = heading_text(html).unwrap_or_default();
    // A page showing a form's error summary leads its title with the
    // summary's own (translated) "Error:", so the title alone says the
    // submit failed.
    let prefix = error_prefix(html)
        .map(|p| format!("{p} "))
        .unwrap_or_default();
    html.replacen(ERROR_TITLE_MARKER, &prefix, 1)
        .replace(TITLE_MARKER, &heading)
}

/// Where the shell's `<title>` takes the error prefix.
pub const ERROR_TITLE_MARKER: &str = "<!--trovato:admin-error-->";

/// The prefix an error summary on the page carries
/// (`data-error-summary="Error:"`, `admin/macros/form.html`), still escaped.
fn error_prefix(html: &str) -> Option<String> {
    const ATTR: &str = "data-error-summary=\"";
    let start = html.find(ATTR)? + ATTR.len();
    let end = start + html[start..].find('"')?;
    let prefix = html[start..end].trim();
    (!prefix.is_empty()).then(|| prefix.to_string())
}

/// The text of the first `<h1>` in `html`, tags removed and whitespace
/// collapsed. Still HTML-escaped, which is what both places it goes want.
pub fn heading_text(html: &str) -> Option<String> {
    let start = html.find("<h1")?;
    let open_end = start + html[start..].find('>')? + 1;
    let close = open_end + html[open_end..].find("</h1>")?;
    let inner = &html[open_end..close];
    let mut text = String::with_capacity(inner.len());
    let mut in_tag = false;
    for c in inner.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!collapsed.is_empty()).then_some(collapsed)
}

/// Interface text a handler builds in Rust for an admin page (a form's
/// button, a line of markup in a form): the viewer's admin language on an
/// `/admin` page, and the English source anywhere else, so a form shared
/// with the front end reads there exactly as before.
pub struct AdminText {
    locale: Option<std::sync::Arc<crate::services::locale::LocaleService>>,
    /// The language `t` translates into, for [`crate::i18n::scope`] around a
    /// form render.
    pub lang: String,
}

impl AdminText {
    /// For `viewer` on `path`.
    pub async fn for_page(
        state: &crate::state::AppState,
        viewer: &UserContext,
        path: &str,
    ) -> Self {
        let lang = if (path == DASHBOARD || path.starts_with("/admin/")) && viewer.authenticated {
            let prefs = Preferences::load(state.db(), viewer.id).await;
            admin_language(state, &prefs)
        } else {
            "en".to_string()
        };
        let locale = state.locale().cloned();
        if let Some(locale) = &locale {
            locale.ensure_loaded(&lang).await;
        }
        Self { locale, lang }
    }

    /// `source` translated and escaped, placeholders filled from `args`.
    pub fn html(&self, source: &str, args: &[(&str, &str)]) -> String {
        crate::i18n::ui_html(self.locale.as_deref(), &self.lang, source, args)
    }

    /// Run a render (a form's) with `t` translating into this language.
    pub fn scope<R>(&self, f: impl FnOnce() -> R) -> R {
        crate::i18n::scope(&self.lang, f)
    }
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
            keywords: vec!["teams".to_string()],
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
        assert_eq!(
            hrefs(&nav),
            vec!["/admin", "/admin/content", "/admin/content/moderated"]
        );
        assert!(nav[1].links[0].active);
        assert_eq!(nav[1].title, Some("Content".to_string()));
        assert_eq!(nav[1].href.as_deref(), Some("/admin/content/overview"));
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
        assert_eq!(active("/admin/users/0198/sessions"), vec!["/admin/people"]);
        assert!(active("/admin/somewhere-else").is_empty());
        assert!(
            active("/admin/structure/overview").is_empty(),
            "a landing page marks its group, not a link"
        );
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

    fn card_hrefs(cards: &[DashboardCard]) -> Vec<String> {
        cards.iter().map(|c| c.href.clone()).collect()
    }

    fn card_titles(cards: &[DashboardCard]) -> Vec<String> {
        cards.iter().map(|c| c.title.clone()).collect()
    }

    #[test]
    fn the_dashboard_offers_only_what_the_viewer_may_open() {
        let editor = viewer(&["access administration pages", "edit any content"]);
        let cards = dashboard(&editor, &[gated("_comments")], &[], &[]);
        assert_eq!(card_titles(&cards), vec!["Content"]);
        assert_eq!(card_hrefs(&cards), vec!["/admin/content/overview"]);
        assert!(
            cards[0].primary.is_none(),
            "an editor who may create nothing is not offered Add content"
        );

        let moderator = viewer(&["access administration pages", "administer comments"]);
        assert!(dashboard(&moderator, &[], &[], &[]).is_empty());
        let cards = dashboard(&moderator, &[gated("_comments")], &[], &[]);
        assert_eq!(card_titles(&cards), vec!["Content"]);
        assert_eq!(card_hrefs(&cards), vec!["/admin/content/overview"]);

        let admin = UserContext::administrator(Uuid::now_v7(), vec![]);
        let all = dashboard(
            &admin,
            &[
                gated("_comments"),
                gated("_categories"),
                gated("_audit_log"),
            ],
            &[],
            &[],
        );
        assert_eq!(
            card_titles(&all),
            GROUPS.iter().map(|g| g.title).collect::<Vec<_>>(),
            "the administrator sees every group"
        );
        assert_eq!(
            card_hrefs(&all),
            GROUPS
                .iter()
                .map(|g| g.group.landing_href())
                .collect::<Vec<_>>()
        );
        for card in &all {
            assert_eq!(card.group.map(|g| g.info().icon), Some(card.icon.as_str()));
            assert!(card.description.is_empty(), "a group card lists nothing");
        }
    }

    // --- the registry (B199) ---

    #[test]
    fn every_kernel_page_is_in_a_group_except_the_personal_ones() {
        for l in LINKS {
            match l.href {
                DASHBOARD | PREFERENCES | SEARCH => assert!(l.group.is_none(), "{}", l.href),
                _ => assert!(l.group.is_some(), "{} has no group", l.href),
            }
            assert!(!l.description.is_empty(), "{} has no description", l.href);
            assert!(l.description.ends_with('.'), "{}: a sentence", l.href);
        }
    }

    #[test]
    fn hrefs_are_unique_and_no_landing_page_is_a_kernel_page() {
        let mut seen = std::collections::HashSet::new();
        for l in LINKS {
            assert!(seen.insert(l.href), "{} twice", l.href);
        }
        for g in GROUPS {
            assert!(!seen.contains(g.group.landing_href().as_str()));
            assert!(kernel_claims(&g.group.landing_href()));
        }
        assert!(GROUPS.len() <= 7, "at most seven groups");
    }

    #[test]
    fn plugin_sections_map_onto_groups() {
        assert_eq!(group_for_section("Structure"), Group::Structure);
        assert_eq!(group_for_section("people"), Group::People);
        assert_eq!(group_for_section("System"), Group::Configuration);
        assert_eq!(group_for_section("config"), Group::Configuration);
        assert_eq!(group_for_section("Widgets"), Group::Extend);
    }

    #[test]
    fn a_landing_page_lists_every_page_of_its_group_the_viewer_may_open() {
        let admin = UserContext::administrator(Uuid::now_v7(), vec![]);
        let people = landing(&admin, &[], Group::People, &[], &[]);
        let hrefs: Vec<&str> = people.items.iter().map(|i| i.href.as_str()).collect();
        assert_eq!(
            hrefs,
            vec![
                "/admin/people",
                "/admin/people/roles",
                "/admin/people/permissions",
                "/admin/people/who-can",
                "/admin/people/rules"
            ],
            "sidebar or not, and not the main action, which is no tile"
        );
        let primary = people.primary.expect("People's main action");
        assert_eq!(
            (primary.label.as_str(), primary.href.as_str()),
            ("Add user", "/admin/people/add"),
            "beside the heading"
        );
        assert_eq!(primary.icon, Group::People.info().icon);
        let users = &people.items[0];
        assert_eq!(users.icon, "users");
        assert_eq!(users.status, Some(Status::BlockedAccounts));
        assert!(users.status_line.is_none(), "filled by the handler");

        let editor = viewer(&["access administration pages", "edit any content"]);
        assert!(landing(&editor, &[], Group::People, &[], &[]).is_empty());

        // Someone who may only create content has a landing page holding
        // only the main action.
        let writer = viewer(&["access administration pages", "create article content"]);
        let content = landing(&writer, &[], Group::Content, &[], &[]);
        assert!(content.items.is_empty());
        assert!(!content.is_empty());
    }

    #[test]
    fn the_breadcrumb_runs_from_administration_through_the_group() {
        let p = placement("/admin/people/roles/7/edit", &[], &[]);
        let trail: Vec<(&str, &str)> = p
            .crumbs
            .iter()
            .map(|c| (c.label.as_str(), c.href.as_str()))
            .collect();
        assert_eq!(
            trail,
            vec![
                ("Administration", "/admin"),
                ("People", "/admin/people/overview"),
                ("Roles", "/admin/people/roles"),
            ]
        );
        assert_eq!(p.group.as_deref(), Some("People"));
        assert_eq!(p.intro, None, "a sub-page writes its own intro");

        let own = placement("/admin/people/roles", &[], &[]);
        assert_eq!(own.crumbs.len(), 2);
        assert_eq!(own.title.as_deref(), Some("Roles"));
        assert!(own.intro.is_some());
        assert_eq!(own.permissions, vec!["administer users"]);

        let landing = placement("/admin/structure/overview", &[], &[]);
        assert_eq!(landing.crumbs.len(), 1);
        assert_eq!(landing.group.as_deref(), Some("Structure"));

        assert!(placement("/admin", &[], &[]).dashboard);
        let stage = placement("/admin/structure/stages/3/edit", &[], &[]);
        assert_eq!(
            stage.group.as_deref(),
            Some("Configuration"),
            "by task, not URL"
        );
    }

    #[test]
    fn search_matches_titles_descriptions_and_synonyms_for_permitted_pages_only() {
        let admin = UserContext::administrator(Uuid::now_v7(), vec![]);
        let id = |s: &str| s.to_string();
        let hits = search(&admin, &[], &[], &[], "pathauto", &id);
        assert_eq!(hits[0].href, "/admin/config/pathauto");
        let hits = search(&admin, &[], &[], &[], "roles", &id);
        assert_eq!(hits[0].href, "/admin/people/roles", "title first");
        assert!(
            search(&admin, &[], &[], &[], "dark mode", &id)
                .iter()
                .any(|h| h.href == PREFERENCES)
        );
        let editor = viewer(&["access administration pages", "edit any content"]);
        assert!(search(&editor, &[], &[], &[], "roles", &id).is_empty());
        assert!(search(&admin, &[], &[], &[], "   ", &id).is_empty());
    }

    /// A plugin entry is found by its label, for a viewer holding its
    /// permission while its plugin is enabled, and not otherwise.
    #[test]
    fn search_finds_a_plugin_entry_only_when_offered() {
        let id = |s: &str| s.to_string();
        let links = [groups_link()];
        let found = |hits: Vec<SearchHit>| hits.iter().any(|h| h.href == "/admin/groups");
        let holder = viewer(&["access administration pages", "administer groups"]);
        let enabled = ["groups".to_string()];
        assert!(found(search(&holder, &enabled, &links, &[], "groups", &id)));
        assert!(
            !found(search(&holder, &[], &links, &[], "groups", &id)),
            "disabled"
        );
        let other = viewer(&["access administration pages"]);
        assert!(
            !found(search(&other, &enabled, &links, &[], "groups", &id)),
            "no permission"
        );
    }

    /// A plugin entry is also found by a keyword it declared through
    /// `AdminNavDefinition::with_keywords`, a word that is in neither its
    /// label nor its section, for a viewer holding its permission, and not
    /// otherwise.
    #[test]
    fn search_finds_a_plugin_entry_by_its_declared_keyword() {
        let id = |s: &str| s.to_string();
        let links = [groups_link()];
        let found = |hits: Vec<SearchHit>| hits.iter().any(|h| h.href == "/admin/groups");
        let holder = viewer(&["access administration pages", "administer groups"]);
        let enabled = ["groups".to_string()];
        assert!(found(search(&holder, &enabled, &links, &[], "teams", &id)));
        let other = viewer(&["access administration pages"]);
        assert!(
            !found(search(&other, &enabled, &links, &[], "teams", &id)),
            "no permission"
        );
    }

    #[test]
    fn the_heading_fills_the_title_and_the_breadcrumb() {
        let html = format!(
            "<title>{TITLE_MARKER}, People, Site</title><li aria-current=\"page\">{TITLE_MARKER}</li><h1 class=\"x\">Edit <span>role</span>\n  Editor</h1>"
        );
        let done = finish_page(&html);
        assert!(
            done.contains("<title>Edit role Editor, People, Site</title>"),
            "{done}"
        );
        assert!(done.contains("<li aria-current=\"page\">Edit role Editor</li>"));
        assert_eq!(finish_page("<p>no marker</p>"), "<p>no marker</p>");
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

    /// A plugin link whose href equals a prefix a kernel link claims (here
    /// Categories' `also`) ties with it on every page beneath; the kernel
    /// link keeps the highlight. The registry refuses such a declaration, so
    /// this is the second line.
    #[test]
    fn a_kernel_link_keeps_the_highlight_on_a_tie_with_a_plugin_link() {
        let holder = viewer(&[
            "access administration pages",
            "administer categories",
            "administer groups",
        ]);
        let tags = PluginNavLink {
            href: "/admin/structure/tags".to_string(),
            ..groups_link()
        };
        let nav = sidebar(
            &holder,
            &[CATEGORIES.to_string(), "groups".to_string()],
            "/admin/structure/tags/9/edit",
            &[tags],
        );
        let active: Vec<&str> = nav
            .iter()
            .flat_map(|s| s.links.iter().filter(|l| l.active).map(|l| l.href.as_str()))
            .collect();
        assert_eq!(active, vec!["/admin/structure/categories"]);
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
        assert_eq!(cards[0].group, None);
        assert!(cards[0].primary.is_none() && cards[0].status_lines.is_empty());

        assert!(
            dashboard(&granted, &[], &[groups_link()], &[groups_card()]).is_empty(),
            "disabling the plugin drops the card too"
        );
    }

    #[test]
    fn a_plugin_page_without_a_card_earns_its_group_a_card() {
        let granted = viewer(&["access administration pages", "administer groups"]);
        let cards = dashboard(&granted, &["groups".to_string()], &[groups_link()], &[]);
        assert_eq!(card_hrefs(&cards), vec!["/admin/structure/overview"]);
        assert_eq!(cards[0].group, Some(Group::Structure));
        assert!(
            cards[0].primary.is_none(),
            "the plugin page is not the group's main action"
        );
        let landing = landing(
            &granted,
            &["groups".to_string()],
            Group::Structure,
            &[groups_link()],
            &[],
        );
        assert!(
            landing.items.iter().any(|i| i.href == "/admin/groups"),
            "the card's landing page lists the plugin page"
        );

        let holder = viewer(&["access administration pages"]);
        assert!(
            dashboard(&holder, &["groups".to_string()], &[groups_link()], &[]).is_empty(),
            "nor without the plugin page's permission"
        );
        assert!(
            dashboard(&granted, &[], &[groups_link()], &[]).is_empty(),
            "nor with the plugin disabled"
        );
    }

    #[test]
    fn a_main_action_kept_out_of_the_sidebar_marks_the_list_it_adds_to() {
        let admin = UserContext::administrator(Uuid::now_v7(), vec![]);
        let active = |path: &str| -> Vec<String> {
            sidebar(&admin, &[], path, &[])
                .into_iter()
                .flat_map(|s| s.links)
                .filter(|l| l.active)
                .map(|l| l.href)
                .collect()
        };
        assert_eq!(
            active("/admin/structure/types/add"),
            vec!["/admin/structure/types"]
        );
        assert_eq!(active("/admin/people/add"), vec!["/admin/people"]);
        assert_eq!(
            active("/admin/structure/types"),
            vec!["/admin/structure/types"]
        );
        assert!(
            active("/admin/people/who-can").is_empty(),
            "a page kept out of the sidebar that adds to no list marks nothing"
        );
    }

    // --- icons (B245) ---

    #[test]
    fn every_group_and_page_icon_is_one_the_icon_set_draws() {
        for g in GROUPS {
            assert!(ICONS.contains(&g.icon), "{:?}: {}", g.group, g.icon);
        }
        for l in LINKS {
            if let Some(icon) = l.icon {
                assert!(ICONS.contains(&icon), "{}: {icon}", l.href);
            }
        }
    }

    #[test]
    fn a_page_without_its_own_icon_takes_its_groups_and_a_personal_page_none() {
        let add = LINKS
            .iter()
            .find(|l| l.href == "/admin/content/add")
            .unwrap();
        assert_eq!(add.resolved_icon(), Group::Content.info().icon);
        let users = LINKS.iter().find(|l| l.href == "/admin/people").unwrap();
        assert_eq!(users.resolved_icon(), "users");
        let dashboard = LINKS.iter().find(|l| l.href == DASHBOARD).unwrap();
        assert_eq!(dashboard.resolved_icon(), "");
        let admin = UserContext::administrator(Uuid::now_v7(), vec![]);
        let nav = sidebar(&admin, &["groups".to_string()], "/admin", &[groups_link()]);
        let pages = offered_pages(&admin, &["groups".to_string()], &[groups_link()], &[]);
        let plugin = pages.iter().find(|p| p.href == "/admin/groups").unwrap();
        assert_eq!(plugin.icon, Group::Structure.info().icon, "its group's");
        for section in &nav {
            match &section.slug {
                Some(slug) => assert_eq!(section.icon, Group::from_slug(slug).unwrap().info().icon),
                None => assert!(section.icon.is_empty()),
            }
        }
    }

    #[test]
    fn a_plugin_card_shows_the_extend_icon() {
        let granted = viewer(&["access administration pages", "administer groups"]);
        let cards = dashboard(
            &granted,
            &["groups".to_string()],
            &[groups_link()],
            &[groups_card()],
        );
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].icon, Group::Extend.info().icon);
    }

    fn render_icon(name: &str) -> String {
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../templates/admin/macros/icons.html"),
        )
        .unwrap();
        let mut tera = tera::Tera::default();
        tera.add_raw_templates(vec![
            ("admin/macros/icons.html", source.as_str()),
            (
                "probe.html",
                "{% import \"admin/macros/icons.html\" as icons %}{{ icons::icon(name=name) }}",
            ),
        ])
        .unwrap();
        let mut context = tera::Context::new();
        context.insert("name", name);
        tera.render("probe.html", &context).unwrap()
    }

    #[test]
    fn every_icon_renders_decorative_and_is_used_and_an_unknown_one_renders_nothing() {
        for name in ICONS {
            let svg = render_icon(name);
            assert!(
                svg.starts_with(
                    "<svg aria-hidden=\"true\" focusable=\"false\" class=\"admin-icon\""
                ),
                "{name}: {svg}"
            );
            assert!(
                ["<path", "<rect", "<circle"]
                    .iter()
                    .any(|s| svg.contains(s)),
                "{name} draws nothing: {svg}"
            );
            assert!(!svg.contains("<text"), "{name}: no text");
            assert!(!svg.contains("fill=\"currentColor\""), "{name}: no fill");
            let used = GROUPS.iter().any(|g| g.icon == *name)
                || LINKS.iter().any(|l| l.icon == Some(*name));
            assert!(used, "{name} is drawn but no group or page uses it");
        }
        assert_eq!(render_icon("no-such-icon").trim(), "");
    }

    // --- status lines and main actions (B245) ---

    #[test]
    fn every_status_is_carried_by_exactly_one_page() {
        for status in Status::ALL {
            let carriers: Vec<&str> = LINKS
                .iter()
                .filter(|l| l.status == Some(*status))
                .map(|l| l.href)
                .collect();
            assert_eq!(carriers.len(), 1, "{status:?} is on {carriers:?}");
        }
        for l in LINKS {
            if let Some(status) = l.status {
                assert!(Status::ALL.contains(&status), "{}: {status:?}", l.href);
            }
        }
    }

    /// A new `Status` fails to compile here until it is placed, and then
    /// fails until `Status::ALL` lists it, once.
    #[test]
    fn status_all_lists_every_variant_once() {
        let index = |s: Status| -> usize {
            match s {
                Status::Unpublished => 0,
                Status::CommentsWaiting => 1,
                Status::BlockedAccounts => 2,
                Status::LogErrors => 3,
                Status::CronLastRun => 4,
            }
        };
        let mut seen = [0usize; 5];
        for s in Status::ALL {
            seen[index(*s)] += 1;
        }
        assert_eq!(seen, [1; 5], "{:?}", Status::ALL);
        for s in DASHBOARD_PANEL_STATUSES {
            assert!(Status::ALL.contains(s));
        }
    }

    #[test]
    fn a_status_is_seen_only_by_whoever_may_open_its_page() {
        let editor = viewer(&["access administration pages", "edit any content"]);
        assert!(may_see(Status::Unpublished, &editor));
        assert!(!may_see(Status::CommentsWaiting, &editor));
        assert!(!may_see(Status::BlockedAccounts, &editor));
        assert!(!may_see(Status::LogErrors, &editor));
        assert!(!may_see(Status::CronLastRun, &editor));
        let reporter = viewer(&["access administration pages", "access site reports"]);
        assert!(may_see(Status::LogErrors, &reporter));
        assert!(!may_see(Status::CronLastRun, &reporter));
        let admin = UserContext::administrator(Uuid::now_v7(), vec![]);
        for s in Status::ALL {
            assert!(may_see(*s, &admin), "{s:?}");
        }
    }

    #[test]
    fn a_group_has_at_most_one_main_action() {
        for g in GROUPS {
            let primaries: Vec<&str> = LINKS
                .iter()
                .filter(|l| l.group == Some(g.group) && l.primary)
                .map(|l| l.href)
                .collect();
            assert!(primaries.len() <= 1, "{:?}: {primaries:?}", g.group);
        }
        assert!(
            LINKS
                .iter()
                .filter(|l| l.primary)
                .all(|l| l.group.is_some()),
            "a main action belongs to a group"
        );
        assert!(
            !LINKS
                .iter()
                .any(|l| l.group == Some(Group::Extend) && l.primary),
            "plugins are switched on from the Plugins page"
        );
    }

    #[test]
    fn each_main_action_asks_for_what_its_page_asks_for() {
        let gate = |href: &str| {
            LINKS
                .iter()
                .find(|l| l.href == href && l.primary)
                .unwrap_or_else(|| panic!("{href} is a main action"))
                .gate
        };
        assert!(matches!(gate("/admin/content/add"), Gate::CreatesContent));
        assert!(matches!(
            gate("/admin/people/add"),
            Gate::Permission("administer users")
        ));
        assert!(matches!(
            gate("/admin/structure/types/add"),
            Gate::Permission("administer site")
        ));
        let add_type = LINKS
            .iter()
            .find(|l| l.href == "/admin/structure/types/add")
            .unwrap();
        assert_eq!(add_type.group, Some(Group::Structure));
        assert!(!add_type.sidebar);
    }
}
