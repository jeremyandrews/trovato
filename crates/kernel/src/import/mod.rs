//! `trovato import`: content from another site, with its ids and dates.
//!
//! A newline delimited JSON file, one record per line, written into the site
//! in one transaction, each record validated as the kernel's own forms
//! validate it. The format, the rules and the reasons are in
//! `docs/design/import.md`.
//!
//! Choosing a record's id and its dates is a migration power. It lives here
//! and nowhere else: the statements that write a caller's id and dates are
//! private to this module's children, no input struct elsewhere gained an id
//! or a date, and the only way in is the `trovato import` command. No route,
//! API or plugin host function calls [`import`].

mod alias;
mod comment;
mod file;
mod item;
mod record;
mod tile;
mod user;

use std::collections::BTreeMap;
use std::io::BufRead;

use anyhow::{Context, Result};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::cache::generation::CacheKind;
use crate::state::AppState;
use crate::tap::UserContext;

pub use record::RECORD_TYPES;
use record::Record;

/// How many records of one type an import created, updated and left alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Absent before, written now.
    pub created: usize,
    /// Stored and different: changed to what the file says.
    pub updated: usize,
    /// Stored and identical: nothing written.
    pub unchanged: usize,
}

impl Counts {
    /// Records this import wrote.
    pub fn changes(&self) -> usize {
        self.created + self.updated
    }
}

/// What an import did, or with `dry_run`, would have done.
#[derive(Debug, Clone, Default)]
pub struct ImportReport {
    /// Whether this was `--dry-run`: everything checked, nothing kept.
    pub dry_run: bool,
    /// Records read (blank and `#` lines not counted).
    pub records: usize,
    /// Per record type, in the order of [`RECORD_TYPES`] when printed.
    pub counts: BTreeMap<&'static str, Counts>,
    /// What telling the running servers came to, when anything changed.
    pub cache_note: Option<String>,
}

impl ImportReport {
    /// Records written, of every type.
    pub fn changes(&self) -> usize {
        self.counts.values().map(Counts::changes).sum()
    }

    /// The counts of `record_type`, zero when the file had none.
    pub fn of(&self, record_type: &str) -> Counts {
        self.counts.get(record_type).copied().unwrap_or_default()
    }

    /// The report as the command prints it: one line per record type the
    /// file held.
    pub fn lines(&self) -> Vec<String> {
        RECORD_TYPES
            .iter()
            .filter_map(|t| self.counts.get(t).map(|c| (t, c)))
            .map(|(t, c)| {
                format!(
                    "  {t}: {} created, {} updated, {} unchanged",
                    c.created, c.updated, c.unchanged
                )
            })
            .collect()
    }
}

/// An import refused: the line that failed and why. Nothing was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportFailed {
    /// The file's line number, from 1.
    pub line: usize,
    /// What is wrong with it.
    pub message: String,
}

impl std::fmt::Display for ImportFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "line {}: {} (nothing was imported)",
            self.line, self.message
        )
    }
}

impl std::error::Error for ImportFailed {}

/// What each record is checked and written with.
pub(crate) struct Run<'a> {
    pub(crate) state: &'a AppState,
    pub(crate) tx: Transaction<'static, Postgres>,
    /// The site's default language, for a record that names none.
    pub(crate) default_language: String,
    /// Every text format: the importer writes as an administrator would.
    pub(crate) formats: Vec<String>,
}

/// What became of one record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Created,
    Updated,
    Unchanged,
}

/// Import the records read from `input` (named `source` in messages).
///
/// Every record is checked and written in turn inside one transaction, which
/// commits only after the last line passed; a record that fails rolls all of
/// it back and is returned as an [`ImportFailed`] inside the error. With
/// `dry_run` the transaction always rolls back. After a commit that changed
/// anything, every cache kind is bumped once, so running servers serve the
/// imported content on their next request.
pub async fn import(state: &AppState, input: impl BufRead, dry_run: bool) -> Result<ImportReport> {
    let default_language = crate::models::Language::get_default(state.db())
        .await?
        .map(|l| l.id)
        .unwrap_or_else(|| "en".to_string());
    let operator = UserContext::administrator(Uuid::nil(), Vec::new());
    let mut run = Run {
        state,
        tx: state
            .db()
            .begin()
            .await
            .context("failed to start the import")?,
        default_language,
        formats: crate::content::submission::permitted_text_formats(&operator),
    };
    let mut report = ImportReport {
        dry_run,
        ..ImportReport::default()
    };

    for (index, line) in input.lines().enumerate() {
        let number = index + 1;
        let fail = |message: String| ImportFailed {
            line: number,
            message,
        };
        let line = line.map_err(|e| fail(format!("could not be read: {e}")))?;
        let text = line.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let record: Record = serde_json::from_str(text).map_err(|e| fail(e.to_string()))?;
        let record_type = record.type_name();
        let outcome = apply(&mut run, record)
            .await
            .map_err(|e| fail(format!("{e:#}")))?;
        report.records += 1;
        let counts = report.counts.entry(record_type).or_default();
        match outcome {
            Outcome::Created => counts.created += 1,
            Outcome::Updated => counts.updated += 1,
            Outcome::Unchanged => counts.unchanged += 1,
        }
    }

    if dry_run || report.changes() == 0 {
        run.tx
            .rollback()
            .await
            .context("failed to end the import")?;
        return Ok(report);
    }
    run.tx
        .commit()
        .await
        .context("failed to commit the import")?;

    let summary: BTreeMap<&str, serde_json::Value> = report
        .counts
        .iter()
        .map(|(t, c)| {
            (
                *t,
                serde_json::json!({
                    "created": c.created, "updated": c.updated, "unchanged": c.unchanged
                }),
            )
        })
        .collect();
    state
        .audit_trail()
        .record(
            "import",
            "import",
            Uuid::now_v7(),
            None,
            serde_json::json!({ "counts": summary }),
        )
        .await;

    // The write has happened either way: a Redis that cannot be reached is
    // reported, not turned into a failure.
    report.cache_note = Some(
        match crate::cache::clear_everywhere(state.redis(), &CacheKind::ALL).await {
            Ok(()) => "Running servers have dropped their cached copies; \
                       the next request reads the database."
                .to_string(),
            Err(e) => format!(
                "Running servers were NOT told: Redis could not be reached ({e:#}). \
                 They see this import when their caches expire, or after \
                 `trovato cache clear`."
            ),
        },
    );
    Ok(report)
}

/// Check and write one record.
async fn apply(run: &mut Run<'_>, record: Record) -> Result<Outcome> {
    match record {
        Record::Item(r) => item::import_item(run, r).await,
        Record::Revision(r) => item::import_revision(run, r).await,
        Record::Comment(r) => comment::import_comment(run, r).await,
        Record::Alias(r) => alias::import_alias(run, r).await,
        Record::Redirect(r) => alias::import_redirect(run, r).await,
        Record::User(r) => user::import_user(run, r).await,
        Record::Tile(r) => tile::import_tile(run, r).await,
        Record::File(r) => file::import_file(run, r).await,
    }
}

/// Refuse a nil id: the record's id is its identity, and nil is nobody's.
pub(crate) fn require_id(id: Uuid, what: &str) -> Result<()> {
    if id.is_nil() {
        anyhow::bail!("{what} must not be the nil UUID");
    }
    Ok(())
}

/// Refuse a flag outside 0 and 1.
pub(crate) fn require_flag(value: i16, what: &str) -> Result<()> {
    if !(0..=1).contains(&value) {
        anyhow::bail!("{what} must be 0 or 1, not {value}");
    }
    Ok(())
}

/// Refuse a `changed` before `created`.
pub(crate) fn require_order(created: i64, changed: i64) -> Result<()> {
    if changed < created {
        anyhow::bail!("changed ({changed}) is before created ({created})");
    }
    Ok(())
}

/// Refuse a user id that is neither stored nor on an earlier line.
pub(crate) async fn require_user(run: &mut Run<'_>, id: Uuid, what: &str) -> Result<()> {
    let found: Option<i32> = sqlx::query_scalar("SELECT 1 FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *run.tx)
        .await
        .context("failed to look up a user")?;
    if found.is_none() {
        anyhow::bail!("{what} {id} is not a user (stored, or on an earlier line)");
    }
    Ok(())
}

/// Refuse a language the site does not have.
pub(crate) async fn require_language(run: &mut Run<'_>, language: &str) -> Result<()> {
    let found: Option<i32> = sqlx::query_scalar("SELECT 1 FROM language WHERE id = $1")
        .bind(language)
        .fetch_optional(&mut *run.tx)
        .await
        .context("failed to look up a language")?;
    if found.is_none() {
        anyhow::bail!("language '{language}' is not one of the site's languages");
    }
    Ok(())
}
