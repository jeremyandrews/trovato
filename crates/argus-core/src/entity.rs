//! Entity normalization and fuzzy alias resolution (M2 extract step).
//!
//! The analyze stage returns entities as free text from a model, so the same
//! organisation arrives as "OpenAI", "Open AI", "OpenAI Inc." and "openai"
//! across a week of feeds. This module turns those into one canonical row.
//!
//! Two layers:
//!
//! 1. **Normalization** ([`match_key`]) — a deterministic key used for the
//!    unique constraint and for narrowing candidates in SQL. Exact key equality
//!    is the fast path and needs no scoring at all.
//! 2. **Fuzzy resolution** ([`resolve`]) — for keys that do not match exactly,
//!    two criteria that must *both* hold ([`is_alias`]): Jaro-Winkler above a
//!    threshold, and a Levenshtein distance inside a length-scaled allowance.
//!    Where they hold, the incoming spelling is recorded as an alias of the
//!    existing row; where they do not, a new entity is created. Names shorter
//!    than [`MIN_FUZZY_LEN`] never take this path at all.
//!
//! Both distance functions are implemented here rather than pulled from a
//! crate: forty lines of string distance is not worth widening the
//! dependency-audit surface.
//!
//! Nothing here does I/O. [`resolve`] produces a [`EntityPlan`] of actions that
//! the store port applies, so the whole decision layer is unit-testable.

use std::collections::BTreeSet;

/// Entity kinds the analyze prompt asks for. Anything else the model invents
/// is folded into [`EntityType::Other`] rather than rejected — a mislabelled
/// entity is still a useful clustering signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntityType {
    /// A named individual.
    Person,
    /// A company, organisation, agency or institution.
    Company,
    /// A geographic place.
    Place,
    /// A named event.
    Event,
    /// A named technology, product or standard.
    Technology,
    /// Anything the model returned with an unrecognized type.
    Other,
}

impl EntityType {
    /// The value persisted in the `entity_type` column.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            EntityType::Person => "person",
            EntityType::Company => "company",
            EntityType::Place => "place",
            EntityType::Event => "event",
            EntityType::Technology => "technology",
            EntityType::Other => "other",
        }
    }

    /// Parse a model-supplied (or column) type string, tolerating plurals,
    /// casing, whitespace and the common synonyms models reach for.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        let k = raw.trim().to_lowercase();
        // Try the word as given, then its singular: "companies" and "company"
        // must land on the same variant, and a naive trailing-`s` strip turns
        // the former into "companie".
        match Self::exact(&k) {
            EntityType::Other => Self::exact(&singular(&k)),
            found => found,
        }
    }

    /// Match one already-lowercased word against the known type vocabulary.
    fn exact(k: &str) -> Self {
        match k {
            "person" | "people" | "individual" | "human" => EntityType::Person,
            "company" | "organization" | "organisation" | "org" | "corporation" | "business"
            | "institution" | "agency" => EntityType::Company,
            "place" | "location" | "country" | "city" | "region" | "geography" => EntityType::Place,
            "event" | "incident" | "conference" => EntityType::Event,
            "technology" | "tech" | "product" | "standard" | "protocol" | "software" => {
                EntityType::Technology
            }
            _ => EntityType::Other,
        }
    }
}

/// The singular of an English plural, for the type vocabulary only.
///
/// Handles the two forms that matter here — `-ies` → `-y` and a trailing `-s` —
/// and leaves everything else alone. It is not a general stemmer and does not
/// need to be: it only ever sees a handful of known type words.
fn singular(word: &str) -> String {
    if let Some(stem) = word.strip_suffix("ies") {
        return format!("{stem}y");
    }
    word.strip_suffix('s').unwrap_or(word).to_string()
}

/// Leading words dropped when building a match key, so "The Guardian" and
/// "Guardian" resolve to one row.
const LEADING_ARTICLES: &[&str] = &["the", "a", "an", "la", "le", "il", "el"];

/// Trailing corporate suffixes dropped when building a match key.
const CORPORATE_SUFFIXES: &[&str] = &[
    "inc",
    "incorporated",
    "llc",
    "ltd",
    "limited",
    "corp",
    "corporation",
    "co",
    "plc",
    "gmbh",
    "sa",
    "srl",
    "spa",
    "ag",
    "nv",
    "bv",
    "ab",
    "oy",
    "as",
];

/// Longest entity name accepted. A model that returns a sentence where a name
/// was asked for should not create a row keyed on that sentence.
pub const MAX_ENTITY_NAME_LEN: usize = 120;

/// Default Jaro-Winkler threshold for treating an incoming spelling as an
/// alias of an existing entity. Tunable via `argus.entity_match_threshold`.
///
/// It is one of the two criteria in [`is_alias`], not the whole test. Raising
/// it splits entities; lowering it merges unrelated ones, which is the more
/// damaging error, because a wrongly merged entity distorts every article
/// vector it appears in and so distorts clustering itself.
pub const DEFAULT_MATCH_THRESHOLD: f64 = 0.92;

/// Clean up a model-supplied entity name for display.
///
/// Collapses whitespace, trims surrounding punctuation and quotes, and caps the
/// length. Returns `None` for anything left empty, numeric-only, or over
/// [`MAX_ENTITY_NAME_LEN`] after cleaning.
#[must_use]
pub fn clean_name(raw: &str) -> Option<String> {
    let collapsed: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed
        .trim_matches(|c: char| c.is_whitespace() || "\"'`.,;:!?()[]{}<>*_".contains(c))
        .to_string();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_ENTITY_NAME_LEN {
        return None;
    }
    if trimmed.chars().all(|c| !c.is_alphabetic()) {
        return None;
    }
    Some(trimmed)
}

/// The normalized lookup key for an entity name.
///
/// Lowercases, drops every non-alphanumeric character (so "Open A.I." and
/// "OpenAI" agree), removes a leading article and a trailing corporate suffix.
/// Returns an empty string only for input with no alphanumeric content.
#[must_use]
pub fn match_key(name: &str) -> String {
    let lowered = name.to_lowercase();
    let mut words: Vec<String> = lowered
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();

    if words.len() > 1 && LEADING_ARTICLES.contains(&words[0].as_str()) {
        words.remove(0);
    }
    while words.len() > 1 && CORPORATE_SUFFIXES.contains(&words[words.len() - 1].as_str()) {
        words.pop();
    }
    words.concat()
}

/// An entity as the analyze stage extracted it, before resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedEntity {
    /// Display name exactly as the model wrote it.
    pub name: String,
    /// Kind, as parsed from the model's `type` field.
    pub entity_type: EntityType,
}

/// A cleaned, keyed entity ready for resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedEntity {
    /// Display form.
    pub canonical_name: String,
    /// Lookup key.
    pub match_key: String,
    /// Kind.
    pub entity_type: EntityType,
}

/// An existing entity row, as the store hands candidates to the resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityRecord {
    /// Row id (uuid string).
    pub id: String,
    /// Stored display form.
    pub canonical_name: String,
    /// Stored lookup key.
    pub match_key: String,
    /// Stored kind.
    pub entity_type: EntityType,
}

/// One resolution outcome for one extracted entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityAction {
    /// The entity already exists; link the article to it.
    Link {
        /// Existing row id.
        entity_id: String,
        /// A spelling not yet recorded on that row, to append to `aliases`.
        new_alias: Option<String>,
    },
    /// No existing entity matched; create one and link to it.
    Create {
        /// Display form to store.
        canonical_name: String,
        /// Lookup key to store.
        match_key: String,
        /// Kind to store.
        entity_type: EntityType,
    },
}

/// The full set of actions for one article's extracted entities.
pub type EntityPlan = Vec<EntityAction>;

/// Normalize and de-duplicate the entities extracted from one article.
///
/// Duplicates within a single article (the model naming "OpenAI" three times,
/// or naming both "OpenAI" and "Open AI") collapse to one entry, so the
/// article-to-entity link table never needs to absorb the repetition.
#[must_use]
pub fn normalize_all(extracted: &[ExtractedEntity]) -> Vec<NormalizedEntity> {
    let mut seen: BTreeSet<(String, EntityType)> = BTreeSet::new();
    let mut out = Vec::new();
    for e in extracted {
        let Some(name) = clean_name(&e.name) else {
            continue;
        };
        let key = match_key(&name);
        if key.is_empty() {
            continue;
        }
        if !seen.insert((key.clone(), e.entity_type)) {
            continue;
        }
        out.push(NormalizedEntity {
            canonical_name: name,
            match_key: key,
            entity_type: e.entity_type,
        });
    }
    out
}

/// Jaro similarity in `[0, 1]`.
#[must_use]
pub fn jaro(a: &str, b: &str) -> f64 {
    let x: Vec<char> = a.chars().collect();
    let y: Vec<char> = b.chars().collect();
    if x.is_empty() && y.is_empty() {
        return 1.0;
    }
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }

    // Characters may only match within this window of their counterpart.
    let window = (x.len().max(y.len()) / 2).saturating_sub(1);
    let mut x_matched = vec![false; x.len()];
    let mut y_matched = vec![false; y.len()];
    let mut matches = 0usize;

    for (i, xc) in x.iter().enumerate() {
        let lo = i.saturating_sub(window);
        let hi = (i + window + 1).min(y.len());
        for j in lo..hi {
            if y_matched[j] || y[j] != *xc {
                continue;
            }
            x_matched[i] = true;
            y_matched[j] = true;
            matches += 1;
            break;
        }
    }
    if matches == 0 {
        return 0.0;
    }

    // Half the number of matched characters that appear out of order.
    let mut transpositions = 0usize;
    let mut j = 0usize;
    for (i, matched) in x_matched.iter().enumerate() {
        if !matched {
            continue;
        }
        while !y_matched[j] {
            j += 1;
        }
        if x[i] != y[j] {
            transpositions += 1;
        }
        j += 1;
    }

    let m = matches as f64;
    let t = (transpositions / 2) as f64;
    (m / x.len() as f64 + m / y.len() as f64 + (m - t) / m) / 3.0
}

/// Jaro-Winkler similarity in `[0, 1]`: Jaro with a bonus for a shared prefix.
#[must_use]
pub fn jaro_winkler(a: &str, b: &str) -> f64 {
    let base = jaro(a, b);
    if base < 0.7 {
        return base;
    }
    let prefix = a
        .chars()
        .zip(b.chars())
        .take(4)
        .take_while(|(x, y)| x == y)
        .count() as f64;
    base + prefix * 0.1 * (1.0 - base)
}

/// Levenshtein edit distance.
#[must_use]
pub fn levenshtein(a: &str, b: &str) -> usize {
    let x: Vec<char> = a.chars().collect();
    let y: Vec<char> = b.chars().collect();
    if x.is_empty() {
        return y.len();
    }
    if y.is_empty() {
        return x.len();
    }
    let mut prev: Vec<usize> = (0..=y.len()).collect();
    let mut cur = vec![0usize; y.len() + 1];
    for (i, xc) in x.iter().enumerate() {
        cur[0] = i + 1;
        for (j, yc) in y.iter().enumerate() {
            let cost = usize::from(xc != yc);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[y.len()]
}

/// Shortest match key eligible for fuzzy resolution.
///
/// Below this, keys resolve by exact equality only. This is the single most
/// important guard in the module: a one-character difference in a five-letter
/// name is as likely to be a *different* company as a typo ("Intel" and
/// "Intex"), and every fuzzy measure scores such a pair high. Short names are
/// also the ones `match_key` normalization already handles well, since their
/// realistic variation is punctuation and casing rather than spelling.
pub const MIN_FUZZY_LEN: usize = 8;

/// How many edits are tolerable in a key of `len` characters.
///
/// One, plus one more per six characters — so a nine-character name absorbs a
/// transposition (which Levenshtein charges two for) and a long name absorbs a
/// suffix, while a name differing by a whole word does not.
#[must_use]
pub fn max_edit_distance(len: usize) -> usize {
    1 + len / 6
}

/// Ranking score between two keys. Used only to order candidates that have
/// already passed [`is_alias`]; it is never itself a decision.
#[must_use]
pub fn match_score(a: &str, b: &str) -> f64 {
    jaro_winkler(a, b)
}

/// Whether `b` is a spelling variant of `a`.
///
/// Two criteria, both required, because each covers the other's blind spot:
/// Jaro-Winkler is generous about shared prefixes (it scores "microsoft" and
/// "micron" at 0.87), and a bounded edit distance is what rules that pair out.
/// Names shorter than [`MIN_FUZZY_LEN`] never qualify.
#[must_use]
pub fn is_alias(a: &str, b: &str, threshold: f64) -> bool {
    let longest = a.chars().count().max(b.chars().count());
    if longest < MIN_FUZZY_LEN {
        return false;
    }
    jaro_winkler(a, b) >= threshold && levenshtein(a, b) <= max_edit_distance(longest)
}

/// Resolve one normalized entity against candidate rows of the same type.
///
/// Exact key equality wins outright. Otherwise the best candidate above
/// `threshold` is taken; ties break toward the shortest stored key, which is
/// the more general spelling and therefore the better canonical row.
#[must_use]
pub fn resolve_one(
    entity: &NormalizedEntity,
    candidates: &[EntityRecord],
    threshold: f64,
) -> EntityAction {
    let same_type = candidates
        .iter()
        .filter(|c| c.entity_type == entity.entity_type);

    let mut best: Option<(&EntityRecord, f64)> = None;
    for candidate in same_type {
        if candidate.match_key == entity.match_key {
            // Exact key: the row already covers this spelling unless the
            // display form is new.
            let new_alias = (candidate.canonical_name != entity.canonical_name)
                .then(|| entity.canonical_name.clone());
            return EntityAction::Link {
                entity_id: candidate.id.clone(),
                new_alias,
            };
        }
        if !is_alias(&candidate.match_key, &entity.match_key, threshold) {
            continue;
        }
        let score = match_score(&candidate.match_key, &entity.match_key);
        let better = match best {
            None => true,
            Some((prev, prev_score)) => {
                score > prev_score
                    || (score == prev_score && candidate.match_key.len() < prev.match_key.len())
            }
        };
        if better {
            best = Some((candidate, score));
        }
    }

    match best {
        Some((candidate, _)) => EntityAction::Link {
            entity_id: candidate.id.clone(),
            new_alias: Some(entity.canonical_name.clone()),
        },
        None => EntityAction::Create {
            canonical_name: entity.canonical_name.clone(),
            match_key: entity.match_key.clone(),
            entity_type: entity.entity_type,
        },
    }
}

/// Resolve every normalized entity for one article into an [`EntityPlan`].
#[must_use]
pub fn resolve(
    entities: &[NormalizedEntity],
    candidates: &[EntityRecord],
    threshold: f64,
) -> EntityPlan {
    entities
        .iter()
        .map(|e| resolve_one(e, candidates, threshold))
        .collect()
}

/// The short key prefixes a store should query on to fetch fuzzy candidates.
///
/// Fetching every entity of a type would blow the host output buffer once the
/// table has any size, so candidates are narrowed by the first four characters
/// of the key. Two spellings that differ inside the first four characters will
/// not be offered as candidates and so will not merge — an accepted precision
/// trade documented in `M2-FRICTION.md`.
#[must_use]
pub fn candidate_prefixes(entities: &[NormalizedEntity]) -> Vec<String> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    for e in entities {
        set.insert(e.match_key.chars().take(4).collect());
    }
    set.into_iter().collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn norm(name: &str, t: EntityType) -> NormalizedEntity {
        NormalizedEntity {
            canonical_name: name.to_string(),
            match_key: match_key(name),
            entity_type: t,
        }
    }

    fn rec(id: &str, name: &str, t: EntityType) -> EntityRecord {
        EntityRecord {
            id: id.to_string(),
            canonical_name: name.to_string(),
            match_key: match_key(name),
            entity_type: t,
        }
    }

    // ---- normalization ---------------------------------------------------

    #[test]
    fn match_key_folds_spacing_punctuation_and_case() {
        assert_eq!(match_key("OpenAI"), "openai");
        assert_eq!(match_key("Open A.I."), "openai");
        assert_eq!(match_key("open ai"), "openai");
        assert_eq!(match_key("OPENAI"), "openai");
    }

    #[test]
    fn match_key_drops_leading_article_and_corporate_suffix() {
        assert_eq!(match_key("The Guardian"), "guardian");
        assert_eq!(match_key("Acme Corp"), "acme");
        assert_eq!(match_key("Acme Corporation"), "acme");
        assert_eq!(match_key("Acme Inc."), "acme");
        // A bare suffix is not stripped to nothing.
        assert_eq!(match_key("Inc"), "inc");
        assert_eq!(match_key("The"), "the");
    }

    #[test]
    fn clean_name_rejects_junk() {
        assert_eq!(clean_name("  OpenAI  ").as_deref(), Some("OpenAI"));
        assert_eq!(clean_name("\"OpenAI\",").as_deref(), Some("OpenAI"));
        assert_eq!(clean_name("Open   AI").as_deref(), Some("Open AI"));
        assert!(clean_name("").is_none());
        assert!(clean_name("   ").is_none());
        assert!(clean_name("12345").is_none());
        assert!(clean_name(&"x".repeat(MAX_ENTITY_NAME_LEN + 1)).is_none());
    }

    #[test]
    fn entity_type_parse_is_tolerant() {
        assert_eq!(EntityType::parse("Person"), EntityType::Person);
        assert_eq!(EntityType::parse("people"), EntityType::Person);
        assert_eq!(EntityType::parse("ORGANIZATION"), EntityType::Company);
        assert_eq!(EntityType::parse(" companies "), EntityType::Company);
        assert_eq!(EntityType::parse("country"), EntityType::Place);
        assert_eq!(EntityType::parse("protocol"), EntityType::Technology);
        assert_eq!(EntityType::parse("banana"), EntityType::Other);
    }

    #[test]
    fn normalize_all_dedupes_within_an_article() {
        let extracted = vec![
            ExtractedEntity {
                name: "OpenAI".into(),
                entity_type: EntityType::Company,
            },
            ExtractedEntity {
                name: "Open A.I.".into(),
                entity_type: EntityType::Company,
            },
            ExtractedEntity {
                name: "  ".into(),
                entity_type: EntityType::Company,
            },
            ExtractedEntity {
                name: "OpenAI".into(),
                entity_type: EntityType::Place,
            },
        ];
        let out = normalize_all(&extracted);
        // Two survive: openai/company (deduped) and openai/place (different type).
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].match_key, "openai");
        assert_eq!(out[0].entity_type, EntityType::Company);
        assert_eq!(out[1].entity_type, EntityType::Place);
    }

    // ---- distance functions ----------------------------------------------

    #[test]
    fn jaro_winkler_known_values() {
        assert!((jaro_winkler("martha", "marhta") - 0.961).abs() < 0.005);
        assert!((jaro_winkler("dwayne", "duane") - 0.840).abs() < 0.005);
        assert_eq!(jaro_winkler("", ""), 1.0);
        assert_eq!(jaro_winkler("abc", ""), 0.0);
        assert_eq!(jaro_winkler("abc", "abc"), 1.0);
        assert_eq!(jaro_winkler("abc", "xyz"), 0.0);
    }

    #[test]
    fn levenshtein_known_values() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
    }

    #[test]
    fn is_alias_requires_both_criteria() {
        // Jaro-Winkler alone is generous about a shared prefix; the bounded
        // edit distance is what keeps these two companies apart.
        assert!(jaro_winkler("microsoft", "micron") > 0.8);
        assert!(
            !is_alias("microsoft", "micron", DEFAULT_MATCH_THRESHOLD),
            "distinct names sharing a prefix must not merge"
        );
        // A transposition inside a long-enough name does merge.
        assert!(is_alias("anthropic", "anthropci", DEFAULT_MATCH_THRESHOLD));
        // As does a suffix variant.
        assert!(is_alias(
            "jensenhuang",
            "jensonhuang",
            DEFAULT_MATCH_THRESHOLD
        ));
    }

    #[test]
    fn short_names_are_exact_match_only() {
        // "intel" and "intex" score 0.92 on Jaro-Winkler and differ by one
        // edit; at five characters that is as likely to be a different company
        // as a typo, so the fuzzy layer refuses to judge it.
        assert!(jaro_winkler("intel", "intex") >= DEFAULT_MATCH_THRESHOLD);
        assert!(!is_alias("intel", "intex", DEFAULT_MATCH_THRESHOLD));
        assert!(!is_alias("openai", "opemai", DEFAULT_MATCH_THRESHOLD));
        // The exact path still handles the spelling variants that matter for
        // short names, because `match_key` folds them before any scoring.
        assert_eq!(match_key("Open A.I."), match_key("OpenAI"));
    }

    #[test]
    fn the_edit_allowance_grows_with_length() {
        assert_eq!(max_edit_distance(MIN_FUZZY_LEN), 2);
        assert_eq!(max_edit_distance(9), 2);
        assert_eq!(max_edit_distance(18), 4);
        assert_eq!(max_edit_distance(30), 6);
    }

    #[test]
    fn a_name_differing_by_a_whole_word_does_not_merge() {
        assert!(!is_alias(
            "internationalbusinessmachines",
            "internationalbusinessmonkeys",
            DEFAULT_MATCH_THRESHOLD
        ));
    }

    // ---- resolution ------------------------------------------------------

    #[test]
    fn exact_key_links_without_scoring() {
        let candidates = vec![rec("e1", "OpenAI", EntityType::Company)];
        let action = resolve_one(
            &norm("Open A.I.", EntityType::Company),
            &candidates,
            DEFAULT_MATCH_THRESHOLD,
        );
        assert_eq!(
            action,
            EntityAction::Link {
                entity_id: "e1".into(),
                new_alias: Some("Open A.I.".into()),
            }
        );
    }

    #[test]
    fn exact_key_and_same_display_records_no_alias() {
        let candidates = vec![rec("e1", "OpenAI", EntityType::Company)];
        let action = resolve_one(
            &norm("OpenAI", EntityType::Company),
            &candidates,
            DEFAULT_MATCH_THRESHOLD,
        );
        assert_eq!(
            action,
            EntityAction::Link {
                entity_id: "e1".into(),
                new_alias: None,
            }
        );
    }

    #[test]
    fn typo_resolves_to_the_existing_entity() {
        let candidates = vec![rec("e1", "Anthropic", EntityType::Company)];
        let action = resolve_one(
            &norm("Anthropci", EntityType::Company),
            &candidates,
            DEFAULT_MATCH_THRESHOLD,
        );
        assert!(matches!(action, EntityAction::Link { .. }));
    }

    #[test]
    fn different_type_never_merges() {
        let candidates = vec![rec("e1", "Apple", EntityType::Company)];
        let action = resolve_one(
            &norm("Apple", EntityType::Place),
            &candidates,
            DEFAULT_MATCH_THRESHOLD,
        );
        assert!(
            matches!(action, EntityAction::Create { .. }),
            "same name, different type is a different entity"
        );
    }

    #[test]
    fn unrelated_name_creates() {
        let candidates = vec![rec("e1", "Anthropic", EntityType::Company)];
        let action = resolve_one(
            &norm("Nvidia", EntityType::Company),
            &candidates,
            DEFAULT_MATCH_THRESHOLD,
        );
        assert_eq!(
            action,
            EntityAction::Create {
                canonical_name: "Nvidia".into(),
                match_key: "nvidia".into(),
                entity_type: EntityType::Company,
            }
        );
    }

    #[test]
    fn no_candidates_creates() {
        let action = resolve_one(
            &norm("Nvidia", EntityType::Company),
            &[],
            DEFAULT_MATCH_THRESHOLD,
        );
        assert!(matches!(action, EntityAction::Create { .. }));
    }

    #[test]
    fn threshold_is_honoured_at_the_boundary() {
        let candidates = vec![rec("e1", "Anthropic", EntityType::Company)];
        let incoming = norm("Anthropci", EntityType::Company);
        let score = match_score("anthropic", "anthropci");
        // Exactly at the score: inclusive, so it links.
        assert!(matches!(
            resolve_one(&incoming, &candidates, score),
            EntityAction::Link { .. }
        ));
        // A hair above: it creates.
        assert!(matches!(
            resolve_one(&incoming, &candidates, score + 0.0001),
            EntityAction::Create { .. }
        ));
    }

    #[test]
    fn resolve_handles_a_whole_article() {
        let candidates = vec![
            rec("e1", "OpenAI", EntityType::Company),
            rec("e2", "San Francisco", EntityType::Place),
        ];
        let entities = normalize_all(&[
            ExtractedEntity {
                name: "Open AI".into(),
                entity_type: EntityType::Company,
            },
            ExtractedEntity {
                name: "Sam Altman".into(),
                entity_type: EntityType::Person,
            },
        ]);
        let plan = resolve(&entities, &candidates, DEFAULT_MATCH_THRESHOLD);
        assert_eq!(plan.len(), 2);
        assert!(matches!(plan[0], EntityAction::Link { .. }));
        assert!(matches!(plan[1], EntityAction::Create { .. }));
    }

    #[test]
    fn candidate_prefixes_are_deduped_and_short() {
        let entities = normalize_all(&[
            ExtractedEntity {
                name: "OpenAI".into(),
                entity_type: EntityType::Company,
            },
            ExtractedEntity {
                name: "Open A.I.".into(),
                entity_type: EntityType::Place,
            },
            ExtractedEntity {
                name: "Nvidia".into(),
                entity_type: EntityType::Company,
            },
            ExtractedEntity {
                name: "Al".into(),
                entity_type: EntityType::Person,
            },
        ]);
        let prefixes = candidate_prefixes(&entities);
        assert_eq!(
            prefixes,
            vec!["al".to_string(), "nvid".into(), "open".into()]
        );
        assert!(prefixes.iter().all(|p| p.chars().count() <= 4));
    }
}
