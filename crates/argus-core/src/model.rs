//! Domain types shared across the Argus pipeline core.
//!
//! These are pure data types with no host, wasm, or database dependencies.
//! The plugin's port implementations map them to/from `argus_articles` rows
//! and queue payloads; the kernel never sees them.

use serde::{Deserialize, Serialize};

/// The lifecycle state of an article as it moves through the pipeline.
///
/// Stored in the `argus_articles.pipeline_state` text column. The terminal
/// states are `Discarded` (below the relevance threshold), `Complete` (filed
/// into a story), and `Error` (a permanent failure). The happy path is
/// `Fetched` → `Decided` → `Analyzed` → `Embedded` → `Complete`; `Discarded` is
/// the common early exit at the decide stage, and `Waiting` is the cluster
/// stage's bounded "too close to call" hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineState {
    /// Freshly ingested from a feed, awaiting the decide stage.
    Fetched,
    /// Passed the decide threshold; awaiting analyze.
    Decided,
    /// Analyze stage claimed the article (M1 stub parks here).
    Analyzing,
    /// Analyze finished; awaiting embed.
    Analyzed,
    /// A feature vector was stored; awaiting cluster (M2).
    Embedded,
    /// Clustering deferred this article as too close to call; the maintenance
    /// pass re-enqueues it, and the wait is bounded by `cluster_attempts` (M2).
    Waiting,
    /// Fully processed.
    Complete,
    /// Scored below the topic threshold at decide. Terminal.
    Discarded,
    /// A permanent failure was recorded against this article. Terminal.
    Error,
}

impl PipelineState {
    /// The lowercase snake_case string persisted in the `pipeline_state` column.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            PipelineState::Fetched => "fetched",
            PipelineState::Decided => "decided",
            PipelineState::Analyzing => "analyzing",
            PipelineState::Analyzed => "analyzed",
            PipelineState::Embedded => "embedded",
            PipelineState::Waiting => "waiting",
            PipelineState::Complete => "complete",
            PipelineState::Discarded => "discarded",
            PipelineState::Error => "error",
        }
    }

    /// Whether the pipeline is finished with this article.
    ///
    /// The retention pass reclaims body text only from terminal articles, so an
    /// article still in flight never loses the content a later stage needs.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            PipelineState::Complete | PipelineState::Discarded | PipelineState::Error
        )
    }

    /// Every terminal state's column value, for the retention sweep's `IN` clause.
    #[must_use]
    pub fn terminal_columns() -> &'static [&'static str] {
        &["complete", "discarded", "error"]
    }

    /// Parse a `pipeline_state` column value back into the enum.
    #[must_use]
    pub fn from_column(s: &str) -> Option<Self> {
        Some(match s {
            "fetched" => PipelineState::Fetched,
            "decided" => PipelineState::Decided,
            "analyzing" => PipelineState::Analyzing,
            "analyzed" => PipelineState::Analyzed,
            "embedded" => PipelineState::Embedded,
            "waiting" => PipelineState::Waiting,
            "complete" => PipelineState::Complete,
            "discarded" => PipelineState::Discarded,
            "error" => PipelineState::Error,
            _ => return None,
        })
    }
}

/// A pipeline stage, used as the self-routing discriminator in queue payloads.
///
/// The kernel dispatches every job to the plugin's single `tap_queue_worker`
/// with the bare payload (there is no per-queue routing), so the worker reads
/// this discriminator to pick a stage handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Fetch a feed's body and ingest its articles.
    Fetch,
    /// Score an article's relevance against its topic.
    Decide,
    /// Deep analysis (M1 stub).
    Analyze,
    /// Embedding (M1 stub).
    Embed,
    /// Clustering into stories (M1 stub).
    Cluster,
    /// Summarization (M1 stub).
    Summarize,
    /// Dispatch one outbox notification to its channels (M4).
    ///
    /// Keyed on a notification event id rather than an article or a story: the
    /// decision to notify is recorded before the send is attempted, so an
    /// at-least-once redelivery re-reads the same row instead of re-deciding.
    Notify,
}

impl Stage {
    /// The queue name a stage's jobs are enqueued onto.
    ///
    /// Named per-stage for observability even though the kernel drains by
    /// plugin, not by queue.
    #[must_use]
    pub fn queue_name(&self) -> &'static str {
        match self {
            Stage::Fetch => "argus_fetch",
            Stage::Decide => "argus_decide",
            Stage::Analyze => "argus_analyze",
            Stage::Embed => "argus_embed",
            Stage::Cluster => "argus_cluster",
            Stage::Summarize => "argus_summarize",
            Stage::Notify => "argus_notify",
        }
    }

    /// Every stage, so a queue declaration or a round-trip test cannot silently
    /// miss one.
    #[must_use]
    pub fn all() -> &'static [Stage] {
        &[
            Stage::Fetch,
            Stage::Decide,
            Stage::Analyze,
            Stage::Embed,
            Stage::Cluster,
            Stage::Summarize,
            Stage::Notify,
        ]
    }
}

/// An article parsed out of a feed body, before it is persisted.
///
/// This is the pure output of [`crate::feed::parse_feed`]; the store port turns
/// it into an `argus_articles` upsert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedArticle {
    /// Canonical article URL (the dedup key).
    pub url: String,
    /// Article headline.
    pub title: String,
    /// Body/summary text extracted from the feed entry.
    pub content: String,
    /// Publication time as a unix timestamp (seconds), if the feed supplied one.
    pub published_at: Option<i64>,
}

/// A decode of a decide-stage model response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// Relevance score, clamped to `0..=100`.
    pub score: u8,
    /// Free-text justification the model returned (may be empty).
    pub reason: String,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Every state this enum knows about. A new variant that is not added here
    /// fails to compile, which is the point: the round-trip and terminal-set
    /// guards below then cover it automatically.
    const ALL: &[PipelineState] = &[
        PipelineState::Fetched,
        PipelineState::Decided,
        PipelineState::Analyzing,
        PipelineState::Analyzed,
        PipelineState::Embedded,
        PipelineState::Waiting,
        PipelineState::Complete,
        PipelineState::Discarded,
        PipelineState::Error,
    ];

    #[test]
    fn every_state_round_trips_through_its_column_value() {
        for state in ALL {
            assert_eq!(
                PipelineState::from_column(state.as_str()),
                Some(*state),
                "{state:?} does not round-trip"
            );
        }
    }

    #[test]
    fn column_values_are_unique() {
        let mut seen: Vec<&str> = ALL.iter().map(PipelineState::as_str).collect();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(before, seen.len(), "two states share a column value");
    }

    #[test]
    fn an_unknown_column_value_is_rejected() {
        assert_eq!(PipelineState::from_column("banana"), None);
        assert_eq!(PipelineState::from_column(""), None);
    }

    #[test]
    fn the_terminal_set_and_the_terminal_predicate_agree() {
        // Two sources of truth (a `matches!` and a SQL `IN` list) that must not
        // drift apart.
        let from_predicate: Vec<&str> = ALL
            .iter()
            .filter(|s| s.is_terminal())
            .map(PipelineState::as_str)
            .collect();
        let mut declared = PipelineState::terminal_columns().to_vec();
        let mut derived = from_predicate;
        declared.sort_unstable();
        derived.sort_unstable();
        assert_eq!(declared, derived);
    }

    #[test]
    fn in_flight_states_are_not_terminal() {
        for state in [
            PipelineState::Fetched,
            PipelineState::Decided,
            PipelineState::Analyzed,
            PipelineState::Embedded,
            PipelineState::Waiting,
        ] {
            assert!(!state.is_terminal(), "{state:?} must not be terminal");
        }
    }

    #[test]
    fn stage_queue_names_are_unique_and_prefixed() {
        let mut names: Vec<&str> = Stage::all().iter().map(Stage::queue_name).collect();
        assert!(names.iter().all(|n| n.starts_with("argus_")));
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "two stages share a queue name");
    }
}
