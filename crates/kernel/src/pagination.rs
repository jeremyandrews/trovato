//! The one pagination shape of the JSON API and the MCP tools.
//!
//! Every list the kernel answers with, over HTTP (`/api/v1/...`) or MCP, is a
//! [`Page`]: `{ "data": [...], "total": n, "page": p, "per_page": k }`. Every
//! list takes the same parameters, [`PageParams`]: `page` (1-based) and
//! `per_page`, with `limit` and `offset` accepted as their aliases for clients
//! written against the older search shapes.
//!
//! A list that is not paged at the source (the categories, a comment thread,
//! the registered gathers) is paged here, in memory, by [`Page::slice`], so a
//! client reads every list the same way.

use serde::{Deserialize, Serialize};

/// One page of a list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Page<T> {
    /// The entries on this page.
    pub data: Vec<T>,
    /// How many entries the whole list has. For a list an access check thins
    /// after the query, the size of the matching set, as the query counted it.
    pub total: u64,
    /// This page's number, from 1.
    pub page: i64,
    /// The page size asked for (after clamping), not the length of `data`.
    pub per_page: i64,
}

/// The page a request asks for: `page` and `per_page`, or `limit` and
/// `offset`.
///
/// `per_page` wins over `limit` and `page` over `offset` when both are given.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
pub struct PageParams {
    /// Page number, from 1.
    #[serde(default, deserialize_with = "lenient_i64")]
    pub page: Option<i64>,
    /// Entries per page.
    #[serde(default, deserialize_with = "lenient_i64")]
    pub per_page: Option<i64>,
    /// Alias of `per_page`.
    #[serde(default, deserialize_with = "lenient_i64")]
    pub limit: Option<i64>,
    /// Entries to skip; an alias of `page` counted in entries.
    #[serde(default, deserialize_with = "lenient_i64")]
    pub offset: Option<i64>,
}

/// A resolved window: the page number, its size and the offset of its first
/// entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// Page number, from 1.
    pub page: i64,
    /// Entries per page, between 1 and the list's maximum.
    pub per_page: i64,
    /// Entries before the first one on this page.
    pub offset: i64,
}

impl PageParams {
    /// The window these parameters ask for, `per_page` defaulting to
    /// `default` and clamped to `1..=max`.
    pub fn window(&self, default: i64, max: i64) -> Window {
        let per_page = self
            .per_page
            .or(self.limit)
            .unwrap_or(default)
            .clamp(1, max.max(1));
        match (self.page, self.offset) {
            (None, Some(offset)) => {
                let offset = offset.max(0);
                Window {
                    page: offset / per_page + 1,
                    per_page,
                    offset,
                }
            }
            (page, _) => {
                let page = page.unwrap_or(1).max(1);
                Window {
                    page,
                    per_page,
                    offset: (page - 1).saturating_mul(per_page),
                }
            }
        }
    }
}

impl<T> Page<T> {
    /// A page of entries the source already paged.
    pub fn new(data: Vec<T>, total: u64, window: Window) -> Self {
        Self {
            data,
            total,
            page: window.page,
            per_page: window.per_page,
        }
    }

    /// The window's page of a whole list.
    pub fn slice(all: Vec<T>, window: Window) -> Self {
        let total = all.len() as u64;
        let start = usize::try_from(window.offset).unwrap_or(usize::MAX);
        let take = usize::try_from(window.per_page).unwrap_or(0);
        let data = all.into_iter().skip(start).take(take).collect();
        Self::new(data, total, window)
    }

    /// A whole list as one page, for a list that takes no page parameters.
    pub fn all(data: Vec<T>) -> Self {
        let total = data.len() as u64;
        let per_page = i64::try_from(data.len()).unwrap_or(i64::MAX).max(1);
        Self {
            data,
            total,
            page: 1,
            per_page,
        }
    }

    /// An empty first page.
    pub fn empty(window: Window) -> Self {
        Self::new(Vec::new(), 0, window)
    }

    /// The same page with each entry mapped.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> Page<U> {
        Page {
            data: self.data.into_iter().map(f).collect(),
            total: self.total,
            page: self.page,
            per_page: self.per_page,
        }
    }
}

/// Accept a number or a numeric string: query strings carry strings, and a
/// JSON body (MCP) carries numbers.
fn lenient_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Value {
        Number(i64),
        Text(String),
    }
    match Option::<Value>::deserialize(deserializer)? {
        None => Ok(None),
        Some(Value::Number(n)) => Ok(Some(n)),
        Some(Value::Text(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::Text(s)) => s
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| serde::de::Error::custom(format!("not a number: {s}"))),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn params(
        page: Option<i64>,
        per_page: Option<i64>,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> PageParams {
        PageParams {
            page,
            per_page,
            limit,
            offset,
        }
    }

    #[test]
    fn defaults_and_clamps() {
        let w = PageParams::default().window(20, 100);
        assert_eq!((w.page, w.per_page, w.offset), (1, 20, 0));
        let w = params(Some(0), Some(500), None, None).window(20, 100);
        assert_eq!((w.page, w.per_page, w.offset), (1, 100, 0));
        let w = params(Some(3), Some(10), None, None).window(20, 100);
        assert_eq!((w.page, w.per_page, w.offset), (3, 10, 20));
    }

    #[test]
    fn limit_and_offset_are_aliases() {
        let w = params(None, None, Some(10), Some(25)).window(20, 100);
        assert_eq!((w.page, w.per_page, w.offset), (3, 10, 25));
        // per_page and page win over their aliases.
        let w = params(Some(2), Some(5), Some(10), Some(99)).window(20, 100);
        assert_eq!((w.page, w.per_page, w.offset), (2, 5, 5));
    }

    #[test]
    fn slice_pages_a_whole_list() {
        let w = params(Some(2), Some(2), None, None).window(20, 100);
        let page = Page::slice(vec![1, 2, 3, 4, 5], w);
        assert_eq!(page.data, vec![3, 4]);
        assert_eq!((page.total, page.page, page.per_page), (5, 2, 2));
        let past = Page::slice(vec![1], params(Some(9), None, None, None).window(20, 100));
        assert!(past.data.is_empty());
        assert_eq!(past.total, 1);
    }

    #[test]
    fn query_strings_and_json_numbers_both_parse() {
        let from_query: PageParams =
            serde_json::from_value(serde_json::json!({"page": "2", "per_page": "", "limit": 7}))
                .unwrap();
        assert_eq!(from_query, params(Some(2), None, Some(7), None));
        assert!(serde_json::from_value::<PageParams>(serde_json::json!({"page": "x"})).is_err());
    }

    #[test]
    fn the_envelope_has_four_keys() {
        let page = Page::slice(vec!["a"], PageParams::default().window(10, 10));
        let json = serde_json::to_value(&page).unwrap();
        let mut keys: Vec<&String> = json.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["data", "page", "per_page", "total"]);
    }
}
