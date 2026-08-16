//! Error types for the Argus pipeline core.
//!
//! The core is host-agnostic, so it never surfaces kernel host error codes
//! (negative `i32`s) directly. Port implementations translate host failures
//! into [`CoreError`] variants, and the core's own pure logic (feed parsing,
//! decision parsing) produces the parse/validation variants.

use std::fmt;

/// A failure inside the pipeline core or one of its injected ports.
///
/// Variants distinguish *transient* failures (worth a queue retry) from
/// *permanent* ones (parse failures that must not be retried). The
/// [`CoreError::is_transient`] predicate is the single source of truth the
/// plugin's queue worker consults when deciding whether to `panic!` (retry)
/// or return normally (give up / record terminal state).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreError {
    /// A feed body could not be parsed as RSS or Atom. Permanent.
    FeedParse(String),
    /// A model response could not be coerced into the expected shape even
    /// after lenient recovery. Permanent — the article is discarded, not
    /// retried.
    DecisionParse(String),
    /// An analyze response carried no recoverable analysis. Permanent — the
    /// article is flagged with the parse error, not retried against the same
    /// prompt and model.
    AnalysisParse(String),
    /// A summarize response carried no recoverable summary. Permanent — the
    /// story keeps its previous summary and the job is not retried.
    SummaryParse(String),
    /// The LLM provider failed transiently (rate limit, timeout, provider
    /// down, no provider configured). Transient — worth a retry.
    Provider(String),
    /// An outbound fetch failed transiently (network, timeout, 5xx).
    /// Transient.
    Fetch(String),
    /// A fetch was refused for a permanent reason (blocked URL / SSRF fence,
    /// 4xx). Permanent — flag the feed, do not hammer it with retries.
    FetchRefused(String),
    /// A storage (db host) operation failed. Transient by default — a queue
    /// retry re-attempts the write.
    Store(String),
    /// A queue enqueue failed. Transient.
    Queue(String),
    /// A required record was not found where the caller expected one
    /// (e.g. a decide job references an article row that was deleted).
    /// Permanent — nothing to retry.
    NotFound(String),
    /// Input that cannot be interpreted (an unknown reaction name, a
    /// configuration value with no legal reading). Permanent — a retry
    /// re-presents the same input and gets the same answer.
    Invalid(String),
}

impl CoreError {
    /// Whether a queue worker should retry the job that produced this error.
    ///
    /// Transient errors (provider/fetch/store/queue infrastructure hiccups)
    /// return `true`: the worker should `panic!` so queue v2 reschedules with
    /// backoff and eventually dead-letters. Permanent errors (parse failures,
    /// refused fetches, missing rows) return `false`: the worker records a
    /// terminal state and returns normally so the job is not retried.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            CoreError::Provider(_)
                | CoreError::Fetch(_)
                | CoreError::Store(_)
                | CoreError::Queue(_)
        )
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::FeedParse(m) => write!(f, "feed parse error: {m}"),
            CoreError::DecisionParse(m) => write!(f, "decision parse error: {m}"),
            CoreError::AnalysisParse(m) => write!(f, "analysis parse error: {m}"),
            CoreError::SummaryParse(m) => write!(f, "summary parse error: {m}"),
            CoreError::Provider(m) => write!(f, "provider error: {m}"),
            CoreError::Fetch(m) => write!(f, "fetch error: {m}"),
            CoreError::FetchRefused(m) => write!(f, "fetch refused: {m}"),
            CoreError::Store(m) => write!(f, "store error: {m}"),
            CoreError::Queue(m) => write!(f, "queue error: {m}"),
            CoreError::NotFound(m) => write!(f, "not found: {m}"),
            CoreError::Invalid(m) => write!(f, "invalid input: {m}"),
        }
    }
}

impl std::error::Error for CoreError {}

/// Convenience alias for fallible core operations.
pub type CoreResult<T> = Result<T, CoreError>;
