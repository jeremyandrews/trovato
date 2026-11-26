//! The lifecycle of item field indexes (B207).
//!
//! A field marked "Filterable and sortable" (`settings.filterable` on its
//! definition) is copied into `item.filter_values` and gets an index and
//! statistics on its reader of that copy ([`field_sql`]). A field that does
//! not say either way is marked when a registered gather filters or sorts on
//! it, and unmarked otherwise.
//!
//! [`reconcile`] compares what is marked with what `item_field_index`
//! records: it rewrites the trigger that keeps the copy current, and queues
//! the difference, a build for a newly marked field, a drop for an unmarked
//! or deleted one, a rebuild for an index that went missing or invalid.
//! [`drain`] does the queued work, one job at a time. A build fills the copy
//! for the type's existing items in 256 slices (row locks only, a few
//! seconds each at a million items), creates the statistics, then the index
//! with `CREATE INDEX CONCURRENTLY`, so writes to `item` never wait for it;
//! a drop uses `DROP INDEX CONCURRENTLY`, [`DROP_GRACE`] after the field is
//! unmarked, and the copy keeps the field until then. Both run in the
//! background ([`spawn_reconcile`]): at startup, after a field or a gather is
//! saved, after a config import, and on every cron run.
//!
//! Gather reads a field from the copy only once its build is done
//! ([`GatherService::refresh_served`]); until then, and for every field that
//! is not marked, it reads `item.fields`: the same answer, without an index.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use sqlx::{AssertSqlSafe, PgPool};
use trovato_sdk::types::{ContentTypeDefinition, FieldDefinition};

use super::GatherService;
use super::field_sql::{self, FieldKind};
use super::handlers::is_safe_identifier;
use crate::content::ContentTypeRegistry;

/// The field setting that marks a field for indexing.
pub const SETTING: &str = "filterable";

/// The `field_name` of a type's row for its GIN over the whole copy.
const TYPE_ROW: &str = "*";

/// The `kind` of that row.
const TYPE_KIND: &str = "type";

/// The `field_name` of a type's row for its index on `created`, and that
/// row's `kind`. A listing sorted newest first whose conditions no field
/// index orders by `created` (a tag, an `OR`, a field not marked) walks it,
/// visiting the type's items only, not every item of every type.
const CREATED_ROW: &str = "+";
const CREATED_KIND: &str = "created";

/// How many slices a fill cuts a type's items into, by the first byte of
/// their id.
const SLICES: i32 = 256;

/// Whether `def` is marked: its own setting when it has one, otherwise
/// whether a registered gather filters or sorts on it.
pub fn is_marked(def: &FieldDefinition, used_by_gathers: bool) -> bool {
    def.settings
        .get(SETTING)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(used_by_gathers)
}

/// Whether `def` states its own setting (as opposed to inheriting the
/// default from the gathers that use it).
pub fn has_own_setting(def: &FieldDefinition) -> bool {
    def.settings
        .get(SETTING)
        .and_then(serde_json::Value::as_bool)
        .is_some()
}

/// Every `(item type, field)` a registered gather filters or sorts on,
/// including exposed and contextual filters and filter groups.
pub fn fields_used_by_gathers(gather: &GatherService) -> HashSet<(String, String)> {
    let mut used = HashSet::new();
    for query in gather.list_queries() {
        let def = &query.definition;
        let Some(item_type) = def.item_type.as_deref() else {
            continue;
        };
        if def.base_table != "item" {
            continue;
        }
        let fields = def
            .all_filters()
            .into_iter()
            .map(|f| f.field.as_str())
            .chain(def.sorts.iter().map(|s| s.field.as_str()));
        for field in fields {
            if let Some(key) = field.strip_prefix("fields.")
                && !key.contains('.')
            {
                used.insert((item_type.to_string(), key.to_string()));
            }
        }
    }
    used
}

/// One marked or indexed field, as the admin pages show it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FieldIndexStatus {
    pub field_name: String,
    pub kind: String,
    pub index_name: String,
    /// queued, building, ready, failed or dropping.
    pub state: String,
    pub error: Option<String>,
    pub build_ms: Option<i64>,
    /// While building: `filling` (copying the field into existing items),
    /// `statistics`, `indexing`, or PostgreSQL's own phase of the index
    /// build; and how far through it, 0 to 100.
    pub phase: Option<String>,
    pub percent: Option<i64>,
}

/// The index state of every field of `item_type` that has one, with build
/// progress: the fill's slices, then `pg_stat_progress_create_index`.
pub async fn statuses(pool: &PgPool, item_type: &str) -> Result<Vec<FieldIndexStatus>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        field_name: String,
        kind: String,
        index_name: String,
        state: String,
        error: Option<String>,
        build_ms: Option<i64>,
        own_phase: Option<String>,
        progress: Option<i32>,
        pg_phase: Option<String>,
        blocks_done: Option<i64>,
        blocks_total: Option<i64>,
        tuples_done: Option<i64>,
        tuples_total: Option<i64>,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT f.field_name, f.kind, f.index_name, f.state, f.error, f.build_ms, \
                f.phase AS own_phase, f.progress, p.phase AS pg_phase, \
                p.blocks_done, p.blocks_total, p.tuples_done, p.tuples_total \
         FROM item_field_index f \
         LEFT JOIN pg_stat_progress_create_index p \
           ON p.index_relid = to_regclass(f.index_name) \
         WHERE f.item_type = $1 AND f.kind <> $2 AND f.kind <> $3 ORDER BY f.field_name",
    )
    .bind(item_type)
    .bind(TYPE_KIND)
    .bind(CREATED_KIND)
    .fetch_all(pool)
    .await
    .context("reading item field index states")?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let building = r.state == "building";
            let (phase, percent) = match (building, r.own_phase.as_deref(), r.pg_phase) {
                (true, Some("filling"), _) => (
                    Some("filling".to_string()),
                    r.progress.map(|p| i64::from(p) * 100 / i64::from(SLICES)),
                ),
                (true, _, Some(pg)) => {
                    let percent =
                        match (r.tuples_total, r.tuples_done, r.blocks_total, r.blocks_done) {
                            (Some(t), Some(d), _, _) if t > 0 => Some(d * 100 / t),
                            (_, _, Some(t), Some(d)) if t > 0 => Some(d * 100 / t),
                            _ => None,
                        };
                    (Some(pg), percent)
                }
                (true, own, None) => (own.map(str::to_string), None),
                _ => (None, None),
            };
            FieldIndexStatus {
                field_name: r.field_name,
                kind: r.kind,
                index_name: r.index_name,
                state: r.state,
                error: r.error,
                build_ms: r.build_ms,
                phase,
                percent,
            }
        })
        .collect())
}

/// What [`reconcile`] queued.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reconciled {
    pub queued_builds: usize,
    pub queued_drops: usize,
    /// Whether the copy trigger was rewritten.
    pub trigger_changed: bool,
}

/// Rewrite the copy trigger if the marked fields changed, queue a build for
/// every marked field without a ready, valid index, and a drop for every
/// index whose field is no longer marked or no longer exists.
pub async fn reconcile(
    pool: &PgPool,
    types: &ContentTypeRegistry,
    gather: &GatherService,
) -> Result<Reconciled> {
    // Both registries are caches whose entries expire, and their lists hold
    // only the entries still cached. Read from an expired cache, every field
    // would look unmarked: every index queued to drop, and the copy trigger
    // rewritten to copy nothing. So reconcile from the database.
    types
        .reload_from_db()
        .await
        .context("reloading content types to reconcile field indexes")?;
    gather
        .reload_from_db()
        .await
        .context("reloading gathers to reconcile field indexes")?;
    reconcile_with(pool, &types.list(), &fields_used_by_gathers(gather)).await
}

/// What a row of `item_field_index` asks for: a field's index, or a type's
/// GIN over the whole copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    Field(FieldKind),
    TypeGin,
    TypeCreated,
}

impl Want {
    fn kind_str(self) -> &'static str {
        match self {
            Self::Field(kind) => kind.as_str(),
            Self::TypeGin => TYPE_KIND,
            Self::TypeCreated => CREATED_KIND,
        }
    }
}

/// [`reconcile`] over the given content types, with `used` the fields
/// registered gathers filter or sort on.
pub async fn reconcile_with(
    pool: &PgPool,
    types: &[ContentTypeDefinition],
    used: &HashSet<(String, String)>,
) -> Result<Reconciled> {
    // (type, field) -> what it needs, for every marked, indexable field, and
    // a type GIN for each type with enough marked text fields to ask it.
    let mut wanted: HashMap<(String, String), Want> = HashMap::new();
    // type -> its marked fields, sorted, for the trigger.
    let mut copied: BTreeMap<String, Vec<(String, FieldKind)>> = BTreeMap::new();
    for def in types {
        if !is_safe_identifier(&def.machine_name) {
            continue;
        }
        let mut texts = 0;
        for field in &def.fields {
            let Some(kind) = FieldKind::of(field) else {
                continue;
            };
            let key = (def.machine_name.clone(), field.field_name.clone());
            if is_marked(field, used.contains(&key)) && is_safe_identifier(&field.field_name) {
                wanted.insert(key, Want::Field(kind));
                copied
                    .entry(def.machine_name.clone())
                    .or_default()
                    .push((field.field_name.clone(), kind));
                if kind == FieldKind::Text {
                    texts += 1;
                }
            }
        }
        if copied.contains_key(&def.machine_name) {
            wanted.insert(
                (def.machine_name.clone(), CREATED_ROW.to_string()),
                Want::TypeCreated,
            );
        }
        if texts >= super::query_builder::CONTAINMENT_MIN_EQUALITIES {
            wanted.insert(
                (def.machine_name.clone(), TYPE_ROW.to_string()),
                Want::TypeGin,
            );
        }
    }
    #[derive(sqlx::FromRow)]
    struct Row {
        item_type: String,
        field_name: String,
        kind: String,
        state: String,
        valid: Option<bool>,
        finished: Option<i64>,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT f.item_type, f.field_name, f.kind, f.state, i.indisvalid AS valid, f.finished \
         FROM item_field_index f \
         LEFT JOIN pg_index i ON i.indexrelid = to_regclass(f.index_name)",
    )
    .fetch_all(pool)
    .await
    .context("reading item field indexes")?;

    // A field keeps being copied for as long as it has a row, its index
    // being dropped included. A server reads a field from the copy until it
    // learns the field is no longer served (a cache bump, at the latest its
    // next cron run), and an unmarked field's row is dropped only after
    // DROP_GRACE, so the copy stays complete for every server still reading
    // it; a field marked again before then has a complete copy already.
    for row in &rows {
        if row.field_name == TYPE_ROW
            || row.field_name == CREATED_ROW
            || !is_safe_identifier(&row.item_type)
            || !is_safe_identifier(&row.field_name)
        {
            continue;
        }
        let Some(kind) = FieldKind::parse(&row.kind) else {
            continue;
        };
        let fields = copied.entry(row.item_type.clone()).or_default();
        if !fields.iter().any(|(name, _)| *name == row.field_name) {
            fields.push((row.field_name.clone(), kind));
        }
    }
    let copied: Vec<(String, Vec<(String, FieldKind)>)> = copied
        .into_iter()
        .map(|(t, mut fields)| {
            fields.sort_by(|a, b| a.0.cmp(&b.0));
            (t, fields)
        })
        .collect();

    let mut out = Reconciled {
        trigger_changed: install_trigger(pool, &copied).await?,
        ..Reconciled::default()
    };

    let now = chrono::Utc::now().timestamp();
    let mut seen = HashSet::new();
    for row in rows {
        let key = (row.item_type.clone(), row.field_name.clone());
        seen.insert(key.clone());
        match wanted.get(&key) {
            None if row.state != "dropping" => {
                sqlx::query(
                    "UPDATE item_field_index SET state = 'dropping', requested = $3, error = NULL, \
                     started = NULL, phase = NULL, progress = NULL \
                     WHERE item_type = $1 AND field_name = $2",
                )
                .bind(&row.item_type)
                .bind(&row.field_name)
                .bind(now)
                .execute(pool)
                .await?;
                out.queued_drops += 1;
            }
            None => {}
            Some(want) => {
                let kind_changed = row.kind != want.kind_str();
                // Ready but gone or invalid (a crashed build leaves an invalid
                // index), building with no build running (the server building
                // it stopped), a drop the field was marked again before, or a
                // build that failed an hour or more ago (its cause may have
                // passed): queue it again.
                let stale = match row.state.as_str() {
                    "ready" => row.valid != Some(true),
                    "building" => !build_running(pool, &row.item_type, &row.field_name).await?,
                    "dropping" => true,
                    "failed" => row.finished.is_none_or(|t| now - t >= RETRY_FAILED_AFTER),
                    _ => false,
                };
                if kind_changed || stale {
                    sqlx::query(
                        "UPDATE item_field_index SET state = 'queued', kind = $3, requested = $4, \
                         error = NULL, started = NULL, finished = NULL, phase = NULL, progress = NULL, \
                         filled = filled AND kind = $3 \
                         WHERE item_type = $1 AND field_name = $2",
                    )
                    .bind(&row.item_type)
                    .bind(&row.field_name)
                    .bind(want.kind_str())
                    .bind(now)
                    .execute(pool)
                    .await?;
                    out.queued_builds += 1;
                }
            }
        }
    }
    for ((item_type, field_name), want) in &wanted {
        if seen.contains(&(item_type.clone(), field_name.clone())) {
            continue;
        }
        let index_name = match want {
            Want::Field(_) => field_sql::index_name(item_type, field_name),
            Want::TypeGin => field_sql::type_index_name(item_type),
            Want::TypeCreated => field_sql::type_created_index_name(item_type),
        };
        sqlx::query(
            "INSERT INTO item_field_index (item_type, field_name, kind, index_name, state, requested) \
             VALUES ($1, $2, $3, $4, 'queued', $5) ON CONFLICT DO NOTHING",
        )
        .bind(item_type)
        .bind(field_name)
        .bind(want.kind_str())
        .bind(index_name)
        .bind(now)
        .execute(pool)
        .await?;
        out.queued_builds += 1;
    }
    Ok(out)
}

/// Install the trigger body that copies `copied`'s fields, unless the one in
/// place already does. Returns whether it changed.
async fn install_trigger(
    pool: &PgPool,
    copied: &[(String, Vec<(String, FieldKind)>)],
) -> Result<bool> {
    let body = field_sql::trigger_body_sql(copied);
    let current: Option<String> = sqlx::query_scalar(
        "SELECT prosrc FROM pg_proc WHERE proname = 'trovato_item_filter_values'",
    )
    .fetch_optional(pool)
    .await
    .context("reading the copy trigger")?;
    if current.as_deref() == Some(body.as_str()) {
        return Ok(false);
    }
    sqlx::query(AssertSqlSafe(field_sql::trigger_function_sql(copied)))
        .execute(pool)
        .await
        .context("installing the copy trigger")?;
    Ok(true)
}

/// Do the queued work: every queued build and drop, one at a time, until
/// none is left. Returns how many it did. Safe to run on several servers at
/// once: each job is claimed by one.
pub async fn drain(pool: &PgPool) -> Result<usize> {
    drain_with(pool, DROP_GRACE).await
}

/// Seconds an unmarked field's index and copy are kept before its drop runs.
///
/// Every server reads a field from the copy until it learns the field is no
/// longer served: at once from the cache bump the change makes, at the
/// latest on its next cron run when Redis is unreachable. The trigger keeps
/// copying the field until its row is gone, so a drop that waits longer
/// than that leaves no server reading a copy that has stopped being kept.
pub const DROP_GRACE: i64 = 600;

/// [`drain`], dropping an unmarked field's index once it has been waiting
/// `grace` seconds (0: at once).
pub async fn drain_with(pool: &PgPool, grace: i64) -> Result<usize> {
    let mut done = 0;
    loop {
        // Claim one job: a queued build, or a drop whose grace has passed.
        let claimed: Option<(String, String, String, String, String)> = sqlx::query_as(
            "UPDATE item_field_index f SET \
               state = CASE WHEN f.state = 'queued' THEN 'building' ELSE f.state END, \
               started = $1 \
             WHERE (f.item_type, f.field_name) = ( \
               SELECT item_type, field_name FROM item_field_index \
               WHERE state = 'queued' OR (state = 'dropping' AND requested <= $1 - $2 \
                 AND (started IS NULL OR started < $1 - 3600)) \
               ORDER BY requested LIMIT 1 FOR UPDATE SKIP LOCKED) \
             RETURNING f.item_type, f.field_name, f.kind, f.state, f.index_name",
        )
        .bind(chrono::Utc::now().timestamp())
        .bind(grace)
        .fetch_optional(pool)
        .await
        .context("claiming an item field index job")?;
        let Some((item_type, field_name, kind, state, index_name)) = claimed else {
            return Ok(done);
        };
        done += 1;
        if state == "dropping" {
            if let Err(e) = drop_one(pool, &item_type, &field_name, &index_name).await {
                tracing::warn!(%item_type, %field_name, error = %e, "dropping a field index failed");
                continue;
            }
            sqlx::query(
                "DELETE FROM item_field_index WHERE item_type = $1 AND field_name = $2 \
                 AND state = 'dropping'",
            )
            .bind(&item_type)
            .bind(&field_name)
            .execute(pool)
            .await?;
            continue;
        }
        let result = build(pool, &item_type, &field_name, &kind).await;
        let now = chrono::Utc::now().timestamp();
        match result {
            Ok(ms) => {
                tracing::info!(%item_type, %field_name, ms, "built field index");
                sqlx::query(
                    "UPDATE item_field_index SET state = 'ready', finished = $3, build_ms = $4, \
                     error = NULL, phase = NULL, progress = NULL \
                     WHERE item_type = $1 AND field_name = $2 AND state = 'building'",
                )
                .bind(&item_type)
                .bind(&field_name)
                .bind(now)
                .bind(ms)
                .execute(pool)
                .await?;
            }
            Err(e) => {
                tracing::warn!(%item_type, %field_name, error = %e, "building a field index failed");
                sqlx::query(
                    "UPDATE item_field_index SET state = 'failed', finished = $3, error = $4, \
                     phase = NULL, progress = NULL \
                     WHERE item_type = $1 AND field_name = $2 AND state = 'building'",
                )
                .bind(&item_type)
                .bind(&field_name)
                .bind(now)
                .bind(format!("{e:#}"))
                .execute(pool)
                .await?;
            }
        }
    }
}

/// Drop one row's index and, for a field, its statistics. The copy keeps
/// the field's old values until the item is next saved or the field is
/// marked again; nothing reads them.
async fn drop_one(
    pool: &PgPool,
    item_type: &str,
    field_name: &str,
    index_name: &str,
) -> Result<()> {
    let drop = field_sql::drop_index_ddl(index_name).context("unsafe index name")?;
    let stats = (field_name != TYPE_ROW && field_name != CREATED_ROW)
        .then(|| field_sql::drop_stats_ddl(item_type, field_name))
        .flatten();
    let mut conn = pool.acquire().await?;
    wait_for_lock(&mut conn, DDL_TRY_LOCK_SQL, None).await?;
    let result = async {
        sqlx::query(AssertSqlSafe(drop)).execute(&mut *conn).await?;
        if let Some(sql) = stats {
            sqlx::query(AssertSqlSafe(sql)).execute(&mut *conn).await?;
        }
        anyhow::Ok(())
    }
    .await;
    let _ = sqlx::query(DDL_UNLOCK_SQL).execute(&mut *conn).await;
    result
}

/// Seconds after a failed build before [`reconcile`] queues it again: a
/// deadlock or a full disk may have passed, and the content type page shows
/// the error meanwhile.
const RETRY_FAILED_AFTER: i64 = 3600;

/// The advisory lock every index or statistics change on `item` holds, on
/// one server or several. `CREATE STATISTICS`, `CREATE INDEX CONCURRENTLY`,
/// `DROP INDEX CONCURRENTLY` and `ANALYZE` each take the table's SHARE
/// UPDATE EXCLUSIVE lock, which conflicts with itself, and a concurrent
/// build of an expression or partial index waits for every other build's
/// snapshot, so two builds at once deadlock. They take turns instead.
const DDL_TRY_LOCK_SQL: &str = "SELECT pg_try_advisory_lock(209, 0)";
const DDL_UNLOCK_SQL: &str = "SELECT pg_advisory_unlock(209, 0)";

/// Take an advisory lock on `conn` by asking again until it is free.
///
/// Not `pg_advisory_lock`, which waits inside a statement: a waiting
/// statement is an open transaction, and `CREATE INDEX CONCURRENTLY` waits
/// for every transaction older than its snapshot. A build holding the lock
/// would wait for the statement waiting for the lock, and PostgreSQL would
/// end one of them with `deadlock detected`.
async fn wait_for_lock(
    conn: &mut sqlx::PgConnection,
    try_sql: &'static str,
    key: Option<&str>,
) -> Result<()> {
    loop {
        let mut query = sqlx::query_scalar::<_, bool>(try_sql);
        if let Some(key) = key {
            query = query.bind(key);
        }
        if query.fetch_one(&mut *conn).await? {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// The advisory lock a build holds for its whole run, on its own connection:
/// the first key is fixed, the second hashes the type and field.
const LOCK_SQL: &str = "SELECT pg_try_advisory_lock(207, hashtext($1 || '.' || $2))";
const UNLOCK_SQL: &str = "SELECT pg_advisory_unlock(207, hashtext($1 || '.' || $2))";

/// Whether some server is building `item_type.field_name` now: its build
/// holds the advisory lock, so taking it fails. A row left `building` by a
/// server that stopped has no holder, and is queued again.
async fn build_running(pool: &PgPool, item_type: &str, field_name: &str) -> Result<bool> {
    let mut conn = pool.acquire().await?;
    let got: bool = sqlx::query_scalar(LOCK_SQL)
        .bind(item_type)
        .bind(field_name)
        .fetch_one(&mut *conn)
        .await?;
    if got {
        sqlx::query(UNLOCK_SQL)
            .bind(item_type)
            .bind(field_name)
            .execute(&mut *conn)
            .await?;
    }
    Ok(!got)
}

/// Record what a build is doing, for the content type page.
async fn set_phase(
    pool: &PgPool,
    item_type: &str,
    field_name: &str,
    phase: &str,
    progress: Option<i32>,
) -> Result<()> {
    sqlx::query(
        "UPDATE item_field_index SET phase = $3, progress = $4 \
         WHERE item_type = $1 AND field_name = $2",
    )
    .bind(item_type)
    .bind(field_name)
    .bind(phase)
    .bind(progress)
    .execute(pool)
    .await?;
    Ok(())
}

/// Build one row's index, holding its advisory lock throughout. For a
/// field: fill the copy for the type's existing items, create the
/// statistics, drop any leftover index of the same name (an interrupted
/// build leaves an invalid one), create the index concurrently, and
/// `ANALYZE` the table so the planner has the statistics. For a type's GIN:
/// the same without the fill and statistics. Returns the build time in
/// milliseconds.
async fn build(pool: &PgPool, item_type: &str, field_name: &str, kind: &str) -> Result<i64> {
    let want = if kind == TYPE_KIND {
        Want::TypeGin
    } else if kind == CREATED_KIND {
        Want::TypeCreated
    } else {
        Want::Field(FieldKind::parse(kind).context("unknown field index kind")?)
    };
    let (create, stats, name) = match want {
        Want::Field(kind) => (
            field_sql::index_ddl(item_type, field_name, kind)
                .context("unsafe field or type name")?,
            field_sql::stats_ddl(item_type, field_name, kind),
            field_sql::index_name(item_type, field_name),
        ),
        Want::TypeGin => (
            field_sql::type_index_ddl(item_type).context("unsafe type name")?,
            None,
            field_sql::type_index_name(item_type),
        ),
        Want::TypeCreated => (
            field_sql::type_created_index_ddl(item_type).context("unsafe type name")?,
            None,
            field_sql::type_created_index_name(item_type),
        ),
    };
    let drop = field_sql::drop_index_ddl(&name).context("unsafe index name")?;

    let mut conn = pool.acquire().await?;
    let got: bool = sqlx::query_scalar(LOCK_SQL)
        .bind(item_type)
        .bind(field_name)
        .fetch_one(&mut *conn)
        .await?;
    anyhow::ensure!(got, "another server is building this index");
    let started = Instant::now();
    let mut ddl_locked = false;
    let result = async {
        if let Want::Field(_) = want {
            fill(pool, item_type, field_name).await?;
        }
        set_phase(pool, item_type, field_name, "waiting", None).await?;
        wait_for_lock(&mut conn, DDL_TRY_LOCK_SQL, None).await?;
        ddl_locked = true;
        if let Want::Field(_) = want {
            set_phase(pool, item_type, field_name, "statistics", None).await?;
            if let Some(stats) = stats {
                sqlx::query(AssertSqlSafe(stats))
                    .execute(&mut *conn)
                    .await
                    .context("creating the field's statistics")?;
            }
        }
        set_phase(pool, item_type, field_name, "indexing", None).await?;
        sqlx::query(AssertSqlSafe(drop))
            .execute(&mut *conn)
            .await
            .context("dropping a leftover index")?;
        sqlx::query(AssertSqlSafe(create))
            .execute(&mut *conn)
            .await
            .context("creating the index")?;
        sqlx::query("ANALYZE item")
            .execute(&mut *conn)
            .await
            .context("analyzing item")?;
        anyhow::Ok(())
    }
    .await;
    if ddl_locked {
        let _ = sqlx::query(DDL_UNLOCK_SQL).execute(&mut *conn).await;
    }
    let _ = sqlx::query(UNLOCK_SQL)
        .bind(item_type)
        .bind(field_name)
        .execute(&mut *conn)
        .await;
    result?;
    Ok(i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX))
}

/// Fill `item.filter_values` for every item of `item_type` whose copy is not
/// what the trigger would write now: the type's marked fields, as recorded
/// in `item_field_index`. One slice of items at a time (by the first byte of
/// the id), each its own statement, so no lock is held for long and the
/// progress shows. Saves while it runs are copied by the trigger.
///
/// One fill covers every marked field of the type, so the rows it covered
/// are recorded as `filled` and the jobs of those fields skip theirs. Fills
/// of one type wait for each other (an advisory lock on the type), on one
/// server or several, so a type is never filled twice at once.
async fn fill(pool: &PgPool, item_type: &str, field_name: &str) -> Result<()> {
    let mut conn = pool.acquire().await?;
    // Another build of the same type may hold the fill; say so meanwhile.
    set_phase(pool, item_type, field_name, "waiting", None).await?;
    wait_for_lock(
        &mut conn,
        "SELECT pg_try_advisory_lock(208, hashtext($1))",
        Some(item_type),
    )
    .await?;
    let result = fill_locked(pool, item_type, field_name).await;
    let _ = sqlx::query("SELECT pg_advisory_unlock(208, hashtext($1))")
        .bind(item_type)
        .execute(&mut *conn)
        .await;
    result
}

async fn fill_locked(pool: &PgPool, item_type: &str, field_name: &str) -> Result<()> {
    let already: bool = sqlx::query_scalar(
        "SELECT filled FROM item_field_index WHERE item_type = $1 AND field_name = $2",
    )
    .bind(item_type)
    .bind(field_name)
    .fetch_optional(pool)
    .await?
    .unwrap_or(false);
    if already {
        return Ok(());
    }
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT field_name, kind FROM item_field_index \
         WHERE item_type = $1 AND state <> 'dropping' AND kind <> $2 ORDER BY field_name",
    )
    .bind(item_type)
    .bind(TYPE_KIND)
    .fetch_all(pool)
    .await?;
    let fields: Vec<(String, FieldKind)> = rows
        .into_iter()
        .filter(|(name, _)| is_safe_identifier(name))
        .filter_map(|(name, kind)| FieldKind::parse(&kind).map(|k| (name, k)))
        .collect();
    // The copy is computed from the row being updated, not from a second
    // scan of `item`: when a save holds a row, this statement waits for it
    // and then re-evaluates on the saved version, so it copies the saved
    // fields. A value from a separate scan would be the one read before the
    // wait, and would overwrite the copy the save's trigger just wrote.
    let copy = field_sql::copy_object_sql("fields", &fields);
    let sql = format!(
        "UPDATE item SET filter_values = {copy} \
         WHERE type = $1 AND id >= $2::uuid AND ($3::uuid IS NULL OR id < $3::uuid) \
           AND filter_values IS DISTINCT FROM {copy}"
    );
    for slice in 0..SLICES {
        set_phase(pool, item_type, field_name, "filling", Some(slice)).await?;
        let lo = format!("{slice:02x}000000-0000-0000-0000-000000000000");
        let hi = (slice + 1 < SLICES)
            .then(|| format!("{:02x}000000-0000-0000-0000-000000000000", slice + 1));
        sqlx::query(AssertSqlSafe(sql.clone()))
            .bind(item_type)
            .bind(&lo)
            .bind(hi)
            .execute(pool)
            .await
            .context("filling the copy column")?;
    }
    let names: Vec<String> = fields.into_iter().map(|(name, _)| name).collect();
    sqlx::query(
        "UPDATE item_field_index SET filled = TRUE WHERE item_type = $1 AND field_name = ANY($2)",
    )
    .bind(item_type)
    .bind(&names)
    .execute(pool)
    .await?;
    Ok(())
}

/// Marked fields of every type whose index is not ready: what the status
/// check at startup and the cron report name.
pub async fn missing(pool: &PgPool) -> Result<Vec<(String, String, String)>> {
    Ok(sqlx::query_as(
        "SELECT f.item_type, f.field_name, f.state FROM item_field_index f \
         LEFT JOIN pg_index i ON i.indexrelid = to_regclass(f.index_name) \
         WHERE f.state <> 'dropping' AND (f.state <> 'ready' OR i.indisvalid IS NOT TRUE) \
         ORDER BY 1, 2",
    )
    .fetch_all(pool)
    .await?)
}

/// The fields of each type Gather may read from the copy: a ready row with
/// a valid index. Returns type -> (fields, whether the type's GIN is ready).
pub async fn served(pool: &PgPool) -> Result<HashMap<String, (HashSet<String>, bool)>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT f.item_type, f.field_name FROM item_field_index f \
         JOIN pg_index i ON i.indexrelid = to_regclass(f.index_name) \
         WHERE f.state = 'ready' AND i.indisvalid",
    )
    .fetch_all(pool)
    .await
    .context("reading served fields")?;
    let mut out: HashMap<String, (HashSet<String>, bool)> = HashMap::new();
    for (item_type, field_name) in rows {
        let entry = out.entry(item_type).or_default();
        if field_name == CREATED_ROW {
            continue;
        }
        if field_name == TYPE_ROW {
            entry.1 = true;
        } else {
            entry.0.insert(field_name);
        }
    }
    Ok(out)
}

/// Run [`reconcile`] then [`drain`] in the background, once at a time per
/// server: a call while one is running asks that one to go round again.
pub fn spawn_reconcile(pool: PgPool, types: Arc<ContentTypeRegistry>, gather: Arc<GatherService>) {
    static RECONCILE: Coalesced = Coalesced::new();
    if !RECONCILE.request() {
        return;
    }
    tokio::spawn(async move {
        loop {
            while RECONCILE.take() {
                if let Err(e) = reconcile_and_drain(&pool, &types, &gather).await {
                    tracing::warn!(error = %e, "item field index reconcile failed");
                }
            }
            if !RECONCILE.finish() {
                break;
            }
        }
    });
}

/// Requests for work that runs once at a time and folds the requests made
/// while it runs into one more round.
///
/// A requester calls [`Self::request`] and starts a worker when it says so.
/// The worker serves requests while [`Self::take`] says there is one, then
/// calls [`Self::finish`], and goes round again when that says a request
/// came in after its last `take`: one made between that `take` and the
/// worker stopping would otherwise see the worker running, start none, and
/// wait for some later request.
pub(crate) struct Coalesced {
    running: AtomicBool,
    again: AtomicBool,
}

impl Coalesced {
    pub(crate) const fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            again: AtomicBool::new(false),
        }
    }

    /// Ask for a round. True when no worker is running and the caller must
    /// start one.
    pub(crate) fn request(&self) -> bool {
        self.again.store(true, Ordering::SeqCst);
        !self.running.swap(true, Ordering::SeqCst)
    }

    /// For the worker: whether a request is waiting, taking it if so.
    pub(crate) fn take(&self) -> bool {
        self.again.swap(false, Ordering::SeqCst)
    }

    /// For the worker, once `take` says nothing is waiting: stop, unless a
    /// request came in meanwhile and no other worker has started for it, in
    /// which case this one carries on (true).
    pub(crate) fn finish(&self) -> bool {
        self.running.store(false, Ordering::SeqCst);
        self.again.load(Ordering::SeqCst) && !self.running.swap(true, Ordering::SeqCst)
    }
}

/// [`reconcile`], then [`drain`], in the caller's task, refreshing what
/// Gather reads from the copy after each: a field stops being read from it
/// as soon as it is unmarked, and starts once its index is built.
pub async fn reconcile_and_drain(
    pool: &PgPool,
    types: &ContentTypeRegistry,
    gather: &GatherService,
) -> Result<usize> {
    let r = reconcile(pool, types, gather).await?;
    if r.queued_builds > 0 || r.queued_drops > 0 || r.trigger_changed {
        tracing::info!(
            builds = r.queued_builds,
            drops = r.queued_drops,
            trigger_changed = r.trigger_changed,
            "queued item field index work"
        );
    }
    gather.refresh_served().await;
    let done = drain(pool).await;
    gather.refresh_served().await;
    done
}

/// A field a gather filters or sorts on that has no ready index, and why.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct UnindexedField {
    pub field_name: String,
    /// `not_marked` (its "Filterable and sortable" setting is off),
    /// `cannot` (long text and structured fields have no index), or the
    /// index's state: `queued`, `building`, `failed`, `dropping`.
    pub reason: String,
}

/// The fields `definition` filters or sorts on that no ready index serves:
/// what the Gather editor warns about. Such a gather still works; on a large
/// site each of these reads every item of the type.
pub async fn unindexed_fields(
    pool: &PgPool,
    types: &ContentTypeRegistry,
    gather: &GatherService,
    definition: &super::QueryDefinition,
) -> Vec<UnindexedField> {
    let Some(item_type) = definition.item_type.as_deref() else {
        return Vec::new();
    };
    if definition.base_table != "item" {
        return Vec::new();
    }
    let Some(def) = types.get(item_type) else {
        return Vec::new();
    };
    let used = fields_used_by_gathers(gather);
    let states = statuses(pool, item_type).await.unwrap_or_default();
    let mut keys: Vec<&str> = definition
        .all_filters()
        .into_iter()
        .map(|f| f.field.as_str())
        .chain(definition.sorts.iter().map(|s| s.field.as_str()))
        .filter_map(|f| f.strip_prefix("fields."))
        .filter(|k| !k.contains('.'))
        .collect();
    keys.sort_unstable();
    keys.dedup();
    let mut out = Vec::new();
    for key in keys {
        let Some(field) = def.fields.iter().find(|f| f.field_name == key) else {
            continue;
        };
        let reason = if FieldKind::of(field).is_none() {
            "cannot".to_string()
        } else if !is_marked(
            field,
            used.contains(&(item_type.to_string(), key.to_string())),
        ) {
            "not_marked".to_string()
        } else {
            match states.iter().find(|s| s.field_name == key) {
                Some(s) if s.state == "ready" => continue,
                Some(s) => s.state.clone(),
                None => "queued".to_string(),
            }
        };
        out.push(UnindexedField {
            field_name: key.to_string(),
            reason,
        });
    }
    out
}

// Tests are allowed to use unwrap/expect freely.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::Coalesced;

    #[test]
    fn a_request_while_the_worker_stops_is_not_lost() {
        let c = Coalesced::new();
        // The first request starts a worker, which serves it.
        assert!(c.request());
        assert!(c.take());
        // The worker finds nothing more waiting...
        assert!(!c.take());
        // ...and before it has stopped, another request comes in: the worker
        // is still running, so the requester starts none.
        assert!(!c.request());
        // The worker must go round again for it rather than stop.
        assert!(c.finish());
        assert!(c.take());
        assert!(!c.take());
        assert!(!c.finish());
        // Stopped: the next request starts a worker again.
        assert!(c.request());
    }

    #[test]
    fn requests_while_running_fold_into_one_round() {
        let c = Coalesced::new();
        assert!(c.request());
        assert!(!c.request());
        assert!(!c.request());
        assert!(c.take());
        assert!(!c.take());
        assert!(!c.finish());
    }
}
