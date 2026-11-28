//! User model and CRUD operations.

use anyhow::{Context, Result};
use argon2::password_hash::SaltString;
use argon2::password_hash::rand_core::OsRng;
use argon2::{Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, PgPool};
use uuid::Uuid;

/// Anonymous user UUID (nil UUID).
pub const ANONYMOUS_USER_ID: Uuid = Uuid::nil();

/// Display name for authored content with no resolvable account: an id that
/// matches no row at all (the anonymous sentinel itself has one — see
/// `20260212000001_create_users.sql` — and resolves to its own `name` without
/// reaching this constant). The one spelling every author-name call site
/// uses, so a theme never has to guess between this and a template's own
/// `default()` fallback.
pub const ANONYMOUS_DISPLAY_NAME: &str = "anonymous";

/// User record.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub name: String,
    #[serde(skip_serializing)]
    pub pass: String,
    pub mail: String,
    pub is_admin: bool,
    pub created: DateTime<Utc>,
    pub access: Option<DateTime<Utc>>,
    pub login: Option<DateTime<Utc>>,
    pub status: i16,
    pub timezone: Option<String>,
    pub language: Option<String>,
    pub data: serde_json::Value,

    /// Whether the user has given consent (GDPR).
    #[serde(default)]
    pub consent_given: Option<bool>,

    /// When consent was recorded.
    #[serde(default)]
    pub consent_date: Option<DateTime<Utc>>,

    /// Privacy policy version consented to.
    #[serde(default)]
    pub consent_version: Option<String>,

    /// User-requested data retention period in days.
    #[serde(default)]
    pub data_retention_days: Option<i32>,
}

/// Input for creating a new user.
#[derive(Debug, Deserialize)]
pub struct CreateUser {
    pub name: String,
    /// The plain password, hashed on insert. Empty for an account with no
    /// password, which cannot sign in with one until a password is set.
    pub password: String,
    pub mail: String,
    pub is_admin: bool,
}

/// Input for updating a user.
#[derive(Debug, Default, Deserialize)]
pub struct UpdateUser {
    pub name: Option<String>,
    pub mail: Option<String>,
    pub is_admin: Option<bool>,
    pub status: Option<i16>,
    pub timezone: Option<String>,
    pub language: Option<String>,
    pub data: Option<serde_json::Value>,
}

/// An account's fields a change to which is a security event, as they were
/// before an update ([`User::update_returning_before`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountBefore {
    /// The username.
    pub name: String,
    /// The e-mail address.
    pub mail: String,
    /// `1` active, `0` blocked.
    pub status: i16,
    /// The superuser flag.
    pub is_admin: bool,
}

impl AccountBefore {
    /// `user`'s fields as they are now.
    pub fn of(user: &User) -> Self {
        Self {
            name: user.name.clone(),
            mail: user.mail.clone(),
            status: user.status,
            is_admin: user.is_admin,
        }
    }
}

impl User {
    /// Check if this is the anonymous user.
    pub fn is_anonymous(&self) -> bool {
        self.id == ANONYMOUS_USER_ID
    }

    /// Check if this user is active.
    pub fn is_active(&self) -> bool {
        self.status == 1
    }

    /// Find a user by ID.
    pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Self>> {
        let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .context("failed to fetch user by id")?;

        Ok(user)
    }

    /// The users among `ids`, in one query. An id with no row is absent.
    pub async fn find_by_ids(pool: &PgPool, ids: &[Uuid]) -> Result<Vec<Self>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = ANY($1)")
            .bind(ids)
            .fetch_all(pool)
            .await
            .context("failed to fetch users by id")
    }

    /// The names of the users among `ids`, in one query.
    ///
    /// An id with no row is absent from the map. Deleted accounts never are:
    /// account deletion reattributes their content to [`ANONYMOUS_USER_ID`],
    /// whose row names it.
    pub async fn names_by_ids(
        pool: &PgPool,
        ids: &[Uuid],
    ) -> Result<std::collections::HashMap<Uuid, String>> {
        if ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let rows: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT id, name FROM users WHERE id = ANY($1)")
                .bind(ids)
                .fetch_all(pool)
                .await
                .context("failed to fetch user names")?;
        Ok(rows.into_iter().collect())
    }

    /// Find a user by username (case-insensitive).
    pub async fn find_by_name(pool: &PgPool, name: &str) -> Result<Option<Self>> {
        let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE LOWER(name) = LOWER($1)")
            .bind(name)
            .fetch_optional(pool)
            .await
            .context("failed to fetch user by name")?;

        Ok(user)
    }

    /// Find a user by email (case-insensitive).
    pub async fn find_by_mail(pool: &PgPool, mail: &str) -> Result<Option<Self>> {
        let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE LOWER(mail) = LOWER($1)")
            .bind(mail)
            .fetch_optional(pool)
            .await
            .context("failed to fetch user by mail")?;

        Ok(user)
    }

    /// Create a new user (active by default).
    pub async fn create<'e, E: sqlx::PgExecutor<'e>>(pool: E, input: CreateUser) -> Result<Self> {
        let id = Uuid::now_v7();
        let pass = stored_password(&input.password)?;

        let user = sqlx::query_as::<_, User>(
            r#"
            INSERT INTO users (id, name, pass, mail, is_admin)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(&input.name)
        .bind(&pass)
        .bind(&input.mail)
        .bind(input.is_admin)
        .fetch_one(pool)
        .await
        .context("failed to create user")?;

        Ok(user)
    }

    /// Create a new user with a specific status.
    ///
    /// Use `status = 0` for inactive accounts pending email verification.
    pub async fn create_with_status<'e, E: sqlx::PgExecutor<'e>>(
        executor: E,
        input: CreateUser,
        status: i16,
    ) -> Result<Self> {
        let pass = stored_password(&input.password)?;
        Self::insert_hashed(executor, input, &pass, status).await
    }

    /// [`Self::create_with_status`] with the password already hashed, so a
    /// caller can hash (slow, by design) before it opens a transaction.
    pub(crate) async fn insert_hashed<'e, E: sqlx::PgExecutor<'e>>(
        executor: E,
        input: CreateUser,
        pass: &str,
        status: i16,
    ) -> Result<Self> {
        let id = Uuid::now_v7();
        let user = sqlx::query_as::<_, User>(
            r#"
            INSERT INTO users (id, name, pass, mail, is_admin, status)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(&input.name)
        .bind(pass)
        .bind(&input.mail)
        .bind(input.is_admin)
        .bind(status)
        .fetch_one(executor)
        .await
        .context("failed to create user")?;

        Ok(user)
    }

    /// Update a user.
    pub async fn update(pool: &PgPool, id: Uuid, input: UpdateUser) -> Result<Option<Self>> {
        let mut conn = pool
            .acquire()
            .await
            .context("failed to acquire a connection to update a user")?;
        Ok(Self::update_returning_before(&mut conn, id, input)
            .await?
            .map(|(user, _)| user))
    }

    /// [`Self::update`] on `conn` (a transaction the caller commits), also
    /// returning the fields a change to which is a security event, as they
    /// were before the update.
    ///
    /// One statement either way: the earlier values come from a subquery the
    /// `UPDATE` joins, which reads the row as it was before the statement
    /// changed it, so recording what changed costs no read of its own.
    ///
    /// The subquery locks the row (`FOR UPDATE`). Without the lock it would
    /// read the statement's snapshot, and an update that waited on a
    /// concurrent one would report the values from before that one committed:
    /// an address the other request had just set, then replaced here, would
    /// appear in no event at all. Locking reads the row as last committed.
    pub async fn update_returning_before(
        conn: &mut sqlx::PgConnection,
        id: Uuid,
        input: UpdateUser,
    ) -> Result<Option<(Self, AccountBefore)>> {
        // Build dynamic update query
        let mut query = String::from("UPDATE users SET ");
        let mut params: Vec<String> = Vec::new();
        let mut param_idx = 1;

        if input.name.is_some() {
            params.push(format!("name = ${param_idx}"));
            param_idx += 1;
        }
        if input.mail.is_some() {
            params.push(format!("mail = ${param_idx}"));
            param_idx += 1;
        }
        if input.is_admin.is_some() {
            params.push(format!("is_admin = ${param_idx}"));
            param_idx += 1;
        }
        if input.status.is_some() {
            params.push(format!("status = ${param_idx}"));
            param_idx += 1;
        }
        if input.timezone.is_some() {
            params.push(format!("timezone = ${param_idx}"));
            param_idx += 1;
        }
        if input.language.is_some() {
            params.push(format!("language = ${param_idx}"));
            param_idx += 1;
        }
        if input.data.is_some() {
            params.push(format!("data = ${param_idx}"));
            param_idx += 1;
        }

        if params.is_empty() {
            // Nothing to update: the account as it is, unchanged.
            let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *conn)
                .await
                .context("failed to fetch user by id")?;
            return Ok(user.map(|u| {
                let before = AccountBefore::of(&u);
                (u, before)
            }));
        }

        query.push_str(&params.join(", "));
        query.push_str(&format!(
            " FROM (SELECT id, name, mail, status, is_admin FROM users WHERE id = ${param_idx} \
             FOR UPDATE) \
             AS before WHERE users.id = before.id \
             RETURNING users.*, before.name AS before_name, before.mail AS before_mail, \
             before.status AS before_status, before.is_admin AS before_is_admin"
        ));

        let mut query_builder = sqlx::query(AssertSqlSafe(query));

        if let Some(ref name) = input.name {
            query_builder = query_builder.bind(name);
        }
        if let Some(ref mail) = input.mail {
            query_builder = query_builder.bind(mail);
        }
        if let Some(is_admin) = input.is_admin {
            query_builder = query_builder.bind(is_admin);
        }
        if let Some(status) = input.status {
            query_builder = query_builder.bind(status);
        }
        if let Some(ref timezone) = input.timezone {
            query_builder = query_builder.bind(timezone);
        }
        if let Some(ref language) = input.language {
            query_builder = query_builder.bind(language);
        }
        if let Some(ref data) = input.data {
            query_builder = query_builder.bind(data);
        }
        query_builder = query_builder.bind(id);

        let Some(row) = query_builder
            .fetch_optional(&mut *conn)
            .await
            .context("failed to update user")?
        else {
            return Ok(None);
        };
        use sqlx::{FromRow, Row};
        let user = User::from_row(&row).context("failed to read the updated user")?;
        let before = AccountBefore {
            name: row.try_get("before_name")?,
            mail: row.try_get("before_mail")?,
            status: row.try_get("before_status")?,
            is_admin: row.try_get("before_is_admin")?,
        };
        Ok(Some((user, before)))
    }

    /// Update the user's password.
    pub async fn update_password(pool: &PgPool, id: Uuid, new_password: &str) -> Result<bool> {
        let pass = hash_password(new_password)?;

        let result = sqlx::query("UPDATE users SET pass = $1 WHERE id = $2")
            .bind(&pass)
            .bind(id)
            .execute(pool)
            .await
            .context("failed to update password")?;

        Ok(result.rows_affected() > 0)
    }

    /// Store `hashed` (from [`hash_password`], made before the caller's
    /// transaction began) as the account's password, on `conn`.
    ///
    /// Returns whether the account had a password before, read in the same
    /// statement (from the row as last committed, locked as
    /// [`Self::update_returning_before`] locks it), or `None` when there is
    /// no such account.
    pub(crate) async fn replace_password<'e, E: sqlx::PgExecutor<'e>>(
        executor: E,
        id: Uuid,
        hashed: &str,
    ) -> Result<Option<bool>> {
        sqlx::query_scalar(
            "UPDATE users SET pass = $1 \
             FROM (SELECT id, pass <> '' AS had_password FROM users WHERE id = $2 FOR UPDATE) \
             AS before \
             WHERE users.id = before.id RETURNING before.had_password",
        )
        .bind(hashed)
        .bind(id)
        .fetch_optional(executor)
        .await
        .context("failed to update password")
    }

    /// Clear the user's password, leaving their passkeys as the way in.
    ///
    /// Whether they may go without one is the caller's to decide
    /// (`AccountAccess::can_remove_password`).
    pub async fn clear_password<'e, E: sqlx::PgExecutor<'e>>(pool: E, id: Uuid) -> Result<bool> {
        let result = sqlx::query("UPDATE users SET pass = '' WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to remove password")?;

        Ok(result.rows_affected() > 0)
    }

    /// Update the user's last access time.
    pub async fn touch_access(pool: &PgPool, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE users SET access = NOW() WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to update access time")?;

        Ok(())
    }

    /// Update the user's last login time.
    pub async fn touch_login(pool: &PgPool, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE users SET login = NOW(), access = NOW() WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to update login time")?;

        Ok(())
    }

    /// List all users.
    pub async fn list(pool: &PgPool) -> Result<Vec<Self>> {
        let users = sqlx::query_as::<_, User>("SELECT * FROM users ORDER BY name")
            .fetch_all(pool)
            .await
            .context("failed to list users")?;

        Ok(users)
    }

    /// One page of the admin people listing, and how many accounts match.
    ///
    /// `like` is an `ILIKE` pattern matched against the name and the email
    /// address, bound as a parameter. `order_by` must come from the listing's
    /// own list of sortable columns (see `routes::admin_list`), never from the
    /// request.
    pub async fn admin_page(
        pool: &PgPool,
        like: Option<&str>,
        order_by: &str,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Self>, i64)> {
        let condition = if like.is_some() {
            " WHERE (name ILIKE $1 OR mail ILIKE $1)"
        } else {
            ""
        };
        let n = usize::from(like.is_some());
        // Ties: by name, then the id; a sort by name needs only the id.
        let ties = if order_by.starts_with("lower(name) ") {
            "id"
        } else {
            "lower(name), id"
        };
        let list_sql = format!(
            "SELECT * FROM users{condition} ORDER BY {order_by}, {ties} \
             LIMIT ${} OFFSET ${}",
            n + 1,
            n + 2
        );
        let count_sql = format!("SELECT COUNT(*) FROM users{condition}");

        let mut list = sqlx::query_as::<_, User>(AssertSqlSafe(list_sql));
        let mut count = sqlx::query_scalar::<_, i64>(AssertSqlSafe(count_sql));
        if let Some(l) = like {
            list = list.bind(l);
            count = count.bind(l);
        }
        let users = list
            .bind(limit)
            .bind(offset)
            .fetch_all(pool)
            .await
            .context("failed to list the admin people page")?;
        let total = count
            .fetch_one(pool)
            .await
            .context("failed to count the admin people page")?;
        Ok((users, total))
    }

    /// List users with pagination.
    pub async fn list_paginated(pool: &PgPool, limit: i64, offset: i64) -> Result<Vec<Self>> {
        let users =
            sqlx::query_as::<_, User>("SELECT * FROM users ORDER BY name LIMIT $1 OFFSET $2")
                .bind(limit)
                .bind(offset)
                .fetch_all(pool)
                .await
                .context("failed to list users")?;

        Ok(users)
    }

    /// Count all users.
    pub async fn count(pool: &PgPool) -> Result<i64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(pool)
            .await
            .context("failed to count users")?;

        Ok(count)
    }

    /// Delete a user.
    pub async fn delete(pool: &PgPool, id: Uuid) -> Result<bool> {
        let mut tx = pool
            .begin()
            .await
            .context("failed to start the account deletion transaction")?;
        let deleted = Self::delete_in(&mut tx, id).await?;
        tx.commit()
            .await
            .context("failed to commit the account deletion")?;
        Ok(deleted)
    }

    /// [`Self::delete`] inside `tx`, which the caller commits.
    pub async fn delete_in(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        id: Uuid,
    ) -> Result<bool> {
        // Prevent deletion of anonymous user
        if id == ANONYMOUS_USER_ID {
            anyhow::bail!("cannot delete anonymous user");
        }

        // Everything that points at this account and is not `ON DELETE CASCADE`
        // has to be dealt with first, or the delete below fails on a foreign key.
        // One transaction, because a half-reattributed account is worse than
        // either outcome.
        Self::reattribute_content(tx, id).await?;

        let result = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&mut **tx)
            .await
            .context("failed to delete user")?;

        Ok(result.rows_affected() > 0)
    }

    /// Hand an account's authored rows to the anonymous author, so the account can
    /// be deleted without taking the content with it.
    ///
    /// # Why reattribute rather than delete
    ///
    /// Content integrity wins. A comment thread with holes in it, or an article
    /// that vanishes because its author closed their account, damages every other
    /// participant's record of a conversation they took part in. The account, its
    /// credentials, its tokens and its sessions go; the writing stays, under the
    /// anonymous author the kernel already uses for content with no owner.
    ///
    /// # Why this is a fix and not only a feature
    ///
    /// `item.author_id`, `item_revision.author_id`, `comment.author_id` and
    /// `file_managed.owner_id` are `NOT NULL REFERENCES users(id)` with **no**
    /// `ON DELETE` clause, which is `NO ACTION`. So before this, deleting any
    /// account that had ever written anything failed on a foreign key — including
    /// through the admin screen, which offered the button and reported "Failed to
    /// delete user". Two nullable references (`config_revision.author_id`,
    /// `stage_deletion.deleted_by`) are cleared instead: those record who did an
    /// administrative act, and attributing that act to the anonymous author would
    /// be a false statement rather than a neutral one.
    ///
    /// # Errors
    ///
    /// Returns the first write error. The caller's transaction is what makes the
    /// partial state impossible.
    pub async fn reattribute_content(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        user_id: Uuid,
    ) -> Result<()> {
        // Authored content: reattributed, because it is part of the site's record.
        for (table, column) in [
            ("item", "author_id"),
            ("item_revision", "author_id"),
            ("comment", "author_id"),
            ("file_managed", "owner_id"),
        ] {
            // Table and column are from this fixed list, never from input.
            let sql = format!("UPDATE {table} SET {column} = $1 WHERE {column} = $2");
            sqlx::query(AssertSqlSafe(sql))
                .bind(ANONYMOUS_USER_ID)
                .bind(user_id)
                .execute(&mut **tx)
                .await
                .with_context(|| format!("failed to reattribute {table}.{column}"))?;
        }

        // Administrative provenance: cleared, not reattributed. "The anonymous
        // author published this revision" would be a claim, and a false one.
        for (table, column) in [
            ("config_revision", "author_id"),
            ("stage_deletion", "deleted_by"),
        ] {
            let sql = format!("UPDATE {table} SET {column} = NULL WHERE {column} = $1");
            sqlx::query(AssertSqlSafe(sql))
                .bind(user_id)
                .execute(&mut **tx)
                .await
                .with_context(|| format!("failed to clear {table}.{column}"))?;
        }

        Ok(())
    }

    /// How many active administrators the site has.
    ///
    /// The anonymous account is excluded: it is a sentinel, not a person who could
    /// log in and undo a mistake.
    pub async fn active_admin_count(pool: &PgPool) -> Result<i64> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM users WHERE is_admin = true AND status = 1 AND id <> $1",
        )
        .bind(ANONYMOUS_USER_ID)
        .fetch_one(pool)
        .await
        .context("failed to count active administrators")?;

        Ok(count)
    }

    /// Verify a password against this user's hash.
    pub fn verify_password(&self, password: &str) -> bool {
        if self.pass.is_empty() {
            return false;
        }

        let Ok(parsed_hash) = PasswordHash::new(&self.pass) else {
            return false;
        };

        argon2_instance()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok()
    }
}

/// A password hash nobody's password matches, built once with the production
/// parameters, to verify against when there is no real hash to check.
///
/// Its only job is to cost exactly what a real verification costs.
#[allow(clippy::expect_used)] // Hashing with hardcoded, valid params cannot fail.
static DUMMY_HASH: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    hash_password(&hex::encode(rand::random::<[u8; 32]>()))
        .expect("hashing a random string with fixed parameters succeeds")
});

impl User {
    /// Verify `password` for a sign-in, doing one full Argon2id verification
    /// whatever the outcome.
    ///
    /// With a user that has a password hash, this is [`User::verify_password`].
    /// With no user (an unknown username, or one the caller has already decided
    /// to refuse, such as a blocked account), or a user with no password, it
    /// verifies against `DUMMY_HASH` and returns `false`. The point is that
    /// the three sign-in failures (no such user, blocked user, wrong password)
    /// take the same time, so response time does not reveal which usernames
    /// exist.
    pub fn verify_password_or_dummy(user: Option<&User>, password: &str) -> bool {
        match user {
            Some(user) if !user.pass.is_empty() => user.verify_password(password),
            _ => {
                verify_against_dummy(password);
                false
            }
        }
    }
}

/// Spend one production-cost Argon2id verification on a hash no password
/// matches. Returns whether it matched, which is never.
fn verify_against_dummy(password: &str) -> bool {
    PasswordHash::new(&DUMMY_HASH)
        .map(|hash| {
            argon2_instance()
                .verify_password(password.as_bytes(), &hash)
                .is_ok()
        })
        .unwrap_or(false)
}

/// Create an Argon2id instance with RFC 9106 recommended parameters.
///
/// Parameters: 64 MiB memory, 3 iterations, 4 lanes of parallelism.
///
/// # Panics
///
/// Panics if the hardcoded Argon2 parameters are invalid (should never happen).
#[allow(clippy::expect_used)] // Infallible: hardcoded params are within valid ranges.
fn argon2_instance() -> Argon2<'static> {
    let params = Params::new(
        64 * 1024, // 64 MiB memory cost (in KiB)
        3,         // 3 iterations (time cost)
        4,         // 4 lanes of parallelism
        None,      // default output length (32 bytes)
    )
    .expect("hardcoded Argon2 params are valid");

    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// Hash a password using Argon2id.
///
/// Also the hash of an OAuth client secret a config import supplies
/// (`config_storage::plugin_config::hash_secret`).
/// What `users.pass` holds for a new account's `password`: its hash, or
/// nothing for an empty one, which is an account with no password (it cannot
/// sign in with one until a password is set, as after `clear_password`). Not
/// the hash of the empty string, which an empty password would match.
pub(crate) fn stored_password(password: &str) -> Result<String> {
    if password.is_empty() {
        return Ok(String::new());
    }
    hash_password(password)
}

pub(crate) fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = argon2_instance();

    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("failed to hash password: {e}"))?;

    Ok(hash.to_string())
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! Tests marked `SECURITY REGRESSION TEST` verify fixes for specific security
    //! findings from Epic 27. Do not remove without security review.

    use super::*;

    /// The dummy hash costs what a real one costs: it is Argon2id with exactly
    /// the production parameters, so verifying against it runs the full
    /// Argon2 path. And it never matches.
    #[test]
    fn the_dummy_verification_runs_production_argon2_and_returns_false() {
        let parsed = PasswordHash::new(&DUMMY_HASH).expect("the dummy hash parses");
        assert_eq!(parsed.algorithm, argon2::Algorithm::Argon2id.ident());
        let params = Params::try_from(&parsed).expect("the dummy hash carries Argon2 params");
        let production = argon2_instance();
        assert_eq!(params.m_cost(), production.params().m_cost());
        assert_eq!(params.t_cost(), production.params().t_cost());
        assert_eq!(params.p_cost(), production.params().p_cost());

        assert!(!User::verify_password_or_dummy(None, "anything"));
        assert!(!User::verify_password_or_dummy(None, ""));
        assert!(!verify_against_dummy("password"));
    }

    #[test]
    fn test_anonymous_user_id() {
        assert_eq!(ANONYMOUS_USER_ID, Uuid::nil());
    }

    // SECURITY REGRESSION TEST — Story 27.4 Finding #1: Argon2id with RFC 9106 params
    #[test]
    fn test_password_hashing() {
        let password = "test_password_123";
        let hash = hash_password(password).unwrap();

        // Hash should use Argon2id (not Argon2i or Argon2d)
        assert!(hash.starts_with("$argon2id$"));

        // Verify params are RFC 9106 recommended, not crate defaults.
        // Hash format: $argon2id$v=19$m=65536,t=3,p=4$...
        assert!(
            hash.contains("m=65536"),
            "memory cost should be 64 MiB (65536 KiB), got: {hash}"
        );
        assert!(
            hash.contains("t=3"),
            "time cost should be 3 iterations, got: {hash}"
        );
        assert!(
            hash.contains("p=4"),
            "parallelism should be 4 lanes, got: {hash}"
        );

        // Verify should work with our configured instance
        let parsed = PasswordHash::new(&hash).unwrap();
        assert!(
            argon2_instance()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        );

        // Wrong password should fail
        assert!(
            argon2_instance()
                .verify_password(b"wrong_password", &parsed)
                .is_err()
        );
    }
}
