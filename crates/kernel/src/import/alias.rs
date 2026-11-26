//! URL aliases and redirects, with the ids and dates the file gives.
//!
//! Both are checked as their admin forms check them: an alias's paths
//! normalised as `UrlAliasService` stores them, a redirect's source
//! canonicalised and its destination and status code refused as
//! `Redirect::create` refuses them. Aliases are on the live stage, where
//! every alias the forms make on it is.

use anyhow::{Context, Result};
use uuid::Uuid;

use super::record::{AliasRecord, RedirectRecord};
use super::{Outcome, Run, require_id, require_language, require_order};
use crate::models::stage::LIVE_STAGE_ID;
use crate::services::redirect::{canonicalize_source, validate_redirect_destination};
use crate::services::url_alias::normalize_path;

/// A stored alias, as a record would describe it.
#[derive(Debug, PartialEq, sqlx::FromRow)]
struct StoredAlias {
    source: String,
    alias: String,
    language: String,
    created: i64,
    changed: i64,
}

/// Import one `alias` record.
pub(crate) async fn import_alias(run: &mut Run<'_>, r: AliasRecord) -> Result<Outcome> {
    require_id(r.id, "id")?;
    require_order(r.created, r.changed)?;
    let source = normalize_path(&r.source);
    let alias = normalize_path(&r.alias);
    if source == "/" || alias == "/" {
        anyhow::bail!("source and alias are both required");
    }
    let language = r
        .language
        .clone()
        .unwrap_or_else(|| run.default_language.clone());
    require_language(run, &language).await?;

    let wanted = StoredAlias {
        source,
        alias,
        language,
        created: r.created,
        changed: r.changed,
    };
    let stored: Option<StoredAlias> = sqlx::query_as(
        "SELECT source, alias, language, created, changed FROM url_alias \
         WHERE id = $1 FOR UPDATE",
    )
    .bind(r.id)
    .fetch_optional(&mut *run.tx)
    .await
    .context("failed to look up the alias")?;
    if stored.as_ref() == Some(&wanted) {
        return Ok(Outcome::Unchanged);
    }

    // An address serves one page per language: the unique index would refuse
    // the write, and saying which alias holds it is more use.
    let holder: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM url_alias WHERE alias = $1 AND language = $2 AND stage_id = $3 \
         AND id <> $4",
    )
    .bind(&wanted.alias)
    .bind(&wanted.language)
    .bind(LIVE_STAGE_ID)
    .bind(r.id)
    .fetch_optional(&mut *run.tx)
    .await
    .context("failed to look up the address")?;
    if let Some(holder) = holder {
        anyhow::bail!(
            "the address {} ({}) is already alias {holder}",
            wanted.alias,
            wanted.language
        );
    }

    if stored.is_some() {
        sqlx::query(
            "UPDATE url_alias SET source = $2, alias = $3, language = $4, created = $5, \
             changed = $6 WHERE id = $1",
        )
        .bind(r.id)
        .bind(&wanted.source)
        .bind(&wanted.alias)
        .bind(&wanted.language)
        .bind(wanted.created)
        .bind(wanted.changed)
        .execute(&mut *run.tx)
        .await
        .context("failed to update the alias")?;
        return Ok(Outcome::Updated);
    }
    sqlx::query(
        "INSERT INTO url_alias (id, source, alias, language, stage_id, created, changed) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(r.id)
    .bind(&wanted.source)
    .bind(&wanted.alias)
    .bind(&wanted.language)
    .bind(LIVE_STAGE_ID)
    .bind(wanted.created)
    .bind(wanted.changed)
    .execute(&mut *run.tx)
    .await
    .context("failed to insert the alias")?;
    Ok(Outcome::Created)
}

/// A stored redirect, as a record would describe it.
#[derive(Debug, PartialEq, sqlx::FromRow)]
struct StoredRedirect {
    source: String,
    destination: String,
    status_code: i16,
    language: String,
    created: i64,
}

/// Import one `redirect` record. The table is the `trovato_redirects`
/// plugin's; a site without it refuses the record by name.
pub(crate) async fn import_redirect(run: &mut Run<'_>, r: RedirectRecord) -> Result<Outcome> {
    require_id(r.id, "id")?;
    let table: Option<String> = sqlx::query_scalar("SELECT to_regclass('redirect')::text")
        .fetch_one(&mut *run.tx)
        .await
        .context("failed to look for the redirect table")?;
    if table.is_none() {
        anyhow::bail!("redirects need the trovato_redirects plugin, which is not installed");
    }
    let source = canonicalize_source(&r.source)?;
    if !validate_redirect_destination(&r.destination) {
        anyhow::bail!("invalid redirect destination: must be a relative path or http(s) URL");
    }
    let status_code = r.status_code.unwrap_or(301);
    if !matches!(status_code, 301 | 302 | 303 | 307 | 308) {
        anyhow::bail!(
            "invalid redirect status code {status_code}: must be 301, 302, 303, 307, or 308"
        );
    }
    // As on the redirect form: no language is every language (`""`).
    let language = r.language.clone().unwrap_or_default();
    if !language.is_empty() {
        require_language(run, &language).await?;
    }

    let wanted = StoredRedirect {
        source,
        destination: r.destination.clone(),
        status_code,
        language,
        created: r.created,
    };
    let stored: Option<StoredRedirect> = sqlx::query_as(
        "SELECT source, destination, status_code, language, created FROM redirect \
         WHERE id = $1 FOR UPDATE",
    )
    .bind(r.id)
    .fetch_optional(&mut *run.tx)
    .await
    .context("failed to look up the redirect")?;
    if stored.as_ref() == Some(&wanted) {
        return Ok(Outcome::Unchanged);
    }
    if stored.is_some() {
        sqlx::query(
            "UPDATE redirect SET source = $2, destination = $3, status_code = $4, \
             language = $5, created = $6 WHERE id = $1",
        )
        .bind(r.id)
        .bind(&wanted.source)
        .bind(&wanted.destination)
        .bind(wanted.status_code)
        .bind(&wanted.language)
        .bind(wanted.created)
        .execute(&mut *run.tx)
        .await
        .context("failed to update the redirect")?;
        return Ok(Outcome::Updated);
    }
    sqlx::query(
        "INSERT INTO redirect (id, source, destination, status_code, language, created) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(r.id)
    .bind(&wanted.source)
    .bind(&wanted.destination)
    .bind(wanted.status_code)
    .bind(&wanted.language)
    .bind(wanted.created)
    .execute(&mut *run.tx)
    .await
    .context("failed to insert the redirect")?;
    Ok(Outcome::Created)
}
