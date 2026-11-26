//! Follow-up sentiment, classified off the request path.
//!
//! When sentiment and analytics are both switched on, the follow-up endpoint
//! hands the visitor's message and the answer before it to [`SentimentQueue`]
//! and returns. One background worker drains the queue, asks the configured
//! Chat provider for one word through the kernel's one send path
//! (`AiProviderService::chat_once`, by way of `routes::api_search::send_prompt`),
//! and adds
//! the label to the day's analytics counters. Nothing else is kept: the text
//! goes to the provider and is dropped.
//!
//! The queue is bounded at [`QUEUE_CAPACITY`]. A full queue drops the job, and
//! the number dropped is logged at the next analytics flush.

use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::mpsc;

use super::SearchService;
use super::analytics::Counter;
use super::prompts;
use crate::models::SiteConfig;
use crate::services::ai_provider::{AiCaller, AiOperationType, AiProviderService};

/// Jobs that may wait for the worker.
pub const QUEUE_CAPACITY: usize = 256;

/// Most characters of each message sent to the classifier.
const MAX_MESSAGE_CHARS: usize = 2000;

/// What the usage log records the classifier's calls as.
pub const SENTIMENT_USAGE_SOURCE: &str = "kernel_search_sentiment";

/// Who a classification is made for: the anonymous search traffic that
/// produced it, off the request path (so it takes the background timeout).
fn classifier_caller() -> AiCaller {
    AiCaller::anonymous(SENTIMENT_USAGE_SOURCE).in_background()
}

/// One follow-up to classify.
struct Job {
    question: String,
    previous_answer: String,
}

/// The sending side of the classifier queue. One per kernel process, owned by
/// the application state.
pub struct SentimentQueue {
    tx: mpsc::Sender<Job>,
    search: Arc<SearchService>,
}

impl SentimentQueue {
    /// Create the queue and spawn its one worker.
    pub fn start(search: Arc<SearchService>, ai: Arc<AiProviderService>, pool: PgPool) -> Self {
        let (tx, rx) = mpsc::channel(QUEUE_CAPACITY);
        tokio::spawn(run(rx, Arc::clone(&search), ai, pool));
        Self { tx, search }
    }

    /// Queue one follow-up for classification and return at once. The caller
    /// checks the settings first; a full queue drops the job.
    pub fn submit(&self, question: &str, previous_answer: &str) {
        let job = Job {
            question: clip(question),
            previous_answer: clip(previous_answer),
        };
        if self.tx.try_send(job).is_err() {
            self.search.analytics().note_dropped_sentiment();
        }
    }
}

/// The label in a classifier reply, if it is one of the three.
pub fn parse_label(reply: &str) -> Option<Counter> {
    match reply
        .trim()
        .trim_end_matches(['.', '!'])
        .to_ascii_lowercase()
        .as_str()
    {
        "satisfied" => Some(Counter::Satisfied),
        "confused" => Some(Counter::Confused),
        "unmet" => Some(Counter::Unmet),
        _ => None,
    }
}

fn clip(text: &str) -> String {
    text.chars().take(MAX_MESSAGE_CHARS).collect()
}

async fn run(
    mut rx: mpsc::Receiver<Job>,
    search: Arc<SearchService>,
    ai: Arc<AiProviderService>,
    pool: PgPool,
) {
    while let Some(job) = rx.recv().await {
        classify(job, &search, &ai, &pool).await;
    }
}

async fn classify(job: Job, search: &SearchService, ai: &AiProviderService, pool: &PgPool) {
    // Switched off since the job was queued: drop it unsent.
    let settings = search.settings().await;
    if !(settings.sentiment_enabled && settings.analytics_enabled) {
        return;
    }

    let Ok(Some(resolved)) = ai.resolve_provider(AiOperationType::Chat, None).await else {
        return;
    };
    let site_name = SiteConfig::site_name(pool)
        .await
        .unwrap_or_else(|_| "Trovato".to_string());
    let site_slogan = SiteConfig::site_slogan(pool).await.unwrap_or_default();
    let system_prompt = prompts::resolve(prompts::CLASSIFY_SENTIMENT, &site_name, &site_slogan);
    let user_prompt = format!(
        "Previous answer:\n{}\n\nVisitor's follow-up:\n{}",
        job.previous_answer, job.question
    );

    // Through the send path, so the classification is held to the anonymous
    // cap and recorded in `ai_usage_log` like any other call.
    let Ok(reply) = crate::routes::api_search::send_prompt(
        ai,
        &resolved,
        &classifier_caller(),
        &system_prompt,
        &user_prompt,
    )
    .await
    else {
        return;
    };

    match parse_label(&reply) {
        Some(label) => search.analytics().count(&settings, label),
        None => tracing::warn!(
            reply_chars = reply.chars().count(),
            "sentiment classifier answered with something other than one of its three labels"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_three_labels_count() {
        assert_eq!(parse_label("unmet"), Some(Counter::Unmet));
        assert_eq!(parse_label(" Satisfied.\n"), Some(Counter::Satisfied));
        assert_eq!(parse_label("CONFUSED"), Some(Counter::Confused));
        assert_eq!(parse_label("unmet, mostly"), None);
        assert_eq!(parse_label(""), None);
        assert_eq!(parse_label("happy"), None);
    }

    #[test]
    fn messages_are_clipped() {
        assert_eq!(clip(&"x".repeat(5000)).chars().count(), MAX_MESSAGE_CHARS);
    }
}
