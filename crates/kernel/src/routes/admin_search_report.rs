//! Search report: what visitors search for.
//!
//! `GET /admin/reports/search?days=7|30|90`: totals, the top queries, the top
//! queries that found nothing, the expansion cache hit rate and the follow-up
//! sentiment counts, read from the two daily analytics tables. Each list is one
//! aggregate query.

use axum::Router;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use serde::{Deserialize, Serialize};
use tower_sessions::Session;

use crate::search::SearchSettings;
use crate::state::AppState;

use super::helpers::{render_admin_template, render_server_error, require_permission};

/// Periods the screen offers, in days.
const PERIODS: [u32; 3] = [7, 30, 90];

/// Rows per list.
const TOP_N: i64 = 25;

/// Build the router.
pub fn router() -> Router<AppState> {
    Router::new().route("/admin/reports/search", get(report))
}

#[derive(Debug, Deserialize)]
struct ReportQuery {
    days: Option<u32>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
struct QueryRow {
    query: String,
    searches: i64,
    zero_results: i64,
}

#[derive(Debug, Default, Serialize, sqlx::FromRow)]
struct Totals {
    searches: i64,
    zero_results: i64,
}

#[derive(Debug, Default, Serialize, sqlx::FromRow)]
struct DailyTotals {
    expansion_hits: i64,
    expansion_misses: i64,
    satisfied: i64,
    confused: i64,
    unmet: i64,
}

/// A share as a whole percentage, or `None` when there is nothing to divide.
fn percent(part: i64, whole: i64) -> Option<i64> {
    (whole > 0).then(|| (part * 100 + whole / 2) / whole)
}

/// GET /admin/reports/search
async fn report(
    State(state): State<AppState>,
    session: Session,
    Query(params): Query<ReportQuery>,
) -> Response {
    if let Err(redirect) = require_permission(&state, &session, "administer site").await {
        return redirect;
    }

    let settings = SearchSettings::load(state.db(), state.cache()).await;
    let days = params
        .days
        .filter(|d| PERIODS.contains(d))
        .unwrap_or(30)
        .min(settings.analytics_retention_days);
    let periods: Vec<u32> = PERIODS
        .iter()
        .copied()
        .filter(|p| *p <= settings.analytics_retention_days)
        .collect();
    let since = chrono::Utc::now().date_naive() - chrono::Duration::days(i64::from(days) - 1);
    let pool = state.db();

    let totals = sqlx::query_as::<_, Totals>(
        "SELECT COALESCE(SUM(searches), 0)::bigint AS searches, \
                COALESCE(SUM(zero_results), 0)::bigint AS zero_results \
         FROM search_query_daily WHERE day >= $1",
    )
    .bind(since)
    .fetch_one(pool);
    let top = sqlx::query_as::<_, QueryRow>(
        "SELECT query, SUM(searches)::bigint AS searches, SUM(zero_results)::bigint AS zero_results \
         FROM search_query_daily WHERE day >= $1 \
         GROUP BY query ORDER BY searches DESC, query LIMIT $2",
    )
    .bind(since)
    .bind(TOP_N)
    .fetch_all(pool);
    let zero = sqlx::query_as::<_, QueryRow>(
        "SELECT query, SUM(searches)::bigint AS searches, SUM(zero_results)::bigint AS zero_results \
         FROM search_query_daily WHERE day >= $1 \
         GROUP BY query HAVING SUM(zero_results) > 0 \
         ORDER BY zero_results DESC, query LIMIT $2",
    )
    .bind(since)
    .bind(TOP_N)
    .fetch_all(pool);
    let daily = sqlx::query_as::<_, DailyTotals>(
        "SELECT COALESCE(SUM(expansion_hits), 0)::bigint AS expansion_hits, \
                COALESCE(SUM(expansion_misses), 0)::bigint AS expansion_misses, \
                COALESCE(SUM(satisfied), 0)::bigint AS satisfied, \
                COALESCE(SUM(confused), 0)::bigint AS confused, \
                COALESCE(SUM(unmet), 0)::bigint AS unmet \
         FROM search_daily WHERE day >= $1",
    )
    .bind(since)
    .fetch_one(pool);

    let (totals, top, zero, daily) = match tokio::try_join!(totals, top, zero, daily) {
        Ok(all) => all,
        Err(e) => {
            tracing::error!(error = %e, "failed to read search analytics");
            return render_server_error("Failed to read search analytics.");
        }
    };

    let lookups = daily.expansion_hits + daily.expansion_misses;
    let classified = daily.satisfied + daily.confused + daily.unmet;

    let mut context = tera::Context::new();
    context.insert("days", &days);
    context.insert("periods", &periods);
    context.insert("totals", &totals);
    context.insert("zero_share", &percent(totals.zero_results, totals.searches));
    context.insert("top", &top);
    context.insert("zero", &zero);
    context.insert("daily", &daily);
    context.insert("hit_rate", &percent(daily.expansion_hits, lookups));
    context.insert("lookups", &lookups);
    context.insert("classified", &classified);
    context.insert("unmet_share", &percent(daily.unmet, classified));
    context.insert("analytics_enabled", &settings.analytics_enabled);
    context.insert("sentiment_enabled", &settings.sentiment_enabled);
    context.insert("retention_days", &settings.analytics_retention_days);
    context.insert("path", "/admin/reports/search");

    render_admin_template(
        &state,
        &crate::routes::helpers::get_user_context(&session, &state).await,
        "admin/reports/search.html",
        context,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_round_and_survive_an_empty_period() {
        assert_eq!(percent(0, 0), None);
        assert_eq!(percent(1, 3), Some(33));
        assert_eq!(percent(2, 3), Some(67));
        assert_eq!(percent(5, 5), Some(100));
    }
}
