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
    pub fn sort<'a>(
        &self,
        allowed: &[(&'a str, &'a str)],
        default: (&'a str, bool),
    ) -> Sort<'a> {
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
            filtered: self
                .params
                .iter()
                .any(|(k, _)| k != "sort" && k != "page"),
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
/// column's sort text for a row. Text is compared without regard to case.
pub fn sort_rows<T>(rows: &mut [T], sort: &Sort<'_>, keys: &[(&str, fn(&T) -> String)]) {
    if let Some((_, key)) = keys.iter().find(|(k, _)| *k == sort.key) {
        rows.sort_by_cached_key(|row| key(row).to_lowercase());
        if sort.desc {
            rows.reverse();
        }
    }
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
        assert_eq!(s.sort(COLUMNS, ("changed", true)).order_by(), "changed DESC");
    }

    #[test]
    fn the_filter_is_a_literal_pattern() {
        assert_eq!(q(&[("q", " 50%_off ")]).like().as_deref(), Some("%50\\%\\_off%"));
        assert_eq!(q(&[("q", "  ")]).like(), None);
        assert!(q(&[("q", "about US")]).matches(&["About us", "page"]));
        assert!(!q(&[("q", "about them")]).matches(&["About us"]));
    }

    #[test]
    fn links_keep_the_rest_of_the_query() {
        let s = q(&[("q", "a b"), ("type", "page"), ("sort", "title"), ("page", "3")]);
        let sort = s.sort(COLUMNS, ("changed", true));
        let view = s.view(120, 20, Some(&sort));
        assert_eq!(view.query, "q=a+b&sort=title&type=page");
        assert_eq!(
            view.keep_for_sort,
            vec![("q".to_string(), "a b".to_string()), ("type".to_string(), "page".to_string())]
        );
        assert_eq!(view.pages, 3);
        assert_eq!(view.page, 3);
        assert!(view.filtered);
    }

    #[test]
    fn a_page_past_the_end_shows_the_last() {
        let s = q(&[("page", "99")]);
        assert_eq!(s.view(10, 10, None).page, 1);
        assert_eq!(q(&[("page", "-2")]).page, 1);
    }
}
