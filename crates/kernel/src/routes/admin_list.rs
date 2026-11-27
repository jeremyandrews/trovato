//! The admin listing pattern's query side: a text filter, a sort, a page.
//!
//! Every admin listing reads its query string through [`ListQuery`] and gives
//! its template a [`ListView`] under `list`. `templates/admin/macros/list.html`
//! draws the rest from it:
//! - the filter form keeps the sort;
//! - each sort button keeps the filters;
//! - each pager link keeps both;
//! - the result count says how many matched.
//!
//! All of it is GET, so it works without JavaScript and its addresses can be
//! shared.
//!
//! The words: `q` is the text filter, `sort` names a column, with a leading
//! `-` for descending, and `page` counts from 1. Other parameters (a type or
//! status select) belong to the listing and are kept as they are.

use std::collections::HashMap;

use serde::Serialize;

/// A sort expression in natural order: `natural!("lower(title)")` is
/// `lower(title) COLLATE trovato_natural`, so "Post 2" sorts before
/// "Post 10" (the collation is migration `20261211000001`). Only in sort
/// expressions, never a column's collation: the `q` filter's `ILIKE` and the
/// name prefix index depend on the default.
macro_rules! natural {
    ($e:literal) => {
        concat!($e, " COLLATE trovato_natural")
    };
}
pub(crate) use natural;

/// Rows per page on a paged admin listing.
pub const PER_PAGE: i64 = 50;

/// A listing's query: the text filter, the sort and the page.
#[derive(Debug, Clone)]
pub struct ListQuery {
    /// The text filter, trimmed; empty when there is none.
    pub q: String,
    /// The sort column's key, when the address names one.
    sort: Option<String>,
    /// Descending, when the sort parameter starts with `-`.
    desc: bool,
    /// The page, from 1.
    pub page: i64,
    /// Every parameter, for the links that keep them.
    params: Vec<(String, String)>,
}

impl ListQuery {
    /// Read the query from a handler's parameters.
    pub fn from_params(params: &HashMap<String, String>) -> Self {
        let q = params
            .get("q")
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let (sort, desc) = match params.get("sort").map(|s| s.trim()) {
            Some(s) if s.starts_with('-') && s.len() > 1 => (Some(s[1..].to_string()), true),
            Some(s) if !s.is_empty() => (Some(s.to_string()), false),
            _ => (None, false),
        };
        let page = params
            .get("page")
            .and_then(|p| p.parse::<i64>().ok())
            .filter(|p| *p >= 1)
            .unwrap_or(1);
        let mut params: Vec<(String, String)> = params
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        params.sort();
        Self {
            q,
            sort,
            desc,
            page,
            params,
        }
    }

    /// The text filter as a case-insensitive `ILIKE` pattern that matches
    /// anywhere, with `%`, `_` and `\` taken literally; `None` when there is
    /// no filter.
    pub fn like(&self) -> Option<String> {
        if self.q.is_empty() {
            return None;
        }
        let escaped = self
            .q
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        Some(format!("%{escaped}%"))
    }

    /// Whether `text` matches the text filter, for a listing that filters in
    /// memory: every word of the filter appears, ignoring case.
    pub fn matches(&self, texts: &[&str]) -> bool {
        if self.q.is_empty() {
            return true;
        }
        let haystack = texts.join(" ").to_lowercase();
        self.q
            .to_lowercase()
            .split_whitespace()
            .all(|word| haystack.contains(word))
    }

    /// The sort to use: the address's when it names one of `allowed`, else
    /// `default` (a key of `allowed` and whether it is descending). Returns
    /// the key, whether it is descending, and the column's SQL from `allowed`.
    /// Only the SQL in `allowed` ever reaches a query.
    pub fn sort<'a>(&self, allowed: &[(&'a str, &'a str)], default: (&'a str, bool)) -> Sort<'a> {
        if let Some(key) = &self.sort
            && let Some((k, sql)) = allowed.iter().find(|(k, _)| k == key)
        {
            return Sort {
                key: k,
                desc: self.desc,
                sql,
            };
        }
        let sql = allowed
            .iter()
            .find(|(k, _)| *k == default.0)
            .map(|(_, sql)| *sql)
            .unwrap_or(default.0);
        Sort {
            key: default.0,
            desc: default.1,
            sql,
        }
    }

    /// The row offset of the page.
    pub fn offset(&self) -> i64 {
        (self.page - 1) * PER_PAGE
    }

    /// The view the template draws, for `total` matching rows of which
    /// `shown` are on this page, sorted by `sort` (when the listing sorts).
    pub fn view(&self, total: i64, shown: usize, sort: Option<&Sort<'_>>) -> ListView {
        let pages = if total <= 0 {
            1
        } else {
            (total + PER_PAGE - 1) / PER_PAGE
        };
        let encode = |skip: &[&str]| -> String {
            let mut s = url::form_urlencoded::Serializer::new(String::new());
            for (k, v) in &self.params {
                if !skip.contains(&k.as_str()) {
                    s.append_pair(k, v);
                }
            }
            s.finish()
        };
        let keep = |skip: &[&str]| -> Vec<(String, String)> {
            self.params
                .iter()
                .filter(|(k, _)| !skip.contains(&k.as_str()))
                .cloned()
                .collect()
        };
        ListView {
            q: self.q.clone(),
            sort: sort.map(|s| s.key.to_string()).unwrap_or_default(),
            desc: sort.is_some_and(|s| s.desc),
            sort_param: sort
                .map(|s| format!("{}{}", if s.desc { "-" } else { "" }, s.key))
                .unwrap_or_default(),
            page: self.page.min(pages),
            pages,
            total,
            shown,
            filtered: self.params.iter().any(|(k, _)| k != "sort" && k != "page"),
            query: encode(&["page"]),
            keep_for_sort: keep(&["sort", "page"]),
        }
    }
}

/// A listing's sort: the column's key, its direction, and its SQL.
#[derive(Debug, Clone, Copy)]
pub struct Sort<'a> {
    pub key: &'a str,
    pub desc: bool,
    pub sql: &'a str,
}

impl Sort<'_> {
    /// `ORDER BY` text: the column's SQL and direction. The SQL comes from the
    /// listing's own allowed list, never from the address.
    pub fn order_by(&self) -> String {
        format!("{} {}", self.sql, if self.desc { "DESC" } else { "ASC" })
    }
}

/// What a listing's template draws its filter, sort, count and pager from.
#[derive(Debug, Clone, Serialize)]
pub struct ListView {
    /// The text filter as typed.
    pub q: String,
    /// The sorted column's key; empty when the listing does not sort.
    pub sort: String,
    /// Whether the sort is descending.
    pub desc: bool,
    /// The sort as the address writes it (`title` or `-title`).
    pub sort_param: String,
    /// The page shown, and how many there are.
    pub page: i64,
    pub pages: i64,
    /// Rows that match, and rows on this page.
    pub total: i64,
    pub shown: usize,
    /// Whether any filter or sort is in the address.
    pub filtered: bool,
    /// The query without the page, for pager links.
    pub query: String,
    /// The parameters a sort button keeps: everything but the sort and page.
    pub keep_for_sort: Vec<(String, String)>,
}

/// Sort `rows` in memory by the key `sort` names; `keys` gives each allowed
/// column's sort text for a row. Text is compared in natural order, as the
/// SQL listings sort through `natural!`: without regard to case or
/// accents, with each run of digits compared as the number it spells
/// ("Post 2" before "Post 10"). Rows whose natural keys are equal ("Éclair"
/// and "eclair", "Post 02" and "Post 2") are ordered by the bytes of their
/// lowercased text, as Postgres breaks a tie under a deterministic
/// collation. Zero padded numbers (a weight, a count) order as before.
pub fn sort_rows<T>(rows: &mut [T], sort: &Sort<'_>, keys: &[(&str, fn(&T) -> String)]) {
    if let Some((_, key)) = keys.iter().find(|(k, _)| *k == sort.key) {
        rows.sort_by_cached_key(|row| {
            let lower = key(row).to_lowercase();
            (natural_key(&lower), lower)
        });
        if sort.desc {
            rows.reverse();
        }
    }
}

/// `text`'s natural sort key, as a string whose byte order is the natural
/// order: non-ASCII letters transliterated (`deunicode`) and lowercased, and
/// each run of digits written as `0`, its length (after leading zeros) in
/// five digits, then its digits, so numbers compare by length, then by
/// digit. ASCII is kept as it is, so a separator such as `\0` between two
/// numbers stays one.
pub fn natural_key(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() {
            plain.push(c);
        } else if let Some(ascii) = deunicode::deunicode_char(c) {
            plain.push_str(ascii);
        }
    }
    let plain = plain.to_lowercase();
    let mut out = String::with_capacity(plain.len() + 8);
    let mut digits = String::new();
    let flush = |digits: &mut String, out: &mut String| {
        if digits.is_empty() {
            return;
        }
        let trimmed = digits.trim_start_matches('0');
        let number = if trimmed.is_empty() { "0" } else { trimmed };
        out.push('0');
        out.push_str(&format!("{:05}", number.len()));
        out.push_str(number);
        digits.clear();
    };
    for c in plain.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
        } else {
            flush(&mut digits, &mut out);
            out.push(c);
        }
    }
    flush(&mut digits, &mut out);
    out
}

/// How loudly a listing shows a row's state (docs/admin/design.md, "Icons
/// and status"): only a state that needs someone's attention stands out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusKind {
    /// Something failed or was refused: `--danger-surface`.
    Problem,
    /// Something waits for a person: `--warning-surface`.
    Attention,
    /// The ordinary state: a quiet neutral pill.
    Done,
}

/// Every state an admin listing shows for a row, in one place, so the same
/// state looks the same on every page and the common one (Published) is
/// never as loud as the rare one (Blocked). [`ListState::kind`] is the one
/// place a state's kind is set; `list::status(label, kind)` draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListState {
    Published,
    Unpublished,
    PendingReview,
    Spam,
    Active,
    Blocked,
    Enabled,
    Disabled,
    NotInstalled,
    Permanent,
    Temporary,
    Allow,
    Deny,
    LogError,
    LogWarn,
    LogInfo,
}

impl ListState {
    /// Every state, once.
    pub const ALL: [ListState; 16] = [
        ListState::Published,
        ListState::Unpublished,
        ListState::PendingReview,
        ListState::Spam,
        ListState::Active,
        ListState::Blocked,
        ListState::Enabled,
        ListState::Disabled,
        ListState::NotInstalled,
        ListState::Permanent,
        ListState::Temporary,
        ListState::Allow,
        ListState::Deny,
        ListState::LogError,
        ListState::LogWarn,
        ListState::LogInfo,
    ];

    /// The English word the listings show (the source a template passes to
    /// `t`): the words they showed before, so existing translations match.
    pub fn label(self) -> &'static str {
        match self {
            ListState::Published => "Published",
            ListState::Unpublished => "Unpublished",
            ListState::PendingReview => "Pending review",
            ListState::Spam => "Spam",
            ListState::Active => "Active",
            ListState::Blocked => "Blocked",
            ListState::Enabled => "Enabled",
            ListState::Disabled => "Disabled",
            ListState::NotInstalled => "Not installed",
            ListState::Permanent => "Permanent",
            ListState::Temporary => "Temporary",
            ListState::Allow => "Allow",
            ListState::Deny => "Deny",
            // The site log has always shown the stored level.
            ListState::LogError => "error",
            ListState::LogWarn => "warn",
            ListState::LogInfo => "info",
        }
    }

    /// How loudly the state is shown.
    pub fn kind(self) -> StatusKind {
        match self {
            ListState::Blocked => StatusKind::Problem,
            ListState::Spam => StatusKind::Problem,
            ListState::LogError => StatusKind::Problem,
            ListState::Unpublished => StatusKind::Attention,
            ListState::PendingReview => StatusKind::Attention,
            ListState::Temporary => StatusKind::Attention,
            ListState::LogWarn => StatusKind::Attention,
            ListState::Published => StatusKind::Done,
            ListState::Active => StatusKind::Done,
            ListState::Enabled => StatusKind::Done,
            ListState::Disabled => StatusKind::Done,
            ListState::NotInstalled => StatusKind::Done,
            ListState::Permanent => StatusKind::Done,
            ListState::Allow => StatusKind::Done,
            ListState::Deny => StatusKind::Done,
            ListState::LogInfo => StatusKind::Done,
        }
    }

    /// An item's `status` (1 published, anything else not).
    pub fn of_item(status: i16) -> Self {
        if status == 1 {
            ListState::Published
        } else {
            ListState::Unpublished
        }
    }

    /// A comment's status.
    pub fn of_comment(status: crate::models::comment::CommentStatus) -> Self {
        use crate::models::comment::CommentStatus;
        match status {
            CommentStatus::Unpublished => ListState::Unpublished,
            CommentStatus::Published => ListState::Published,
            CommentStatus::Pending => ListState::PendingReview,
            CommentStatus::Spam => ListState::Spam,
        }
    }

    /// An account's `status` (1 active, anything else blocked).
    pub fn of_account(status: i16) -> Self {
        if status == 1 {
            ListState::Active
        } else {
            ListState::Blocked
        }
    }

    /// A managed file's status.
    pub fn of_file(status: crate::file::service::FileStatus) -> Self {
        use crate::file::service::FileStatus;
        match status {
            FileStatus::Permanent => ListState::Permanent,
            FileStatus::Temporary => ListState::Temporary,
        }
    }

    /// Something that is on or off (an AI provider).
    pub fn of_enabled(enabled: bool) -> Self {
        if enabled {
            ListState::Enabled
        } else {
            ListState::Disabled
        }
    }

    /// A plugin: installed and enabled, installed and disabled, or not
    /// installed.
    pub fn of_plugin(installed: bool, enabled: bool) -> Self {
        match (installed, enabled) {
            (true, true) => ListState::Enabled,
            (true, false) => ListState::Disabled,
            (false, _) => ListState::NotInstalled,
        }
    }

    /// An access rule's effect.
    pub fn of_rule(status: crate::services::access_rule::RuleStatus) -> Self {
        use crate::services::access_rule::RuleStatus;
        match status {
            RuleStatus::Allow => ListState::Allow,
            RuleStatus::Deny => ListState::Deny,
        }
    }

    /// A site log entry's stored level: `error`, `warn` or `info` (anything
    /// else is shown as `info`, as the log's level sort treats it).
    pub fn of_log_level(level: &str) -> Self {
        match level {
            "error" => ListState::LogError,
            "warn" => ListState::LogWarn,
            _ => ListState::LogInfo,
        }
    }

    /// What a template draws: `{{ list::status(label=t(s=state.label), kind=state.kind) }}`.
    pub fn view(self) -> StateView {
        StateView {
            label: self.label(),
            kind: self.kind(),
        }
    }
}

/// A row's state as a template reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StateView {
    /// The English word, for `t`.
    pub label: &'static str,
    /// `problem`, `attention` or `done`.
    pub kind: StatusKind,
}

/// The rows of one page of an in-memory listing.
pub fn page_of<T: Clone>(rows: &[T], query: &ListQuery) -> Vec<T> {
    let start = usize::try_from(query.offset()).unwrap_or(0);
    rows.iter()
        .skip(start)
        .take(usize::try_from(PER_PAGE).unwrap_or(50))
        .cloned()
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn q(pairs: &[(&str, &str)]) -> ListQuery {
        ListQuery::from_params(
            &pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        )
    }

    const COLUMNS: &[(&str, &str)] = &[("title", "lower(title)"), ("changed", "changed")];

    #[test]
    fn only_an_allowed_column_reaches_the_sql() {
        let s = q(&[("sort", "-title")]);
        let sort = s.sort(COLUMNS, ("changed", true));
        assert_eq!(sort.order_by(), "lower(title) DESC");
        let s = q(&[("sort", "id; DROP TABLE item")]);
        assert_eq!(
            s.sort(COLUMNS, ("changed", true)).order_by(),
            "changed DESC"
        );
    }

    #[test]
    fn the_filter_is_a_literal_pattern() {
        assert_eq!(
            q(&[("q", " 50%_off ")]).like().as_deref(),
            Some("%50\\%\\_off%")
        );
        assert_eq!(q(&[("q", "  ")]).like(), None);
        assert!(q(&[("q", "about US")]).matches(&["About us", "page"]));
        assert!(!q(&[("q", "about them")]).matches(&["About us"]));
    }

    #[test]
    fn links_keep_the_rest_of_the_query() {
        let s = q(&[
            ("q", "a b"),
            ("type", "page"),
            ("sort", "title"),
            ("page", "3"),
        ]);
        let sort = s.sort(COLUMNS, ("changed", true));
        let view = s.view(120, 20, Some(&sort));
        assert_eq!(view.query, "q=a+b&sort=title&type=page");
        assert_eq!(
            view.keep_for_sort,
            vec![
                ("q".to_string(), "a b".to_string()),
                ("type".to_string(), "page".to_string())
            ]
        );
        assert_eq!(view.pages, 3);
        assert_eq!(view.page, 3);
        assert!(view.filtered);
    }

    /// Every state has its word and its kind, and only failures and work
    /// that waits stand out.
    #[test]
    fn every_list_state_maps_to_its_word_and_kind() {
        for state in ListState::ALL {
            assert!(!state.label().is_empty(), "{state:?}");
            let expected = match state.label() {
                "Blocked" | "Spam" | "error" => StatusKind::Problem,
                "Unpublished" | "Pending review" | "Temporary" | "warn" => StatusKind::Attention,
                _ => StatusKind::Done,
            };
            assert_eq!(state.kind(), expected, "{state:?}");
            assert_eq!(state.view().label, state.label());
            assert_eq!(state.view().kind, state.kind());
        }
        assert_eq!(
            serde_json::to_value(ListState::Spam.view()).unwrap(),
            serde_json::json!({ "label": "Spam", "kind": "problem" })
        );
        assert_eq!(ListState::of_log_level("warn"), ListState::LogWarn);
        assert_eq!(ListState::of_item(1), ListState::Published);
        assert_eq!(ListState::of_account(0), ListState::Blocked);
        assert_eq!(ListState::of_plugin(false, true), ListState::NotInstalled);
    }

    /// `ListState::ALL` lists every variant exactly once: the match below
    /// has no `_` arm, so a new variant fails to compile here until it is
    /// added to `ALL` too.
    #[test]
    fn list_state_all_holds_every_variant_once() {
        fn index(state: ListState) -> usize {
            match state {
                ListState::Published => 0,
                ListState::Unpublished => 1,
                ListState::PendingReview => 2,
                ListState::Spam => 3,
                ListState::Active => 4,
                ListState::Blocked => 5,
                ListState::Enabled => 6,
                ListState::Disabled => 7,
                ListState::NotInstalled => 8,
                ListState::Permanent => 9,
                ListState::Temporary => 10,
                ListState::Allow => 11,
                ListState::Deny => 12,
                ListState::LogError => 13,
                ListState::LogWarn => 14,
                ListState::LogInfo => 15,
            }
        }
        let mut seen = [0usize; 16];
        for state in ListState::ALL {
            seen[index(state)] += 1;
        }
        assert_eq!(seen, [1; 16]);
    }

    /// Natural order: numbers by value, case and accents ignored, ties by
    /// the lowercased bytes.
    #[test]
    fn rows_sort_in_natural_order() {
        let mut rows: Vec<String> = [
            "Post 10", "Zebra", "post 1", "Éclair", "Post 2", "eclair", "Post 02",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let sort = Sort {
            key: "name",
            desc: false,
            sql: "name",
        };
        sort_rows(&mut rows, &sort, &[("name", |r: &String| r.clone())]);
        assert_eq!(
            rows,
            [
                "eclair", "Éclair", "post 1", "Post 02", "Post 2", "Post 10", "Zebra"
            ]
        );
        // "Post 1: …" before "Post 10".
        let mut titles = vec!["Post 10: b".to_string(), "Post 1: a".to_string()];
        sort_rows(&mut titles, &sort, &[("name", |r: &String| r.clone())]);
        assert_eq!(titles, ["Post 1: a", "Post 10: b"]);
        // Zero padded numbers keep their order, and a separator between two
        // numbers is kept.
        assert!(natural_key("040001\u{0}9") > natural_key("040000\u{0}99"));
        assert!(natural_key("000000\u{0}a") < natural_key("039999\u{0}a"));
        assert_eq!(natural_key("Æble 007"), natural_key("aeble 7"));
    }

    #[test]
    fn a_page_past_the_end_shows_the_last() {
        let s = q(&[("page", "99")]);
        assert_eq!(s.view(10, 10, None).page, 1);
        assert_eq!(q(&[("page", "-2")]).page, 1);
    }
}
