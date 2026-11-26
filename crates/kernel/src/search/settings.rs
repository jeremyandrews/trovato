//! Site wide search settings: ranking signals, AI stage switches, analytics.
//!
//! One typed model, stored under one `site_config` key ([`SETTINGS_KEY`]), read
//! by everything that searches: the search page renders its client part into the
//! `scolta-config` block, [`SearchService::search`](super::SearchService::search)
//! applies the type boosts, and the AI search endpoints check their stage
//! switches. Config export carries it as part of the `variable` entity, like any
//! other `site_config` row.
//!
//! Reads go through the kernel's two tier cache ([`CacheLayer`]) under
//! [`SETTINGS_CACHE_KEY`], so a search request costs no database round trip;
//! [`SearchSettings::save`] drops the cached copy. A value written behind the
//! form's back is picked up when the cached copy expires, within
//! [`SETTINGS_CACHE_TTL_SECS`], or at once after `trovato config import` or
//! `trovato cache clear`, which drop it ([`crate::cache::clear_everywhere`]).
//!
//! Defaults are the literal values `templates/search.html` carried before this
//! model existed, so a site that never opens the screen ranks exactly as before.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::cache::CacheLayer;
use crate::models::SiteConfig;

/// The `site_config` key the settings are stored under.
pub const SETTINGS_KEY: &str = "search";

/// The cache key the settings are read through.
pub const SETTINGS_CACHE_KEY: &str = "search_settings";

/// How long a cached copy lives in the shared tier (seconds).
pub const SETTINGS_CACHE_TTL_SECS: u64 = 300;

/// The most priority pages a site may configure.
pub const MAX_PRIORITY_PAGES: usize = 50;

/// The longest a priority keyword may be, in characters.
pub const MAX_KEYWORD_CHARS: usize = 100;

/// The longest a priority page path may be, in characters.
pub const MAX_PATH_CHARS: usize = 255;

/// One priority page: a query containing `keyword` as a whole word puts `path`
/// first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriorityPage {
    /// The keyword, matched case insensitively as a whole word or phrase.
    pub keyword: String,
    /// The site path the keyword promotes, beginning with `/`.
    pub path: String,
}

/// Every search setting a site administrator controls.
///
/// `#[serde(default)]` so a stored value written by an older kernel, missing the
/// fields added since, still loads with defaults for the rest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchSettings {
    /// Largest recency boost, given to brand new content.
    pub recency_boost_max: f64,
    /// Days for the recency boost to halve.
    pub recency_half_life_days: f64,
    /// Age in days after which content starts to lose score.
    pub recency_penalty_after_days: f64,
    /// Largest age penalty, as a fraction of the score.
    pub recency_max_penalty: f64,
    /// Boost for a title that contains a query term.
    pub title_match_boost: f64,
    /// Multiplier on the title boost when the title contains every term.
    pub title_all_terms_multiplier: f64,
    /// Boost for an excerpt that contains a query term.
    pub content_match_boost: f64,
    /// Characters of excerpt shown per result.
    pub excerpt_length: u32,
    /// Results shown before "load more".
    pub results_per_page: u32,
    /// Most Pagefind results loaded and scored per query.
    pub max_pagefind_results: u32,
    /// How many top results the AI summary reads.
    pub ai_summary_top_n: u32,
    /// Most characters of result text sent to the AI summary.
    pub ai_summary_max_chars: u32,
    /// Item type machine name to score multiplier. A type not listed is 1.0.
    pub type_boosts: BTreeMap<String, f64>,
    /// Keyword to path promotions.
    pub priority_pages: Vec<PriorityPage>,
    /// The multiplier every priority page match gets.
    pub priority_boost: f64,
    /// Whether AI query expansion runs.
    pub expand: bool,
    /// Whether the AI summary runs.
    pub summarize: bool,
    /// Whether follow-up questions after a summary are accepted.
    pub followup: bool,
    /// Follow-up questions allowed after the summary.
    pub max_followups: u32,
    /// Whether search queries are counted for the search report.
    pub analytics_enabled: bool,
    /// Days of search analytics kept.
    pub analytics_retention_days: u32,
    /// Whether follow-up messages are classified by the AI provider.
    pub sentiment_enabled: bool,
    /// The static search index is rebuilt densely (compacted) once freed page
    /// slots, tombstones left by deleted or unpublished items that no new item
    /// has reused, are more than this percentage of a language's slots.
    /// Tombstones shift ranking slightly (`docs/design/incremental-search-index.md`,
    /// "What tombstones do to ranking"); a compaction rewrites every index file.
    pub index_compaction_percent: f64,
}

impl Default for SearchSettings {
    fn default() -> Self {
        Self {
            recency_boost_max: 0.5,
            recency_half_life_days: 365.0,
            recency_penalty_after_days: 1825.0,
            recency_max_penalty: 0.3,
            title_match_boost: 1.0,
            title_all_terms_multiplier: 1.5,
            content_match_boost: 0.4,
            excerpt_length: 300,
            results_per_page: 10,
            max_pagefind_results: 50,
            ai_summary_top_n: 5,
            ai_summary_max_chars: 2000,
            type_boosts: BTreeMap::new(),
            priority_pages: Vec::new(),
            priority_boost: 10.0,
            expand: true,
            summarize: true,
            followup: true,
            max_followups: 3,
            analytics_enabled: true,
            analytics_retention_days: 90,
            sentiment_enabled: false,
            index_compaction_percent: 5.0,
        }
    }
}

/// The inclusive range each number must fall in, with the label the form and
/// its error messages use. One table, so the validation, the form's `min` and
/// `max` attributes and the documentation cannot disagree.
pub struct NumberRange {
    /// Form field name, which is also the settings field name.
    pub field: &'static str,
    /// Human label.
    pub label: &'static str,
    /// Smallest value allowed.
    pub min: f64,
    /// Largest value allowed.
    pub max: f64,
}

/// Every numeric setting's range.
pub const NUMBER_RANGES: &[NumberRange] = &[
    range("recency_boost_max", "Recency boost, maximum", 0.0, 10.0),
    range(
        "recency_half_life_days",
        "Recency half life (days)",
        1.0,
        36500.0,
    ),
    range(
        "recency_penalty_after_days",
        "Age penalty starts after (days)",
        1.0,
        36500.0,
    ),
    range("recency_max_penalty", "Age penalty, maximum", 0.0, 1.0),
    range("title_match_boost", "Title match boost", 0.0, 100.0),
    range(
        "title_all_terms_multiplier",
        "All terms in title multiplier",
        0.0,
        100.0,
    ),
    range("content_match_boost", "Excerpt match boost", 0.0, 100.0),
    range(
        "excerpt_length",
        "Excerpt length (characters)",
        50.0,
        2000.0,
    ),
    range("results_per_page", "Results per page", 1.0, 100.0),
    range(
        "max_pagefind_results",
        "Results scored per query",
        1.0,
        500.0,
    ),
    range("ai_summary_top_n", "Results the summary reads", 1.0, 20.0),
    range(
        "ai_summary_max_chars",
        "Summary context (characters)",
        100.0,
        20000.0,
    ),
    range("priority_boost", "Priority page boost", 1.0, 100.0),
    range("max_followups", "Follow-up questions allowed", 0.0, 20.0),
    range(
        "analytics_retention_days",
        "Keep analytics for (days)",
        1.0,
        365.0,
    ),
    range(
        "index_compaction_percent",
        "Compact the search index above this percentage of freed slots",
        0.5,
        50.0,
    ),
];

/// The range every type boost must fall in.
pub const TYPE_BOOST_RANGE: (f64, f64) = (0.0, 100.0);

const fn range(field: &'static str, label: &'static str, min: f64, max: f64) -> NumberRange {
    NumberRange {
        field,
        label,
        min,
        max,
    }
}

/// The `scoring` object `static/js/scolta.js` reads (`getConfig`), in the
/// upper case keys it has always used.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub struct ClientScoring {
    /// See [`SearchSettings::recency_boost_max`].
    pub recency_boost_max: f64,
    /// See [`SearchSettings::recency_half_life_days`].
    pub recency_half_life_days: f64,
    /// See [`SearchSettings::recency_penalty_after_days`].
    pub recency_penalty_after_days: f64,
    /// See [`SearchSettings::recency_max_penalty`].
    pub recency_max_penalty: f64,
    /// See [`SearchSettings::title_match_boost`].
    pub title_match_boost: f64,
    /// See [`SearchSettings::title_all_terms_multiplier`].
    pub title_all_terms_multiplier: f64,
    /// See [`SearchSettings::content_match_boost`].
    pub content_match_boost: f64,
    /// See [`SearchSettings::excerpt_length`].
    pub excerpt_length: u32,
    /// See [`SearchSettings::results_per_page`].
    pub results_per_page: u32,
    /// See [`SearchSettings::max_pagefind_results`].
    pub max_pagefind_results: u32,
    /// See [`SearchSettings::expand`].
    pub ai_expand_query: bool,
    /// See [`SearchSettings::summarize`].
    pub ai_summarize: bool,
    /// See [`SearchSettings::ai_summary_top_n`].
    pub ai_summary_top_n: u32,
    /// See [`SearchSettings::ai_summary_max_chars`].
    pub ai_summary_max_chars: u32,
    /// Follow-ups the browser offers: [`SearchSettings::max_followups`], or 0
    /// when the follow-up stage is off.
    pub ai_max_followups: u32,
    /// See [`SearchSettings::type_boosts`].
    pub type_boosts: BTreeMap<String, f64>,
    /// The priority pages and their shared boost.
    pub priority_pages: ClientPriorityPages,
}

/// Priority pages as the browser reads them.
#[derive(Debug, Clone, Serialize)]
pub struct ClientPriorityPages {
    /// See [`SearchSettings::priority_boost`].
    pub boost: f64,
    /// See [`SearchSettings::priority_pages`].
    pub pages: Vec<PriorityPage>,
}

/// The whole `scolta-config` block the search page renders.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoltaConfig {
    /// CSS selector of the search container.
    pub container: &'static str,
    /// URL of the Pagefind client library.
    pub pagefind_path: &'static str,
    /// Site name, for the AI prompts' wording in the browser.
    pub site_name: String,
    /// The ranking and AI settings.
    pub scoring: ClientScoring,
    /// Text shown under an AI summary; empty for none.
    pub disclaimer: String,
    /// Whether the browser reports its searches to `POST /api/search/log`.
    pub analytics: bool,
}

impl SearchSettings {
    /// Load the settings: from the cache when it has them, otherwise from
    /// `site_config`, caching what was read.
    ///
    /// Never fails. A missing key is the defaults; a value that does not parse
    /// or does not validate is the defaults with a warning, so a bad import
    /// cannot take search down. A database error is the defaults too, and is
    /// not cached, so the next request tries again.
    pub async fn load(pool: &PgPool, cache: &CacheLayer) -> Self {
        if let Some(cached) = cache.get(SETTINGS_CACHE_KEY).await
            && let Ok(settings) = serde_json::from_str::<Self>(&cached)
        {
            return settings;
        }

        let settings = match SiteConfig::get(pool, SETTINGS_KEY).await {
            Ok(Some(value)) => Self::from_stored(value),
            Ok(None) => Self::default(),
            Err(e) => {
                tracing::warn!(error = %e, "failed to read search settings; using defaults");
                return Self::default();
            }
        };

        match serde_json::to_string(&settings) {
            Ok(payload) => {
                cache
                    .set(SETTINGS_CACHE_KEY, &payload, SETTINGS_CACHE_TTL_SECS, &[])
                    .await;
            }
            Err(e) => tracing::warn!(error = %e, "failed to serialize search settings for cache"),
        }

        settings
    }

    /// Interpret a stored value, falling back to the defaults with a warning.
    pub fn from_stored(value: serde_json::Value) -> Self {
        match serde_json::from_value::<Self>(value) {
            Ok(settings) => match settings.validate() {
                Ok(()) => settings,
                Err(errors) => {
                    tracing::warn!(
                        errors = ?errors,
                        "stored search settings are out of range; using defaults"
                    );
                    Self::default()
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "stored search settings do not parse; using defaults");
                Self::default()
            }
        }
    }

    /// Store the settings and drop the cached copy.
    ///
    /// The caller validates first; this refuses an invalid value rather than
    /// storing something [`load`](Self::load) would throw away.
    pub async fn save(&self, pool: &PgPool, cache: &CacheLayer) -> Result<()> {
        if let Err(errors) = self.validate() {
            anyhow::bail!("invalid search settings: {}", errors.join("; "));
        }
        let value = serde_json::to_value(self).context("serialize search settings")?;
        SiteConfig::set(pool, SETTINGS_KEY, value).await?;
        cache.invalidate(SETTINGS_CACHE_KEY).await;
        Ok(())
    }

    /// Check every value, returning every problem found.
    pub fn validate(&self) -> std::result::Result<(), Vec<String>> {
        let mut errors = Vec::new();

        // `numbers()` lists the fields in `NUMBER_RANGES` order (a unit test
        // holds the two together).
        for ((_, value), r) in self.numbers().into_iter().zip(NUMBER_RANGES) {
            if !value.is_finite() || value < r.min || value > r.max {
                errors.push(format!(
                    "{} must be a number from {} to {}.",
                    r.label, r.min, r.max
                ));
            }
        }

        let (min, max) = TYPE_BOOST_RANGE;
        for (item_type, boost) in &self.type_boosts {
            if !is_machine_name(item_type) {
                errors.push(format!("\"{item_type}\" is not an item type machine name."));
            } else if !boost.is_finite() || *boost < min || *boost > max {
                errors.push(format!(
                    "The boost for {item_type} must be a number from {min} to {max}."
                ));
            }
        }

        if self.priority_pages.len() > MAX_PRIORITY_PAGES {
            errors.push(format!(
                "At most {MAX_PRIORITY_PAGES} priority pages may be configured."
            ));
        }
        for page in &self.priority_pages {
            let keyword = page.keyword.trim();
            if keyword.is_empty() {
                errors.push(format!("The priority page {} needs a keyword.", page.path));
            } else if keyword.chars().count() > MAX_KEYWORD_CHARS {
                errors.push(format!(
                    "Priority keywords are at most {MAX_KEYWORD_CHARS} characters."
                ));
            }
            if !page.path.starts_with('/') {
                errors.push(format!("The path for \"{keyword}\" must start with \"/\"."));
            } else if page.path.starts_with("//")
                || page.path.chars().count() > MAX_PATH_CHARS
                || page.path.chars().any(char::is_whitespace)
            {
                errors.push(format!(
                    "The path for \"{keyword}\" must be a path on this site, without spaces, \
                     at most {MAX_PATH_CHARS} characters."
                ));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Every numeric setting by field name, as `f64`, for range checks and for
    /// the form, in [`NUMBER_RANGES`] order.
    pub fn numbers(&self) -> [(&'static str, f64); 16] {
        [
            ("recency_boost_max", self.recency_boost_max),
            ("recency_half_life_days", self.recency_half_life_days),
            (
                "recency_penalty_after_days",
                self.recency_penalty_after_days,
            ),
            ("recency_max_penalty", self.recency_max_penalty),
            ("title_match_boost", self.title_match_boost),
            (
                "title_all_terms_multiplier",
                self.title_all_terms_multiplier,
            ),
            ("content_match_boost", self.content_match_boost),
            ("excerpt_length", f64::from(self.excerpt_length)),
            ("results_per_page", f64::from(self.results_per_page)),
            ("max_pagefind_results", f64::from(self.max_pagefind_results)),
            ("ai_summary_top_n", f64::from(self.ai_summary_top_n)),
            ("ai_summary_max_chars", f64::from(self.ai_summary_max_chars)),
            ("priority_boost", self.priority_boost),
            ("max_followups", f64::from(self.max_followups)),
            (
                "analytics_retention_days",
                f64::from(self.analytics_retention_days),
            ),
            ("index_compaction_percent", self.index_compaction_percent),
        ]
    }

    /// The type boosts as two parallel arrays, for binding to SQL as
    /// `unnest($n::text[], $m::real[])`.
    pub fn type_boost_arrays(&self) -> (Vec<String>, Vec<f32>) {
        self.type_boosts
            .iter()
            .map(|(item_type, boost)| (item_type.clone(), *boost as f32))
            .unzip()
    }

    /// The part of the settings the browser reads.
    pub fn client_scoring(&self) -> ClientScoring {
        ClientScoring {
            recency_boost_max: self.recency_boost_max,
            recency_half_life_days: self.recency_half_life_days,
            recency_penalty_after_days: self.recency_penalty_after_days,
            recency_max_penalty: self.recency_max_penalty,
            title_match_boost: self.title_match_boost,
            title_all_terms_multiplier: self.title_all_terms_multiplier,
            content_match_boost: self.content_match_boost,
            excerpt_length: self.excerpt_length,
            results_per_page: self.results_per_page,
            max_pagefind_results: self.max_pagefind_results,
            ai_expand_query: self.expand,
            ai_summarize: self.summarize,
            ai_summary_top_n: self.ai_summary_top_n,
            ai_summary_max_chars: self.ai_summary_max_chars,
            ai_max_followups: if self.followup { self.max_followups } else { 0 },
            type_boosts: self.type_boosts.clone(),
            priority_pages: ClientPriorityPages {
                boost: self.priority_boost,
                pages: self.priority_pages.clone(),
            },
        }
    }

    /// The search page's whole `scolta-config` block.
    pub fn scolta_config(&self, site_name: &str) -> ScoltaConfig {
        ScoltaConfig {
            container: "#scolta-search",
            pagefind_path: "/static/pagefind/pagefind.js",
            site_name: site_name.to_string(),
            scoring: self.client_scoring(),
            disclaimer: String::new(),
            analytics: self.analytics_enabled,
        }
    }
}

/// Whether `name` is shaped like an item type machine name.
fn is_machine_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_old_template_literal() {
        let s = SearchSettings::default();
        let scoring = serde_json::to_value(s.client_scoring()).unwrap();

        // `templates/search.html` before the settings model existed.
        let old = serde_json::json!({
            "RECENCY_BOOST_MAX": 0.5,
            "RECENCY_HALF_LIFE_DAYS": 365.0,
            "RECENCY_PENALTY_AFTER_DAYS": 1825.0,
            "RECENCY_MAX_PENALTY": 0.3,
            "TITLE_MATCH_BOOST": 1.0,
            "TITLE_ALL_TERMS_MULTIPLIER": 1.5,
            "CONTENT_MATCH_BOOST": 0.4,
            "EXCERPT_LENGTH": 300,
            "RESULTS_PER_PAGE": 10,
            "MAX_PAGEFIND_RESULTS": 50,
            "AI_EXPAND_QUERY": true,
            "AI_SUMMARIZE": true,
            "AI_SUMMARY_TOP_N": 5,
            "AI_SUMMARY_MAX_CHARS": 2000,
            "AI_MAX_FOLLOWUPS": 3,
        });
        for (key, value) in old.as_object().unwrap() {
            assert_eq!(&scoring[key], value, "{key}");
        }
        assert!(s.validate().is_ok());
    }

    #[test]
    fn every_number_has_its_range_in_the_same_order() {
        let fields: Vec<&str> = SearchSettings::default()
            .numbers()
            .iter()
            .map(|(f, _)| *f)
            .collect();
        let ranged: Vec<&str> = NUMBER_RANGES.iter().map(|r| r.field).collect();
        assert_eq!(fields, ranged);
    }

    #[test]
    fn non_finite_and_out_of_range_numbers_are_refused() {
        let s = SearchSettings {
            recency_half_life_days: f64::NAN,
            recency_max_penalty: 2.0,
            ..SearchSettings::default()
        };
        let errors = s.validate().unwrap_err();
        assert_eq!(errors.len(), 2, "{errors:?}");
    }

    #[test]
    fn priority_pages_are_checked() {
        let page = |keyword: &str, path: &str| PriorityPage {
            keyword: keyword.to_string(),
            path: path.to_string(),
        };
        let ok = SearchSettings {
            priority_pages: vec![page("team", "/team")],
            ..SearchSettings::default()
        };
        assert!(ok.validate().is_ok());

        for bad in [
            page("", "/team"),
            page("team", "team"),
            page("team", "//evil.example"),
            page(&"k".repeat(101), "/team"),
        ] {
            let s = SearchSettings {
                priority_pages: vec![bad.clone()],
                ..SearchSettings::default()
            };
            assert!(s.validate().is_err(), "{bad:?}");
        }

        let s = SearchSettings {
            priority_pages: vec![page("team", "/team"); MAX_PRIORITY_PAGES + 1],
            ..SearchSettings::default()
        };
        assert!(s.validate().is_err());
    }

    #[test]
    fn type_boosts_are_checked() {
        let mut s = SearchSettings::default();
        s.type_boosts.insert("conference".to_string(), 2.0);
        assert!(s.validate().is_ok());
        s.type_boosts.insert("Bad Type".to_string(), 2.0);
        assert!(s.validate().is_err());
        s.type_boosts.remove("Bad Type");
        s.type_boosts.insert("page".to_string(), f64::INFINITY);
        assert!(s.validate().is_err());
    }

    #[test]
    fn a_stored_value_missing_fields_keeps_defaults_for_them() {
        let s = SearchSettings::from_stored(serde_json::json!({"expand": false}));
        assert!(!s.expand);
        assert_eq!(s.max_followups, 3);
    }

    #[test]
    fn an_unparsable_or_invalid_value_is_the_defaults() {
        assert_eq!(
            SearchSettings::from_stored(serde_json::json!("nonsense")),
            SearchSettings::default()
        );
        assert_eq!(
            SearchSettings::from_stored(serde_json::json!({"recency_max_penalty": 9})),
            SearchSettings::default()
        );
    }

    #[test]
    fn follow_ups_switched_off_offer_none() {
        let s = SearchSettings {
            followup: false,
            ..SearchSettings::default()
        };
        assert_eq!(s.client_scoring().ai_max_followups, 0);
    }

    #[test]
    fn type_boost_arrays_are_parallel() {
        let mut s = SearchSettings::default();
        s.type_boosts.insert("b".to_string(), 2.0);
        s.type_boosts.insert("a".to_string(), 0.5);
        let (types, boosts) = s.type_boost_arrays();
        assert_eq!(types, vec!["a", "b"]);
        assert_eq!(boosts, vec![0.5, 2.0]);
    }
}
