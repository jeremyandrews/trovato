//! Story clustering decisions (M2 cluster stage): all the scoring and all the
//! branching, as pure functions over data.
//!
//! The stage asks one question per analyzed article — does this belong to a
//! story that already exists, does it start a new one, is it a re-report of an
//! article already in a story, or is the evidence too thin to decide yet — and
//! this module answers it. Nothing here touches a store, a queue or a model, so
//! every edge is unit-testable against synthetic vectors.
//!
//! # The score
//!
//! ```text
//! score = cosine(article, story centroid)
//!       * decay(days between article and story's newest member)
//!       + coherence(same topic ? +0.05 : -0.10)
//! ```
//!
//! Cosine is over the lexical vectors from [`crate::embed`], which weight
//! entity names heaviest — so the single similarity term already carries entity
//! overlap and no separate Jaccard is blended in (see `M2-DESIGN.md`).
//!
//! # Why "wait" exists
//!
//! A near-threshold match is the expensive mistake in both directions: joining
//! merges two stories that should stay apart, creating fragments a story that
//! should have been one. So a score inside [`WAIT_MARGIN`] below the threshold
//! defers instead of guessing, and the maintenance pass re-runs the article
//! later when more of its neighbourhood exists. The deferral is bounded by
//! [`ClusterConfig::max_waits`]: past that, the decision is forced to create,
//! because an article that waits forever is an article the reader never sees.

use crate::embed::cosine;

/// Default cosine-plus-adjustments threshold for joining a story.
///
/// Calibrated for the `lex-v1` lexical vectors, **not** comparable to the value
/// a semantic-embedding route would use: lexical cosine between two genuine
/// re-reports of one event runs materially lower than semantic cosine would,
/// because the outlets choose different words for the same facts. Tunable via
/// `argus.cluster_threshold`.
pub const DEFAULT_JOIN_THRESHOLD: f32 = 0.55;

/// Default join threshold for the **semantic** route (`argus.embed_model` set).
///
/// Semantic cosine between two genuine re-reports of one event runs materially
/// higher than lexical cosine — the model sees past the outlets' word choices,
/// which is the entire reason to pay for it — and semantic cosine between two
/// *unrelated* news articles rarely falls below ~0.6, so the lexical 0.55 would
/// join nearly everything to everything. `argus.cluster_threshold` overrides.
///
/// Provisional in the same sense 0.55 is: calibrated from the shape of the two
/// distributions rather than from a measured Argus corpus, and `run_cluster`
/// reports the winning score on every join so it can be tuned against one.
pub const DEFAULT_SEMANTIC_JOIN_THRESHOLD: f32 = 0.82;

/// Default cosine at or above which two articles are the same piece.
///
/// This is a near-identity test — syndicated copy, a wire story reprinted, the
/// same text behind two feed URLs — so it sits far above the join threshold.
pub const DEFAULT_NEAR_DUP_THRESHOLD: f32 = 0.98;

/// Default publication window for candidates, in seconds (14 days).
pub const DEFAULT_WINDOW_SECONDS: i64 = 14 * 86_400;

/// Default idle period after which a story stops accepting articles (3 days).
pub const DEFAULT_INACTIVE_SECONDS: i64 = 3 * 86_400;

/// Days of separation over which the decay factor falls from 1.0 to 0.5.
pub const DECAY_HORIZON_DAYS: f32 = 7.0;

/// Floor of the decay factor. Beyond the horizon, separation stops mattering
/// more — a story is either still the same story or it is not.
pub const DECAY_FLOOR: f32 = 0.5;

/// Bonus applied when the article and the story share a topic.
pub const COHERENCE_SAME_TOPIC: f32 = 0.05;

/// Penalty applied when the article and the story are in different topics.
pub const COHERENCE_DIFFERENT_TOPIC: f32 = -0.10;

/// How far below the join threshold counts as "too close to call".
pub const WAIT_MARGIN: f32 = 0.08;

/// Tunables for one clustering run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClusterConfig {
    /// Score at or above which an article joins a story.
    pub join_threshold: f32,
    /// Cosine at or above which an article is a duplicate of another article.
    pub near_dup_threshold: f32,
    /// Candidate publication window, in seconds.
    pub window_seconds: i64,
    /// Idle period after which a story goes inactive, in seconds.
    pub inactive_seconds: i64,
    /// How many times an article may be deferred before a story is forced.
    pub max_waits: u32,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            join_threshold: DEFAULT_JOIN_THRESHOLD,
            near_dup_threshold: DEFAULT_NEAR_DUP_THRESHOLD,
            window_seconds: DEFAULT_WINDOW_SECONDS,
            inactive_seconds: DEFAULT_INACTIVE_SECONDS,
            max_waits: 1,
        }
    }
}

/// A story offered to the scorer as a possible home for an article.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateStory {
    /// Story id (also the `argus_story` Item id).
    pub id: String,
    /// The story's centroid vector.
    pub centroid: Vec<f32>,
    /// The recipe the centroid was built with; a mismatch disqualifies it.
    pub recipe: String,
    /// The story's topic, if it has one.
    pub topic_id: Option<String>,
    /// Publication time of the story's newest member (unix seconds).
    pub last_article_at: Option<i64>,
    /// Members counted toward the story (duplicates excluded).
    pub article_count: u32,
}

/// An already-stored article offered as a possible near-duplicate source.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateArticle {
    /// Article id.
    pub id: String,
    /// Its vector.
    pub vector: Vec<f32>,
    /// The recipe its vector was built with.
    pub recipe: String,
    /// The feed it came from — a duplicate must come from a *different* feed.
    pub feed_id: Option<String>,
    /// The story it belongs to, if it has been clustered.
    pub story_id: Option<String>,
    /// Publication time (unix seconds).
    pub published_at: Option<i64>,
}

/// The article being clustered.
#[derive(Debug, Clone, PartialEq)]
pub struct IncomingArticle {
    /// Article id.
    pub id: String,
    /// Its vector.
    pub vector: Vec<f32>,
    /// The recipe its vector was built with.
    pub recipe: String,
    /// Its topic.
    pub topic_id: Option<String>,
    /// The feed it came from.
    pub feed_id: Option<String>,
    /// Publication time (unix seconds).
    pub published_at: Option<i64>,
    /// Its relevance score from the decide stage.
    pub relevance_score: Option<i32>,
    /// How many times it has already been deferred.
    pub waits: u32,
}

/// What the cluster stage should do with an article.
#[derive(Debug, Clone, PartialEq)]
pub enum ClusterDecision {
    /// Add the article to an existing story.
    Join {
        /// Target story.
        story_id: String,
        /// The winning score, for logging and calibration.
        score: f32,
    },
    /// Start a new story with this article as its only member.
    Create,
    /// Too close to call; defer and reconsider later.
    Wait {
        /// Best score seen, for logging.
        best_score: f32,
    },
    /// The article re-reports one already stored.
    Duplicate {
        /// The earlier article it duplicates.
        of_article_id: String,
        /// That article's story, if it has one — the duplicate is filed there
        /// as a source without being counted as a member.
        story_id: Option<String>,
        /// The cosine that identified it.
        similarity: f32,
    },
}

/// Time-decay factor for `gap_seconds` of separation.
///
/// Falls linearly from 1.0 at zero separation to [`DECAY_FLOOR`] at
/// [`DECAY_HORIZON_DAYS`], then stays flat. A missing timestamp on either side
/// yields 1.0: absent evidence of separation is not evidence of separation.
#[must_use]
pub fn decay(gap_seconds: i64) -> f32 {
    let days = (gap_seconds.abs() as f32) / 86_400.0;
    if days >= DECAY_HORIZON_DAYS {
        return DECAY_FLOOR;
    }
    1.0 - (1.0 - DECAY_FLOOR) * (days / DECAY_HORIZON_DAYS)
}

/// Topic coherence adjustment between an article and a story.
///
/// An unknown topic on either side is neutral rather than penalized — the
/// penalty is for a *known* mismatch.
#[must_use]
pub fn coherence(article_topic: Option<&str>, story_topic: Option<&str>) -> f32 {
    match (article_topic, story_topic) {
        (Some(a), Some(s)) if a == s => COHERENCE_SAME_TOPIC,
        (Some(_), Some(_)) => COHERENCE_DIFFERENT_TOPIC,
        _ => 0.0,
    }
}

/// Score one candidate story for one article.
///
/// Returns `None` when the candidate cannot be compared at all: a recipe or
/// dimension mismatch, or a zero vector on either side. `None` is not a low
/// score — it means the candidate is not evidence, and it must not compete.
#[must_use]
pub fn score_candidate(article: &IncomingArticle, story: &CandidateStory) -> Option<f32> {
    if article.recipe != story.recipe {
        return None;
    }
    let similarity = cosine(&article.vector, &story.centroid);
    if similarity == 0.0 {
        return None;
    }
    let gap = match (article.published_at, story.last_article_at) {
        (Some(a), Some(s)) => a - s,
        _ => 0,
    };
    let scored =
        similarity * decay(gap) + coherence(article.topic_id.as_deref(), story.topic_id.as_deref());
    Some(scored.clamp(-1.0, 1.0))
}

/// Find the near-duplicate source for an article, if any.
///
/// A duplicate must come from a **different feed**: the same feed re-serving
/// its own item is the dedup case M1 already handles on the canonical URL, and
/// treating it as a cross-source duplicate would wrongly suppress a genuine
/// update. Ties break toward the oldest publication time, so the original is
/// kept and the re-report is the one marked.
#[must_use]
pub fn find_duplicate(
    article: &IncomingArticle,
    candidates: &[CandidateArticle],
    threshold: f32,
) -> Option<(String, Option<String>, f32)> {
    let mut best: Option<(&CandidateArticle, f32)> = None;
    for candidate in candidates {
        if candidate.id == article.id || candidate.recipe != article.recipe {
            continue;
        }
        // Same feed, or an unknown feed on either side: not a cross-source
        // duplicate.
        match (&article.feed_id, &candidate.feed_id) {
            (Some(a), Some(c)) if a != c => {}
            _ => continue,
        }
        let similarity = cosine(&article.vector, &candidate.vector);
        if similarity < threshold {
            continue;
        }
        let better = match best {
            None => true,
            Some((prev, prev_sim)) => {
                similarity > prev_sim
                    || (similarity == prev_sim
                        && candidate.published_at.unwrap_or(i64::MAX)
                            < prev.published_at.unwrap_or(i64::MAX))
            }
        };
        if better {
            best = Some((candidate, similarity));
        }
    }
    best.map(|(c, sim)| (c.id.clone(), c.story_id.clone(), sim))
}

/// Decide what to do with one analyzed article.
///
/// Order matters: near-duplicate detection runs first, because a re-report must
/// be filed against the original's story rather than scored on its own merits
/// and possibly used to start a competing story.
#[must_use]
pub fn decide(
    article: &IncomingArticle,
    stories: &[CandidateStory],
    articles: &[CandidateArticle],
    config: &ClusterConfig,
) -> ClusterDecision {
    if let Some((of_article_id, story_id, similarity)) =
        find_duplicate(article, articles, config.near_dup_threshold)
    {
        return ClusterDecision::Duplicate {
            of_article_id,
            story_id,
            similarity,
        };
    }

    let mut best: Option<(&CandidateStory, f32)> = None;
    for story in stories {
        let Some(score) = score_candidate(article, story) else {
            continue;
        };
        let better = match best {
            None => true,
            // Ties break toward the larger story: an established narrative is
            // the better home than a story of one, and it keeps clusters from
            // splintering when several candidates score identically.
            Some((prev, prev_score)) => {
                score > prev_score
                    || (score == prev_score && story.article_count > prev.article_count)
            }
        };
        if better {
            best = Some((story, score));
        }
    }

    match best {
        Some((story, score)) if score >= config.join_threshold => ClusterDecision::Join {
            story_id: story.id.clone(),
            score,
        },
        Some((_, score))
            if score >= config.join_threshold - WAIT_MARGIN && article.waits < config.max_waits =>
        {
            ClusterDecision::Wait { best_score: score }
        }
        _ => ClusterDecision::Create,
    }
}

/// Whether a story has gone idle as of `now`.
#[must_use]
pub fn is_stale(last_article_at: Option<i64>, now: i64, inactive_seconds: i64) -> bool {
    match last_article_at {
        Some(t) => now - t > inactive_seconds,
        // A story with no dated member is judged on nothing; leave it active
        // rather than retiring it on missing data.
        None => false,
    }
}

/// The oldest publication time still inside the candidate window at `now`.
#[must_use]
pub fn window_start(now: i64, window_seconds: i64) -> i64 {
    now.saturating_sub(window_seconds)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const RECIPE: &str = "lex-v1/8";
    const DAY: i64 = 86_400;

    /// A unit vector whose cosine against `blended(1.0)` is exactly `c`, so a
    /// test can dial a similarity instead of guessing one.
    fn blended(c: f32) -> Vec<f32> {
        let mut v = vec![0.0f32; 8];
        v[0] = c;
        v[1] = (1.0 - c * c).max(0.0).sqrt();
        v
    }

    #[test]
    fn the_test_fixture_dials_an_exact_cosine() {
        assert!((crate::embed::cosine(&blended(1.0), &blended(0.7)) - 0.7).abs() < 1e-5);
    }

    fn article(vector: Vec<f32>) -> IncomingArticle {
        IncomingArticle {
            id: "a-new".into(),
            vector,
            recipe: RECIPE.into(),
            topic_id: Some("t1".into()),
            feed_id: Some("f1".into()),
            published_at: Some(10 * DAY),
            relevance_score: Some(80),
            waits: 0,
        }
    }

    fn story(id: &str, centroid: Vec<f32>) -> CandidateStory {
        CandidateStory {
            id: id.into(),
            centroid,
            recipe: RECIPE.into(),
            topic_id: Some("t1".into()),
            last_article_at: Some(10 * DAY),
            article_count: 2,
        }
    }

    fn stored(id: &str, vector: Vec<f32>, feed: &str) -> CandidateArticle {
        CandidateArticle {
            id: id.into(),
            vector,
            recipe: RECIPE.into(),
            feed_id: Some(feed.into()),
            story_id: Some("s1".into()),
            published_at: Some(9 * DAY),
        }
    }

    // ---- decay -----------------------------------------------------------

    #[test]
    fn decay_runs_from_one_to_the_floor_over_the_horizon() {
        assert!((decay(0) - 1.0).abs() < 1e-6);
        assert!((decay(7 * DAY) - DECAY_FLOOR).abs() < 1e-6);
        let mid = decay(7 * DAY / 2);
        assert!(
            (mid - 0.75).abs() < 1e-3,
            "half the horizon halves the drop"
        );
    }

    #[test]
    fn decay_is_flat_past_the_horizon_and_symmetric() {
        assert_eq!(decay(30 * DAY), DECAY_FLOOR);
        assert_eq!(decay(-3 * DAY), decay(3 * DAY));
    }

    // ---- coherence -------------------------------------------------------

    #[test]
    fn coherence_rewards_same_topic_and_penalizes_a_known_mismatch() {
        assert_eq!(coherence(Some("t1"), Some("t1")), COHERENCE_SAME_TOPIC);
        assert_eq!(coherence(Some("t1"), Some("t2")), COHERENCE_DIFFERENT_TOPIC);
        assert_eq!(coherence(None, Some("t2")), 0.0);
        assert_eq!(coherence(Some("t1"), None), 0.0);
        assert_eq!(coherence(None, None), 0.0);
    }

    // ---- scoring ---------------------------------------------------------

    #[test]
    fn identical_vectors_in_the_same_topic_score_above_one_minus_nothing() {
        let v = blended(1.0);
        let score = score_candidate(&article(v.clone()), &story("s1", v)).unwrap();
        // Cosine 1.0, no time gap, same-topic bonus, then clamped to 1.0.
        assert!((score - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_recipe_mismatch_disqualifies_a_candidate() {
        let v = blended(1.0);
        let mut s = story("s1", v.clone());
        s.recipe = "lex-v1/256".into();
        assert_eq!(score_candidate(&article(v), &s), None);
    }

    #[test]
    fn a_zero_vector_disqualifies_a_candidate() {
        let zero = vec![0.0f32; 8];
        assert_eq!(
            score_candidate(&article(blended(1.0)), &story("s1", zero)),
            None
        );
        assert_eq!(
            score_candidate(&article(vec![0.0f32; 8]), &story("s1", blended(1.0))),
            None
        );
    }

    #[test]
    fn time_separation_lowers_the_score() {
        let v = blended(1.0);
        let mut far = story("s1", v.clone());
        far.last_article_at = Some(3 * DAY); // seven days before the article
        let near_score = score_candidate(&article(v.clone()), &story("s1", v.clone())).unwrap();
        let far_score = score_candidate(&article(v), &far).unwrap();
        assert!(far_score < near_score);
        assert!((far_score - (1.0 * DECAY_FLOOR + COHERENCE_SAME_TOPIC)).abs() < 1e-5);
    }

    #[test]
    fn a_different_topic_costs_more_than_the_same_topic_gains() {
        // Deliberately not a perfect match: at cosine 1.0 the same-topic bonus
        // is swallowed by the clamp and the two adjustments cannot be compared.
        let v = blended(0.8);
        let mut other = story("s2", v.clone());
        other.topic_id = Some("t2".into());
        let same = score_candidate(&article(blended(1.0)), &story("s1", v.clone())).unwrap();
        let diff = score_candidate(&article(blended(1.0)), &other).unwrap();
        assert!((same - diff - (COHERENCE_SAME_TOPIC - COHERENCE_DIFFERENT_TOPIC)).abs() < 1e-5);
    }

    #[test]
    fn the_score_is_clamped_to_one() {
        // Cosine 1.0 plus the same-topic bonus would exceed 1.0.
        let v = blended(1.0);
        let score = score_candidate(&article(v.clone()), &story("s1", v)).unwrap();
        assert!(score <= 1.0);
    }

    // ---- the three-way decision -----------------------------------------

    #[test]
    fn a_strong_match_joins() {
        let v = blended(1.0);
        let d = decide(
            &article(v.clone()),
            &[story("s1", v)],
            &[],
            &ClusterConfig::default(),
        );
        match d {
            ClusterDecision::Join { story_id, score } => {
                assert_eq!(story_id, "s1");
                assert!(score >= DEFAULT_JOIN_THRESHOLD);
            }
            other => panic!("expected Join, got {other:?}"),
        }
    }

    #[test]
    fn no_candidates_creates() {
        let d = decide(&article(blended(1.0)), &[], &[], &ClusterConfig::default());
        assert_eq!(d, ClusterDecision::Create);
    }

    #[test]
    fn a_clearly_weak_match_creates_rather_than_waiting() {
        // Cosine ~0.2: far below threshold minus the wait margin.
        let d = decide(
            &article(blended(1.0)),
            &[story("s1", blended(0.2))],
            &[],
            &ClusterConfig::default(),
        );
        assert_eq!(d, ClusterDecision::Create);
    }

    #[test]
    fn a_near_threshold_match_waits() {
        let cfg = ClusterConfig::default();
        // Aim the score just under the threshold but inside the margin.
        let target = cfg.join_threshold - WAIT_MARGIN / 2.0 - COHERENCE_SAME_TOPIC;
        let s = story("s1", blended(target));
        let scored = score_candidate(&article(blended(1.0)), &s).unwrap();
        assert!(
            scored < cfg.join_threshold && scored >= cfg.join_threshold - WAIT_MARGIN,
            "test fixture must land inside the wait band, got {scored}"
        );
        match decide(&article(blended(1.0)), &[s], &[], &cfg) {
            ClusterDecision::Wait { best_score } => assert!((best_score - scored).abs() < 1e-5),
            other => panic!("expected Wait, got {other:?}"),
        }
    }

    #[test]
    fn an_exhausted_wait_budget_forces_a_story() {
        let cfg = ClusterConfig::default();
        let target = cfg.join_threshold - WAIT_MARGIN / 2.0 - COHERENCE_SAME_TOPIC;
        let mut a = article(blended(1.0));
        a.waits = cfg.max_waits;
        assert_eq!(
            decide(&a, &[story("s1", blended(target))], &[], &cfg),
            ClusterDecision::Create,
            "an article must not defer forever"
        );
    }

    #[test]
    fn the_threshold_boundary_is_inclusive() {
        let mut cfg = ClusterConfig::default();
        let v = blended(1.0);
        let s = story("s1", v.clone());
        let scored = score_candidate(&article(v.clone()), &s).unwrap();
        cfg.join_threshold = scored;
        assert!(matches!(
            decide(&article(v.clone()), std::slice::from_ref(&s), &[], &cfg),
            ClusterDecision::Join { .. }
        ));
        cfg.join_threshold = scored + 0.0001;
        assert!(!matches!(
            decide(&article(v), &[s], &[], &cfg),
            ClusterDecision::Join { .. }
        ));
    }

    #[test]
    fn the_best_of_several_candidates_wins() {
        let v = blended(1.0);
        let d = decide(
            &article(v.clone()),
            &[
                story("weak", blended(0.6)),
                story("strong", v),
                story("mid", blended(0.8)),
            ],
            &[],
            &ClusterConfig::default(),
        );
        match d {
            ClusterDecision::Join { story_id, .. } => assert_eq!(story_id, "strong"),
            other => panic!("expected Join(strong), got {other:?}"),
        }
    }

    #[test]
    fn a_tie_breaks_toward_the_larger_story() {
        let v = blended(1.0);
        let small = story("small", v.clone());
        let mut large = story("large", v.clone());
        large.article_count = 9;
        let d = decide(&article(v), &[small, large], &[], &ClusterConfig::default());
        match d {
            ClusterDecision::Join { story_id, .. } => assert_eq!(story_id, "large"),
            other => panic!("expected Join(large), got {other:?}"),
        }
    }

    // ---- near-duplicate --------------------------------------------------

    #[test]
    fn an_identical_article_from_another_feed_is_a_duplicate() {
        let v = blended(1.0);
        let d = decide(
            &article(v.clone()),
            &[story("s1", v.clone())],
            &[stored("a-old", v, "f2")],
            &ClusterConfig::default(),
        );
        match d {
            ClusterDecision::Duplicate {
                of_article_id,
                story_id,
                similarity,
            } => {
                assert_eq!(of_article_id, "a-old");
                assert_eq!(story_id.as_deref(), Some("s1"));
                assert!(similarity >= DEFAULT_NEAR_DUP_THRESHOLD);
            }
            other => panic!("expected Duplicate, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_detection_beats_joining() {
        // The same article is both a perfect story match and a perfect
        // duplicate; the duplicate branch must win so the re-report is filed
        // as a source rather than counted as a second member.
        let v = blended(1.0);
        let d = decide(
            &article(v.clone()),
            &[story("s1", v.clone())],
            &[stored("a-old", v, "f2")],
            &ClusterConfig::default(),
        );
        assert!(matches!(d, ClusterDecision::Duplicate { .. }));
    }

    #[test]
    fn the_same_feed_is_never_a_duplicate_source() {
        let v = blended(1.0);
        let d = decide(
            &article(v.clone()),
            &[],
            &[stored("a-old", v, "f1")],
            &ClusterConfig::default(),
        );
        assert_eq!(d, ClusterDecision::Create, "same feed is M1's URL dedup");
    }

    #[test]
    fn a_close_but_not_identical_article_is_not_a_duplicate() {
        // 0.9 cosine: clearly the same story, clearly not the same piece.
        let d = decide(
            &article(blended(1.0)),
            &[],
            &[stored("a-old", blended(0.9), "f2")],
            &ClusterConfig::default(),
        );
        assert_eq!(d, ClusterDecision::Create);
    }

    #[test]
    fn duplicate_ties_break_toward_the_oldest_original() {
        let v = blended(1.0);
        let mut older = stored("a-older", v.clone(), "f2");
        older.published_at = Some(DAY);
        let newer = stored("a-newer", v.clone(), "f3");
        let found = find_duplicate(&article(v), &[newer, older], DEFAULT_NEAR_DUP_THRESHOLD);
        assert_eq!(found.map(|f| f.0), Some("a-older".to_string()));
    }

    #[test]
    fn an_article_is_never_its_own_duplicate() {
        let v = blended(1.0);
        let mut self_row = stored("a-new", v.clone(), "f2");
        self_row.story_id = None;
        assert!(find_duplicate(&article(v), &[self_row], DEFAULT_NEAR_DUP_THRESHOLD).is_none());
    }

    // ---- staleness / window ---------------------------------------------

    #[test]
    fn a_story_goes_stale_after_the_idle_period() {
        let now = 100 * DAY;
        assert!(!is_stale(Some(now - DAY), now, DEFAULT_INACTIVE_SECONDS));
        assert!(!is_stale(
            Some(now - DEFAULT_INACTIVE_SECONDS),
            now,
            DEFAULT_INACTIVE_SECONDS
        ));
        assert!(is_stale(
            Some(now - DEFAULT_INACTIVE_SECONDS - 1),
            now,
            DEFAULT_INACTIVE_SECONDS
        ));
    }

    #[test]
    fn a_story_with_no_dated_member_is_not_stale() {
        assert!(!is_stale(None, 100 * DAY, DEFAULT_INACTIVE_SECONDS));
    }

    #[test]
    fn window_start_is_the_window_before_now() {
        assert_eq!(window_start(100 * DAY, DEFAULT_WINDOW_SECONDS), 86 * DAY);
        // A window reaching before the epoch is harmless — it is compared
        // against publication timestamps, and an earlier bound just admits
        // every candidate. What matters is that it does not wrap.
        assert_eq!(window_start(10, 100), -90);
        assert_eq!(window_start(i64::MIN, 100), i64::MIN);
    }
}
