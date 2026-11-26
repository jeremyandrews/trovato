//! Comment settings per content type (B159).
//!
//! Drupal sets comments per content type: open, closed (the thread is shown,
//! no new comments) or hidden (no thread, no form); threaded or flat; oldest
//! or newest first; and how many comments a page of the thread shows. A
//! Trovato type keeps the same four in its `settings`, read by the item page's
//! thread (`routes::comment::render_thread`) and by the comment route, which
//! refuses a comment on a type that is not open.
//!
//! A type that says nothing is open, threaded, oldest first, 50 per page:
//! what every type did before, except that the thread was never paged.

/// The type setting: `open`, `closed` or `hidden`.
pub const MODE: &str = "comment_mode";
/// The type setting: `threaded` or `flat`.
pub const THREADING: &str = "comment_threading";
/// The type setting: `oldest` or `newest` first.
pub const ORDER: &str = "comment_order";
/// The type setting: comments per page of the thread.
pub const PER_PAGE: &str = "comment_per_page";

/// Comments per page when a type does not say: Drupal's default.
pub const DEFAULT_PER_PAGE: usize = 50;
/// The most a page may show, so a setting cannot ask for an unbounded page.
pub const MAX_PER_PAGE: usize = 300;

/// Whether a type's items take comments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommentMode {
    /// The thread and the form.
    #[default]
    Open,
    /// The thread, and no new comments.
    Closed,
    /// Neither.
    Hidden,
}

impl CommentMode {
    /// Read a stored or submitted value; anything else is the default.
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("closed") => Self::Closed,
            Some("hidden") => Self::Hidden,
            _ => Self::Open,
        }
    }

    /// The stored value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Hidden => "hidden",
        }
    }
}

/// A type's comment settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommentSettings {
    /// Open, closed or hidden.
    pub mode: CommentMode,
    /// Replies under their parent (`true`), or every comment in date order.
    pub threaded: bool,
    /// Newest first (`true`), or oldest first. In a threaded thread this
    /// orders the threads; replies stay in the order they were written,
    /// under their parent.
    pub newest_first: bool,
    /// Comments per page of the thread.
    pub per_page: usize,
}

impl Default for CommentSettings {
    fn default() -> Self {
        Self {
            mode: CommentMode::Open,
            threaded: true,
            newest_first: false,
            per_page: DEFAULT_PER_PAGE,
        }
    }
}

impl CommentSettings {
    /// The settings a type's `settings` hold.
    pub fn of(settings: Option<&serde_json::Value>) -> Self {
        let text = |key: &str| {
            settings
                .and_then(|s| s.get(key))
                .and_then(serde_json::Value::as_str)
        };
        let per_page = settings
            .and_then(|s| s.get(PER_PAGE))
            .and_then(|v| v.as_u64().or_else(|| v.as_str()?.trim().parse().ok()));
        Self {
            mode: CommentMode::parse(text(MODE)),
            threaded: text(THREADING) != Some("flat"),
            newest_first: text(ORDER) == Some("newest"),
            per_page: Self::clamp_per_page(per_page),
        }
    }

    /// A per-page count, within 1 and [`MAX_PER_PAGE`]; none is the default.
    pub fn clamp_per_page(value: Option<u64>) -> usize {
        match value {
            None | Some(0) => DEFAULT_PER_PAGE,
            Some(n) => usize::try_from(n).unwrap_or(MAX_PER_PAGE).min(MAX_PER_PAGE),
        }
    }

    /// The settings as stored in a type's `settings`.
    pub fn to_settings(self) -> [(&'static str, serde_json::Value); 4] {
        [
            (MODE, self.mode.as_str().into()),
            (
                THREADING,
                if self.threaded { "threaded" } else { "flat" }.into(),
            ),
            (
                ORDER,
                if self.newest_first {
                    "newest"
                } else {
                    "oldest"
                }
                .into(),
            ),
            (PER_PAGE, (self.per_page as u64).into()),
        ]
    }
}

/// One item's own override of its type's `mode` (B239): `inherit` (the
/// default, defer to the type), or one of the type's own three. Held in
/// `item.comment_mode`, `NULL` for `inherit`, so every existing item is
/// `inherit` and nothing about it changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CommentOverride {
    /// Use the type's own `mode`.
    #[default]
    Inherit,
    /// The thread and the form, whatever the type says.
    Open,
    /// The thread, and no new comments, whatever the type says.
    Closed,
    /// Neither, whatever the type says.
    Hidden,
}

impl CommentOverride {
    /// Read a stored or submitted value; anything else (including `None`,
    /// the column's own default) is `Inherit`.
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("open") => Self::Open,
            Some("closed") => Self::Closed,
            Some("hidden") => Self::Hidden,
            _ => Self::Inherit,
        }
    }

    /// The value to store: `None` (a `NULL` column) for `Inherit`, which
    /// clears the override back to the type's own setting.
    pub fn as_stored(self) -> Option<&'static str> {
        match self {
            Self::Inherit => None,
            Self::Open => Some("open"),
            Self::Closed => Some("closed"),
            Self::Hidden => Some("hidden"),
        }
    }

    /// The item form's select value, `inherit` included.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Hidden => "hidden",
        }
    }
}

/// The item form's four choices for [`CommentOverride`]: value and English
/// label, translated by the select macro.
pub const COMMENT_OVERRIDE_OPTIONS: &[(&str, &str)] = &[
    ("inherit", "Use the content type's setting"),
    ("open", "Open"),
    ("closed", "Closed: shown, no new comments"),
    ("hidden", "Hidden: not shown, no new comments"),
];

/// The setting that actually governs one item: its own override if it has
/// one, otherwise its type's `mode`.
///
/// The one function every comment surface reads through (B239): the item
/// form shows it, every write route (the HTML form and the JSON API,
/// replies included) refuses a comment unless it answers `Open`, and the
/// thread, the comment listing API and the search index all omit comments
/// when it answers `Hidden`.
pub fn effective_mode(item_override: Option<&str>, type_mode: CommentMode) -> CommentMode {
    match CommentOverride::parse(item_override) {
        CommentOverride::Inherit => type_mode,
        CommentOverride::Open => CommentMode::Open,
        CommentOverride::Closed => CommentMode::Closed,
        CommentOverride::Hidden => CommentMode::Hidden,
    }
}

/// One page of a thread: which page is shown (1-based), how many there are,
/// and the range of the thread it covers.
///
/// A page past the end is the last page, as Drupal's comment pager does, so
/// a link to a page that a deletion emptied still shows the end of the
/// thread rather than nothing.
pub fn page_window(
    total: usize,
    per_page: usize,
    requested: Option<usize>,
) -> (usize, usize, std::ops::Range<usize>) {
    let per_page = per_page.max(1);
    let pages = total.div_ceil(per_page).max(1);
    let page = requested.unwrap_or(1).clamp(1, pages);
    let start = (page - 1) * per_page;
    let end = (start + per_page).min(total);
    (page, pages, start.min(total)..end)
}

/// The page (1-based) the comment at `index` in the thread is on.
pub fn page_of(index: usize, per_page: usize) -> usize {
    index / per_page.max(1) + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_type_that_says_nothing_is_open_threaded_oldest_first_fifty() {
        assert_eq!(CommentSettings::of(None), CommentSettings::default());
        assert_eq!(
            CommentSettings::of(Some(&serde_json::json!({}))),
            CommentSettings::default()
        );
        assert_eq!(CommentSettings::default().per_page, 50);
    }

    #[test]
    fn stored_settings_round_trip() {
        let settings = CommentSettings {
            mode: CommentMode::Hidden,
            threaded: false,
            newest_first: true,
            per_page: 30,
        };
        let stored: serde_json::Map<String, serde_json::Value> = settings
            .to_settings()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        assert_eq!(
            CommentSettings::of(Some(&serde_json::Value::Object(stored))),
            settings
        );
    }

    #[test]
    fn a_per_page_count_is_bounded() {
        assert_eq!(CommentSettings::clamp_per_page(Some(0)), DEFAULT_PER_PAGE);
        assert_eq!(CommentSettings::clamp_per_page(Some(100_000)), MAX_PER_PAGE);
        assert_eq!(
            CommentSettings::of(Some(&serde_json::json!({ PER_PAGE: "25" }))).per_page,
            25
        );
    }

    #[test]
    fn an_item_with_no_override_is_inherit() {
        assert_eq!(CommentOverride::parse(None), CommentOverride::Inherit);
        assert_eq!(CommentOverride::parse(Some("")), CommentOverride::Inherit);
        assert_eq!(
            CommentOverride::parse(Some("garbage")),
            CommentOverride::Inherit
        );
        assert_eq!(CommentOverride::Inherit.as_stored(), None);
    }

    #[test]
    fn an_override_round_trips_through_its_stored_value() {
        for (value, parsed) in [
            ("open", CommentOverride::Open),
            ("closed", CommentOverride::Closed),
            ("hidden", CommentOverride::Hidden),
        ] {
            assert_eq!(CommentOverride::parse(Some(value)), parsed);
            assert_eq!(parsed.as_stored(), Some(value));
            assert_eq!(parsed.as_str(), value);
        }
    }

    /// The resolver every comment surface reads through: an item's own
    /// override wins when it has one; `inherit` (every existing item) defers
    /// to the type.
    #[test]
    fn effective_mode_prefers_the_item_then_falls_back_to_the_type() {
        for type_mode in [CommentMode::Open, CommentMode::Closed, CommentMode::Hidden] {
            assert_eq!(effective_mode(None, type_mode), type_mode);
            assert_eq!(effective_mode(Some("inherit"), type_mode), type_mode);
            assert_eq!(effective_mode(Some("garbage"), type_mode), type_mode);
        }
        assert_eq!(
            effective_mode(Some("open"), CommentMode::Hidden),
            CommentMode::Open
        );
        assert_eq!(
            effective_mode(Some("closed"), CommentMode::Open),
            CommentMode::Closed
        );
        assert_eq!(
            effective_mode(Some("hidden"), CommentMode::Open),
            CommentMode::Hidden
        );
    }

    /// The groups discussion: 174 comments, 50 a page, is 50, 50, 50 and 24,
    /// and a page past the end is the last page.
    #[test]
    fn a_thread_of_174_pages_as_drupal_does() {
        assert_eq!(page_window(174, 50, None), (1, 4, 0..50));
        assert_eq!(page_window(174, 50, Some(2)), (2, 4, 50..100));
        assert_eq!(page_window(174, 50, Some(4)), (4, 4, 150..174));
        assert_eq!(page_window(174, 50, Some(10)), (4, 4, 150..174));
        assert_eq!(page_window(174, 50, Some(0)), (1, 4, 0..50));
        assert_eq!(page_window(0, 50, Some(3)), (1, 1, 0..0));
        assert_eq!(page_of(0, 50), 1);
        assert_eq!(page_of(49, 50), 1);
        assert_eq!(page_of(50, 50), 2);
    }
}
