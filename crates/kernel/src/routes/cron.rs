//! Cron route handlers.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Serialize;
use tower_sessions::Session;
use tracing::info;

use crate::cron::CronResult;
use crate::state::AppState;

use super::helpers::require_permission;

/// Create the cron router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/cron/{key}", post(run_cron))
        .route("/cron/status", get(cron_status))
}

/// Cron run response.
#[derive(Debug, Serialize)]
pub struct CronResponse {
    pub status: String,
    pub tasks: Option<Vec<String>>,
    pub duration_ms: Option<u64>,
    pub message: Option<String>,
}

/// Await `fut` on a task of its own, so dropping *this* future cancels only the
/// waiting and never the work.
///
/// A request handler's future is dropped as soon as its client disconnects.
/// Anything awaited inline is dropped with it, part way through whatever it was
/// doing; anything spawned is not, because a `JoinHandle` that is dropped stops
/// observing a task, it does not stop the task. Cron needs the second
/// behaviour: a run that is torn up mid-flight leaves claimed queue rows, an
/// unreleased lock, and an orphaned heartbeat behind it.
async fn detached<F>(fut: F) -> Result<F::Output, tokio::task::JoinError>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::spawn(fut).await
}

/// Run cron tasks (protected by secret key).
async fn run_cron(State(state): State<AppState>, Path(key): Path<String>) -> Response {
    // Validate cron key. Resolved once at startup rather than read from the
    // environment on every request to this endpoint.
    let expected_key = &state.runtime().cron_key;
    if &key != expected_key {
        info!(provided_key = %key, "invalid cron key");
        return (
            StatusCode::FORBIDDEN,
            Json(CronResponse {
                status: "error".to_string(),
                tasks: None,
                duration_ms: None,
                message: Some("Invalid cron key".to_string()),
            }),
        )
            .into_response();
    }

    // Run cron on its own task rather than inline.
    //
    // The handler's future is dropped the instant the client that triggered
    // cron hangs up, and the poker allows each run only 30 seconds. Awaiting
    // `run` inline meant that disconnect tore a live cron run apart mid-await:
    // its queue jobs were aborted with their rows still `claimed`, its lock was
    // never released, and the heartbeat task it owned but did not contain was
    // orphaned. Detaching the work means a disconnect cancels only the waiting:
    // the run finishes and releases what it holds.
    info!("cron triggered via HTTP");
    let cron = state.cron().clone();
    let result = match detached(async move { cron.run().await }).await {
        Ok(result) => result,
        Err(e) => CronResult::Failed(format!("cron task did not finish: {e}")),
    };

    match result {
        CronResult::Completed {
            tasks_run,
            duration_ms,
        } => (
            StatusCode::OK,
            Json(CronResponse {
                status: "completed".to_string(),
                tasks: Some(tasks_run),
                duration_ms: Some(duration_ms),
                message: None,
            }),
        )
            .into_response(),
        CronResult::Skipped => (
            StatusCode::OK,
            Json(CronResponse {
                status: "skipped".to_string(),
                tasks: None,
                duration_ms: None,
                message: Some("Another instance is running cron".to_string()),
            }),
        )
            .into_response(),
        CronResult::Failed(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(CronResponse {
                status: "failed".to_string(),
                tasks: None,
                duration_ms: None,
                message: Some(error),
            }),
        )
            .into_response(),
    }
}

/// Cron status response.
#[derive(Debug, Serialize)]
pub struct CronStatusResponse {
    pub last_run: Option<LastRunInfo>,
    pub queue_lengths: QueueLengths,
}

/// Last run information.
#[derive(Debug, Serialize)]
pub struct LastRunInfo {
    pub timestamp: i64,
    pub hostname: String,
    pub result: String,
    pub time_ago: String,
}

/// Queue lengths.
#[derive(Debug, Serialize)]
pub struct QueueLengths {
    pub email_send: u64,
    pub search_reindex: u64,
}

/// Get cron status (admin only).
async fn cron_status(State(state): State<AppState>, session: Session) -> Response {
    // Check admin permission
    if let Err(e) = require_permission(&state, &session, "administer site").await {
        return e;
    }

    // Get last run info
    let last_run = match state.cron().last_run().await {
        Ok(Some(run)) => {
            let now = chrono::Utc::now().timestamp();
            let seconds_ago = now - run.timestamp;
            let time_ago = if seconds_ago < 60 {
                format!("{seconds_ago} seconds ago")
            } else if seconds_ago < 3600 {
                format!("{} minutes ago", seconds_ago / 60)
            } else if seconds_ago < 86400 {
                format!("{} hours ago", seconds_ago / 3600)
            } else {
                format!("{} days ago", seconds_ago / 86400)
            };

            Some(LastRunInfo {
                timestamp: run.timestamp,
                hostname: run.hostname,
                result: run.result,
                time_ago,
            })
        }
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(error = %e, "failed to get cron last run");
            None
        }
    };

    // Get queue lengths
    use crate::cron::Queue;
    let queue = state.cron().queue();
    let email_send = queue.len("email:send").await.unwrap_or(0);
    let search_reindex = queue.len("search:reindex").await.unwrap_or(0);

    Json(CronStatusResponse {
        last_run,
        queue_lengths: QueueLengths {
            email_send,
            search_reindex,
        },
    })
    .into_response()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    /// Work handed to [`detached`] must finish even when the caller waiting on
    /// it goes away.
    ///
    /// This is the cron handler's situation exactly: the poker gives a run 30
    /// seconds, and on a slower run it hangs up and axum drops the handler
    /// future. While the run was awaited inline, that dropped the run itself
    /// mid-await, which is what left claimed queue rows, an unreleased lock and
    /// an orphaned heartbeat behind.
    #[tokio::test]
    async fn work_survives_the_caller_being_dropped() {
        let finished = Arc::new(AtomicBool::new(false));
        let flag = finished.clone();

        {
            let mut waiting = Box::pin(detached(async move {
                tokio::time::sleep(Duration::from_millis(150)).await;
                flag.store(true, Ordering::SeqCst);
            }));
            // Poll once so the task is actually spawned, then walk away, as a
            // disconnecting client does.
            let _ = tokio::time::timeout(Duration::from_millis(10), &mut waiting).await;
        }

        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            finished.load(Ordering::SeqCst),
            "the work was cancelled when its caller was dropped: a cron run must \
             outlive the request that triggered it, or it leaves its lock and its \
             claimed queue rows behind"
        );
    }
}
