//! `trovato generate`: a deterministic site of a requested size.
//!
//! For performance tests and benchmarks against large sites (BG14). The same
//! options always write the same rows: every id, title, body, date, author and
//! tag assignment is derived from the seed and the row number, in SQL, so
//! Postgres does the work and this binary only sends statements.
//!
//! What it writes into `DATABASE_URL`:
//!
//! - `--users` members (`gen_user_<n>`, mail `gen_user_<n>@example.invalid`)
//!   and one administrator, `gen_admin`, every one with `--admin-password`;
//! - `--tags` terms in the kernel's `tags` vocabulary (`topic-<n>`);
//! - `--items` published `blog` posts (the `trovato_blog` plugin's type), each
//!   with a body, two tags and one revision, newest first by number;
//! - `--comments` published comments, spread over the posts newest first;
//! - the site marked installed, its front page set to `/blog`.
//!
//! The blog plugin's own gathers give the site its listing (`/blog`), term
//! pages (`/blog/tag/<slug>`) and feeds. Search vectors are built by the
//! item trigger as each post is written. Comments are written with the
//! comment triggers that update their post off: their text is not in the
//! posts' search vectors, and each post is written with its comment count.
//!
//! It does nothing when the same site is already there, and refuses any other
//! database that is not empty: a different generated site, a run that did not
//! finish, or a real site.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use sqlx::PgPool;

/// What to generate.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct GenerateOptions {
    /// Blog posts.
    pub items: u64,
    /// Comments, in total.
    pub comments: u64,
    /// Terms in the `tags` vocabulary.
    pub tags: u64,
    /// Members (the administrator is extra).
    pub users: u64,
    /// The seed every value is derived from.
    pub seed: u64,
    /// Rows per statement. Shapes no row, so not part of the marker.
    #[serde(skip)]
    pub batch: u64,
    /// Statements in flight at once. Shapes no row, so not part of the marker.
    #[serde(skip)]
    pub parallel: usize,
}

/// The `site_config` key recording what was generated.
const MARKER: &str = "generated_site";

/// The words titles and bodies are made of.
const WORDS: [&str; 64] = [
    "river", "stone", "lantern", "harbor", "meadow", "copper", "willow", "signal", "garden",
    "orbit", "canvas", "thunder", "pepper", "violet", "bridge", "falcon", "marble", "ember",
    "glacier", "orchard", "compass", "velvet", "summit", "pixel", "cedar", "anchor", "prism",
    "saffron", "tundra", "echo", "quarry", "nimbus", "juniper", "basalt", "cobalt", "ripple",
    "sparrow", "timber", "zephyr", "lagoon", "beacon", "cinder", "dune", "fjord", "grove",
    "hollow", "island", "jasper", "kestrel", "linen", "mosaic", "nectar", "onyx", "pebble",
    "quill", "raven", "sable", "thistle", "umber", "valley", "wander", "yarrow", "zenith", "atlas",
];

/// Paragraphs a body is assembled from.
const PARAGRAPHS: [&str; 8] = [
    "A generated post for performance testing. Every word here was chosen by its number, so the same seed always writes the same site.",
    "Large sites behave differently from small ones: indexes grow, caches miss, and a query that was free at a thousand rows costs something at a hundred million.",
    "This paragraph exists to give the body some length, so pages, feeds and search results carry text of a realistic size.",
    "Nothing in this site is real. The authors are numbered, the tags are numbered, and the comments say what number they are.",
    "Trovato renders this through the same templates a real post uses, which is the point: the benchmark measures the real path.",
    "Search finds these posts by their words. Each title has three words from a fixed list, so a search for one of them matches about a twentieth of the site.",
    "Comments are spread over the newest posts first, the way readers spread them, so the front page carries threads.",
    "The last paragraph of a generated post. The next post is one number further and one minute older.",
];

/// The SQL array literal of `values`, quoted.
fn sql_array(values: &[&str]) -> String {
    let quoted: Vec<String> = values
        .iter()
        .map(|v| format!("'{}'", v.replace('\'', "''")))
        .collect();
    format!("ARRAY[{}]", quoted.join(","))
}

/// One phase's timing.
#[derive(Debug, Clone)]
pub struct PhaseReport {
    /// What was written.
    pub name: &'static str,
    /// How many rows.
    pub rows: u64,
    /// How long it took.
    pub seconds: f64,
}

/// What a run did.
#[derive(Debug, Clone, Default)]
pub struct GenerateReport {
    /// Each phase, in order.
    pub phases: Vec<PhaseReport>,
    /// Whether the site was already there and nothing was written.
    pub already_present: bool,
}

impl GenerateReport {
    /// Total seconds.
    pub fn total_seconds(&self) -> f64 {
        self.phases.iter().map(|p| p.seconds).sum()
    }
}

/// The deterministic id of row `n` of `kind`, as SQL over an integer column `n`.
fn id_sql(seed: u64, kind: &str, n: &str) -> String {
    format!("md5('{seed}:{kind}:' || ({n})::text)::uuid")
}

/// Generate the site described by `options` into `pool`'s database.
///
/// The schema must be migrated and the `trovato_blog` plugin installed and
/// enabled (`AppState::new` does both on a fresh database).
pub async fn generate(
    pool: &PgPool,
    options: &GenerateOptions,
    admin_password: &str,
    progress: impl Fn(&str) + Send + Sync,
) -> Result<GenerateReport> {
    if options.items == 0 {
        bail!("--items must be at least 1");
    }
    // An even number of tags keeps a post's two tags apart: the second is
    // `1 + (11n + 3) % tags` against `1 + 7n % tags`, equal only when
    // `tags` divides the odd `4n + 3`.
    if options.users == 0 || options.tags < 2 || !options.tags.is_multiple_of(2) {
        bail!("--users must be at least 1 and --tags an even number of at least 2");
    }
    if admin_password.chars().count() < 12 {
        bail!("--admin-password must be at least 12 characters");
    }
    let wanted = serde_json::to_value(options)?;
    let existing: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT value FROM site_config WHERE key = $1")
            .bind(MARKER)
            .fetch_optional(pool)
            .await?;
    if let Some(existing) = existing {
        if existing == wanted {
            return Ok(GenerateReport {
                already_present: true,
                ..GenerateReport::default()
            });
        }
        bail!(
            "this database already holds a generated site ({existing}); generate into an empty one"
        );
    }
    // Anything else in the database is a real site, or a run that did not
    // finish (the marker is written last): never write into either. The
    // anonymous user is the one row a migrated database starts with.
    let has_content: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM item) OR EXISTS (SELECT 1 FROM comment) \
         OR EXISTS (SELECT 1 FROM users WHERE id <> '00000000-0000-0000-0000-000000000000')",
    )
    .fetch_one(pool)
    .await?;
    if has_content {
        bail!(
            "this database holds content `trovato generate` did not write, or a run that did \
             not finish; generate into an empty database"
        );
    }
    let blog: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM item_type WHERE type = 'blog')")
            .fetch_one(pool)
            .await?;
    if !blog {
        bail!(
            "the `blog` item type is missing: install and enable the trovato_blog plugin \
             (`trovato plugin install trovato_blog && trovato plugin enable trovato_blog`)"
        );
    }

    let mut report = GenerateReport::default();
    let seed = options.seed;
    let now = 1_767_225_600_i64; // 2026-01-01, so dates do not move between runs.

    // Users and the administrator.
    let started = Instant::now();
    progress("users");
    let pass = crate::models::user::hash_password(admin_password)?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO users (id, name, pass, mail, is_admin, status, created) \
         SELECT {id}, 'gen_user_' || n, $1, 'gen_user_' || n || '@example.invalid', false, 1, \
                to_timestamp($2 - n * 3600) \
         FROM generate_series(1, $3::bigint) n",
        id = id_sql(seed, "user", "n")
    )))
    .bind(&pass)
    .bind(now)
    .bind(i64::try_from(options.users)?)
    .execute(pool)
    .await
    .context("writing users")?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO users (id, name, pass, mail, is_admin, status, created) \
         VALUES ({id}, 'gen_admin', $1, 'gen_admin@example.invalid', true, 1, to_timestamp($2))",
        id = id_sql(seed, "admin", "0")
    )))
    .bind(&pass)
    .bind(now)
    .execute(pool)
    .await
    .context("writing the administrator")?;
    report.phases.push(PhaseReport {
        name: "users",
        rows: options.users + 1,
        seconds: started.elapsed().as_secs_f64(),
    });

    // Tags.
    let started = Instant::now();
    progress("tags");
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO category_tag (id, category_id, label, weight, created, changed, slug) \
         SELECT {id}, 'tags', 'Topic ' || n, 0, $1, $1, 'topic-' || n \
         FROM generate_series(1, $2::bigint) n",
        id = id_sql(seed, "tag", "n")
    )))
    .bind(now)
    .bind(i64::try_from(options.tags)?)
    .execute(pool)
    .await
    .context("writing tags")?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO category_tag_hierarchy (tag_id, parent_id) \
         SELECT {id}, NULL FROM generate_series(1, $1::bigint) n",
        id = id_sql(seed, "tag", "n")
    )))
    .bind(i64::try_from(options.tags)?)
    .execute(pool)
    .await
    .context("writing tag hierarchy")?;
    report.phases.push(PhaseReport {
        name: "tags",
        rows: options.tags,
        seconds: started.elapsed().as_secs_f64(),
    });

    // Items with their revisions, in one statement per batch: the two
    // foreign keys between them are checked at the end of the statement, when
    // both rows exist.
    let words = sql_array(&WORDS);
    let paragraphs = sql_array(&PARAGRAPHS);
    let (items, comments) = (options.items, options.comments);
    let item_sql = format!(
        "WITH src AS ( \
           SELECT n, {item_id} AS id, {rev_id} AS rev, \
             {author} AS author, \
             'Post ' || n || ': ' || w[1 + (n * 7) % 64] || ' ' || w[1 + (n * 13 + 5) % 64] \
               || ' ' || w[1 + (n * 29 + 11) % 64] AS title, \
             $2::bigint - n * 60 AS created, \
             jsonb_build_object( \
               'field_body', jsonb_build_object( \
                 'value', '<p>' || p[1 + n % 8] || '</p><p>' || p[1 + (n / 8) % 8] || '</p><p>' \
                   || w[1 + (n * 3) % 64] || ' ' || w[1 + (n * 5 + 1) % 64] || ' ' \
                   || w[1 + (n * 11 + 2) % 64] || '.</p>', \
                 'format', 'filtered_html'), \
               'field_tags', jsonb_build_array(({tag_a})::text, ({tag_b})::text) \
             ) AS fields, \
             ($3::bigint / $4::bigint + CASE WHEN n <= $3::bigint % $4::bigint THEN 1 ELSE 0 END)::int \
               AS comment_count \
           FROM generate_series($1::bigint, $5::bigint) n, \
                (SELECT {words} AS w, {paragraphs} AS p) lists \
         ), rev AS ( \
           INSERT INTO item_revision (id, item_id, author_id, title, status, fields, created, log) \
           SELECT rev, id, author, title, 1, fields, created, 'generated' FROM src \
         ) \
         INSERT INTO item (id, current_revision_id, type, title, author_id, status, created, \
                           changed, promote, sticky, fields, comment_count, language) \
         SELECT id, rev, 'blog', title, author, 1, created, created, 0, 0, fields, comment_count, 'en' \
         FROM src",
        item_id = id_sql(seed, "item", "n"),
        rev_id = id_sql(seed, "rev", "n"),
        author = id_sql(seed, "user", &format!("1 + n % {}", options.users)),
        tag_a = id_sql(seed, "tag", &format!("1 + (n * 7) % {}", options.tags)),
        tag_b = id_sql(seed, "tag", &format!("1 + (n * 11 + 3) % {}", options.tags)),
    );
    let started = Instant::now();
    run_batches(
        pool,
        "items",
        items,
        options,
        &progress,
        item_sql,
        vec![now, i64::try_from(comments)?, i64::try_from(items)?],
    )
    .await
    .context("writing items")?;
    report.phases.push(PhaseReport {
        name: "items (with one revision each)",
        rows: items,
        seconds: started.elapsed().as_secs_f64(),
    });

    // Comments: comment `n` belongs to post `1 + (n - 1) % items`, so the
    // newest posts have the most. With the comment triggers that write the
    // post off: each would update the post's row once per comment, the
    // search one to add the comment's text and the count one to add one to a
    // `comment_count` the posts were written with already.
    if comments > 0 {
        let started = Instant::now();
        let comment_sql = format!(
            "INSERT INTO comment (id, item_id, author_id, body, body_format, status, created, changed, depth) \
             SELECT {id}, {item}, {author}, \
               '<p>Comment ' || n || ' on post ' || (1 + (n - 1) % $3::bigint) || '.</p>', \
               'filtered_html', 1, $2::bigint - n, $2::bigint - n, 0 \
             FROM generate_series($1::bigint, $4::bigint) n",
            id = id_sql(seed, "comment", "n"),
            item = id_sql(seed, "item", "1 + (n - 1) % $3::bigint"),
            author = id_sql(seed, "user", &format!("1 + (n * 3) % {}", options.users)),
        );
        let extra = vec![now, i64::try_from(items)?];
        let result = async {
            for trigger in POST_TRIGGERS {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "ALTER TABLE comment DISABLE TRIGGER {trigger}"
                )))
                .execute(pool)
                .await?;
            }
            run_batches(
                pool,
                "comments",
                comments,
                options,
                &progress,
                comment_sql,
                extra,
            )
            .await
        }
        .await;
        // Both, whatever happens to either: a trigger left off is a database
        // that drifts.
        let mut enabled = Ok(());
        for trigger in POST_TRIGGERS {
            let r = sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER TABLE comment ENABLE TRIGGER {trigger}"
            )))
            .execute(pool)
            .await;
            if let Err(e) = r
                && enabled.is_ok()
            {
                enabled = Err(e);
            }
        }
        match (result, enabled) {
            (Err(e), Err(enable)) => {
                return Err(e.context(format!(
                    "writing comments; re-enabling the comment triggers failed too ({enable}), \
                     and the next `trovato generate` turns them back on"
                )));
            }
            (result, enabled) => {
                result.context("writing comments")?;
                enabled.context("re-enabling the comment triggers")?;
            }
        }
        report.phases.push(PhaseReport {
            name: "comments",
            rows: comments,
            seconds: started.elapsed().as_secs_f64(),
        });
    }

    // The site itself, and statistics for the planner.
    let started = Instant::now();
    progress("site settings and ANALYZE");
    for (key, value) in [
        ("installed", serde_json::json!(true)),
        ("site_name", serde_json::json!("Generated site")),
        ("site_front_page", serde_json::json!("/blog")),
        (MARKER, wanted),
    ] {
        crate::models::SiteConfig::set(pool, key, value).await?;
    }
    sqlx::query("ANALYZE users, category_tag, item, item_revision, comment")
        .execute(pool)
        .await?;
    report.phases.push(PhaseReport {
        name: "settings and ANALYZE",
        rows: 0,
        seconds: started.elapsed().as_secs_f64(),
    });
    Ok(report)
}

/// The comment triggers that write to the comment's post, off while comments
/// are generated.
const POST_TRIGGERS: [&str; 2] = ["trg_comment_search", "trigger_update_comment_count"];

/// Re-enable the comment triggers a crashed run may have left off. Whether
/// any was off.
pub async fn repair_after_crash(pool: &PgPool) -> Result<bool> {
    let mut repaired = false;
    for trigger in POST_TRIGGERS {
        let disabled: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM pg_trigger WHERE tgname = $1 AND tgenabled = 'D')",
        )
        .bind(trigger)
        .fetch_one(pool)
        .await?;
        if disabled {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER TABLE comment ENABLE TRIGGER {trigger}"
            )))
            .execute(pool)
            .await?;
            repaired = true;
        }
    }
    Ok(repaired)
}

/// Run `sql` for every batch of `1..=total`, `options.parallel` statements at
/// a time. The statement's parameters are the batch's first row number, then
/// `extra` in order, then the batch's last row number.
async fn run_batches(
    pool: &PgPool,
    what: &str,
    total: u64,
    options: &GenerateOptions,
    progress: &(impl Fn(&str) + Send + Sync),
    sql: String,
    extra: Vec<i64>,
) -> Result<()> {
    let batch = options.batch.max(1);
    let next = Arc::new(AtomicU64::new(1));
    let done = Arc::new(AtomicU64::new(0));
    let sql = Arc::new(sql);
    let extra = Arc::new(extra);
    let started = Instant::now();
    let mut workers = tokio::task::JoinSet::new();
    for _ in 0..options.parallel.max(1) {
        let (next, done, sql, extra, pool) = (
            next.clone(),
            done.clone(),
            sql.clone(),
            extra.clone(),
            pool.clone(),
        );
        workers.spawn(async move {
            loop {
                let lo = next.fetch_add(batch, Ordering::Relaxed);
                if lo > total {
                    return Ok::<(), anyhow::Error>(());
                }
                let hi = (lo + batch - 1).min(total);
                let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str().to_owned()))
                    .bind(i64::try_from(lo)?);
                for value in extra.iter() {
                    query = query.bind(*value);
                }
                query.bind(i64::try_from(hi)?).execute(&pool).await?;
                done.fetch_add(hi - lo + 1, Ordering::Relaxed);
            }
        });
    }
    let mut last_report = Instant::now();
    while !workers.is_empty() {
        match tokio::time::timeout(std::time::Duration::from_secs(5), workers.join_next()).await {
            Ok(Some(joined)) => joined.context("a batch worker panicked")??,
            Ok(None) => break,
            Err(_) => {}
        }
        if last_report.elapsed().as_secs() >= 5 {
            last_report = Instant::now();
            let finished = done.load(Ordering::Relaxed);
            let secs = started.elapsed().as_secs_f64().max(0.001);
            #[allow(clippy::cast_precision_loss)]
            let rate = finished as f64 / secs;
            progress(&format!("{what}: {finished} of {total} ({rate:.0} rows/s)"));
        }
    }
    Ok(())
}
