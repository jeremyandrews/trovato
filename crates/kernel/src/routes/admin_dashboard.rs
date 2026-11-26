//! The dashboard's panels: what needs the viewer now, beside the cards that
//! lead to each area.
//!
//! - **Your recent content**: what the viewer wrote last, and how much of it
//!   is not published.
//! - **Waiting for you**: drafts to review and comments to approve.
//! - **Scheduled tasks**: whether the last cron run worked, and plugin queue
//!   jobs that stopped after repeated failures.
//! - **Recent errors**: errors the site log recorded in the last day.
//!
//! A panel is shown only to someone who may open the page it is about, by
//! the same rule as the sidebar (`admin_nav::offered_pages`), and every row
//! links to a page that viewer may open. Numbers are words ("3 drafts to
//! review"), never a bare figure or a colour.

use serde::Serialize;

use crate::i18n::UiMessage;
use crate::state::AppState;
use crate::tap::UserContext;

use super::admin_nav;
use super::admin_search_index::shorten;

/// Most rows a panel lists.
const ROWS: i64 = 5;

/// One line in a panel.
#[derive(Debug, Clone, Serialize)]
pub struct PanelRow {
    /// What the row is: a title or message (data), or a message to translate.
    pub text: UiMessage,
    /// More about it, such as "page, not published".
    pub detail: Option<UiMessage>,
    /// Where it leads, when somewhere.
    pub href: Option<String>,
}

/// One dashboard panel: a section with a heading.
#[derive(Debug, Clone, Serialize)]
pub struct Panel {
    /// Its id suffix: `dashboard-panel-{key}`.
    pub key: &'static str,
    pub title: &'static str,
    /// One sentence that says the state in words.
    pub summary: UiMessage,
    pub rows: Vec<PanelRow>,
    /// A link to the whole list, with its words.
    pub more: Option<(UiMessage, String)>,
}

/// Data shown as data: a title or a log message, never translated.
fn data(text: impl Into<String>) -> UiMessage {
    UiMessage::new("{text}").arg("text", text)
}

fn when(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

fn count(n: i64, one: &str, many: &str) -> UiMessage {
    UiMessage::new(one).plural(many, n)
}

/// The panels `viewer` may see, in order.
pub async fn panels(state: &AppState, viewer: &UserContext) -> Vec<Panel> {
    let mut enabled: Vec<String> = state.enabled_plugins().into_iter().collect();
    enabled.sort();
    let nav = state.plugin_admin_nav();
    let offered = admin_nav::offered_pages(viewer, &enabled, nav.links(), nav.cards());
    let may_open = |href: &str| offered.iter().any(|p| p.href == href);

    let mut panels = Vec::new();
    if let Some(panel) = your_content(state, viewer).await {
        panels.push(panel);
    }
    if let Some(panel) = waiting(
        state,
        viewer,
        may_open("/admin/content/moderated"),
        may_open("/admin/content/comments"),
    )
    .await
    {
        panels.push(panel);
    }
    if may_open("/admin/reports/cron") {
        panels.push(tasks(state, may_open("/admin/reports/dead-jobs")).await);
    }
    if may_open("/admin/reports/log") {
        panels.push(errors(state).await);
    }
    panels
}

/// What the viewer wrote last. Shown to anyone who has written something or
/// may create content. Each row links as the content listing does: the admin
/// edit form for someone who may edit any content, else the item's own edit
/// form or page, whichever the viewer may open.
async fn your_content(state: &AppState, viewer: &UserContext) -> Option<Panel> {
    if !viewer.authenticated {
        return None;
    }
    let (total, unpublished): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COUNT(*) FILTER (WHERE status <> 1) FROM item WHERE author_id = $1",
    )
    .bind(viewer.id)
    .fetch_one(state.db())
    .await
    .unwrap_or((0, 0));
    let may_create = viewer.can("create content")
        || state
            .content_types()
            .list_all()
            .await
            .iter()
            .any(|ct| viewer.can(&format!("create {} content", ct.machine_name)));
    if total == 0 && !may_create {
        return None;
    }
    let ids: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT id FROM item WHERE author_id = $1 ORDER BY changed DESC, id DESC LIMIT $2",
    )
    .bind(viewer.id)
    .bind(ROWS)
    .fetch_all(state.db())
    .await
    .unwrap_or_default();
    let ids: Vec<uuid::Uuid> = ids.into_iter().map(|(id,)| id).collect();
    let mut items = crate::models::Item::find_by_ids(state.db(), &ids)
        .await
        .unwrap_or_default();
    items.sort_by_key(|i| std::cmp::Reverse((i.changed, i.id)));

    let editor = viewer.can("edit any content");
    let mut rows = Vec::new();
    for item in items {
        let href = if editor {
            Some(format!("/admin/content/{}/edit", item.id))
        } else if state
            .items()
            .check_access(&item, "edit", viewer)
            .await
            .unwrap_or(false)
        {
            Some(format!("/item/{}/edit", item.id))
        } else if state
            .items()
            .check_access(&item, "view", viewer)
            .await
            .unwrap_or(false)
        {
            Some(format!("/item/{}", item.id))
        } else {
            continue;
        };
        rows.push(PanelRow {
            text: data(item.title.clone()),
            detail: Some(
                if item.status == 1 {
                    UiMessage::new("{type}, published, changed {time} UTC")
                } else {
                    UiMessage::new("{type}, not published, changed {time} UTC")
                }
                .arg("type", item.item_type.clone())
                .arg("time", when(item.changed)),
            ),
            href,
        });
    }
    let summary = if total == 0 {
        UiMessage::new("You have not written anything yet.")
    } else {
        count(
            total,
            "You have written {count} item; {unpublished} not published.",
            "You have written {count} items; {unpublished} not published.",
        )
        .arg("unpublished", unpublished.to_string())
    };
    Some(Panel {
        key: "content",
        title: "Your recent content",
        summary,
        rows,
        more: None,
    })
}

/// Drafts to review and comments to approve, for whoever may do it.
async fn waiting(
    state: &AppState,
    viewer: &UserContext,
    may_review: bool,
    may_moderate_comments: bool,
) -> Option<Panel> {
    if !may_review && !may_moderate_comments {
        return None;
    }
    let mut rows = Vec::new();
    let mut total = 0;
    if may_review {
        let drafts = crate::models::Item::list_moderated(
            state.db(),
            Some(crate::content::moderation::DRAFT),
            200,
        )
        .await
        .unwrap_or_default();
        let ids: Vec<uuid::Uuid> = drafts.iter().map(|d| d.id).collect();
        let items = crate::models::Item::find_by_ids(state.db(), &ids)
            .await
            .unwrap_or_default();
        let mut n: i64 = 0;
        for item in &items {
            if state
                .items()
                .check_access(item, "view", viewer)
                .await
                .unwrap_or(false)
            {
                n += 1;
            }
        }
        total += n;
        rows.push(PanelRow {
            text: count(n, "{count} draft to review", "{count} drafts to review"),
            detail: None,
            href: Some("/admin/content/moderated?state=draft".to_string()),
        });
    }
    if may_moderate_comments {
        let pending = crate::models::comment::CommentStatus::Pending.as_i16();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM comment WHERE status = $1")
            .bind(pending)
            .fetch_one(state.db())
            .await
            .unwrap_or(0);
        total += n;
        rows.push(PanelRow {
            text: count(
                n,
                "{count} comment waiting for approval",
                "{count} comments waiting for approval",
            ),
            detail: None,
            href: Some(format!("/admin/content/comments?status={pending}")),
        });
    }
    Some(Panel {
        key: "waiting",
        title: "Waiting for you",
        summary: if total == 0 {
            UiMessage::new("Nothing is waiting for you.")
        } else {
            count(
                total,
                "{count} thing waits for you.",
                "{count} things wait for you.",
            )
        },
        rows,
        more: None,
    })
}

/// Whether the last cron run worked, and dead plugin queue jobs. `may_open_dead_jobs`
/// is whether the viewer may open the dead jobs report, by the same rule as
/// every other panel row.
async fn tasks(state: &AppState, may_open_dead_jobs: bool) -> Panel {
    let last = state.cron().last_run().await.ok().flatten();
    let dead: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plugin_queue WHERE status = 'dead'")
        .fetch_one(state.db())
        .await
        .unwrap_or(0);
    let failed = last.as_ref().is_some_and(|run| run.status == "failed");
    let cron = match &last {
        None => UiMessage::new("Cron has not run yet."),
        Some(run) if run.status == "failed" => {
            UiMessage::new("Error: the last cron run, at {time} UTC, failed.")
                .arg("time", when(run.timestamp))
        }
        Some(run) if run.status == "skipped" => UiMessage::new(
            "The last cron run, at {time} UTC, was skipped: another server was running it.",
        )
        .arg("time", when(run.timestamp)),
        Some(run) => UiMessage::new("Cron last ran at {time} UTC and worked.")
            .arg("time", when(run.timestamp)),
    };
    let rows = vec![
        PanelRow {
            text: cron,
            detail: None,
            href: Some("/admin/reports/cron".to_string()),
        },
        PanelRow {
            text: count(
                dead,
                "{count} queued job stopped after failing repeatedly",
                "{count} queued jobs stopped after failing repeatedly",
            ),
            detail: None,
            href: may_open_dead_jobs.then(|| "/admin/reports/dead-jobs".to_string()),
        },
    ];
    Panel {
        key: "tasks",
        title: "Scheduled tasks",
        summary: if failed || dead > 0 {
            UiMessage::new("Something needs attention.")
        } else {
            UiMessage::new("Scheduled tasks are working.")
        },
        rows,
        more: None,
    }
}

/// Errors the site log recorded in the last day, newest first.
async fn errors(state: &AppState) -> Panel {
    let since = chrono::Utc::now().timestamp() - 24 * 60 * 60;
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM system_log WHERE level = 'error' AND created >= $1",
    )
    .bind(since)
    .fetch_one(state.db())
    .await
    .unwrap_or(0);
    let latest: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT created, log_type, message FROM system_log \
         WHERE level = 'error' AND created >= $1 ORDER BY id DESC LIMIT $2",
    )
    .bind(since)
    .bind(ROWS)
    .fetch_all(state.db())
    .await
    .unwrap_or_default();
    let rows = latest
        .into_iter()
        .map(|(created, log_type, message)| PanelRow {
            text: data(shorten(&message, 120)),
            detail: Some(
                UiMessage::new("{type}, {time} UTC")
                    .arg("type", log_type)
                    .arg("time", when(created)),
            ),
            href: Some(format!(
                "/admin/reports/log?level=error&q={}",
                urlencoding::encode(&message.chars().take(60).collect::<String>())
            )),
        })
        .collect();
    Panel {
        key: "errors",
        title: "Recent errors",
        summary: count(
            n,
            "{count} error in the last 24 hours.",
            "{count} errors in the last 24 hours.",
        ),
        rows,
        more: Some((
            UiMessage::new("All errors in the log"),
            "/admin/reports/log?level=error".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_message_is_shortened_with_an_ellipsis() {
        assert_eq!(shorten("one two three four", 10), "one two t…");
    }

    #[test]
    fn a_time_is_written_as_text() {
        assert_eq!(when(0), "1970-01-01 00:00");
    }
}
