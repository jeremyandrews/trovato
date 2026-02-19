//! User model and CRUD operations.

use anyhow::{Context, Result};
use argon2::password_hash::SaltString;
use argon2::password_hash::rand_core::OsRng;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Anonymous user UUID (nil UUID).
pub const ANONYMOUS_USER_ID: Uuid = Uuid::nil();

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
}

/// Input for creating a new user.
#[derive(Debug, Deserialize)]
pub struct CreateUser {
    pub name: String,
    pub password: String,
    pub mail: String,
    pub is_admin: bool,
}

/// Input for updating a user.
#[derive(Debug, Deserialize)]
pub struct UpdateUser {
    pub name: Option<String>,
    pub mail: Option<String>,
    pub is_admin: Option<bool>,
    pub status: Option<i16>,
    pub timezone: Option<String>,
    pub language: Option<String>,
    pub data: Option<serde_json::Value>,
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

    /// Find a user by username.
    pub async fn find_by_name(pool: &PgPool, name: &str) -> Result<Option<Self>> {
        let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE name = $1")
            .bind(name)
            .fetch_optional(pool)
            .await
            .context("failed to fetch user by name")?;

        Ok(user)
    }

    /// Find a user by email.
    pub async fn find_by_mail(pool: &PgPool, mail: &str) -> Result<Option<Self>> {
        let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE mail = $1")
            .bind(mail)
            .fetch_optional(pool)
            .await
            .context("failed to fetch user by mail")?;

        Ok(user)
    }

    /// Create a new user.
    pub async fn create(pool: &PgPool, input: CreateUser) -> Result<Self> {
        let id = Uuid::now_v7();
        let pass = hash_password(&input.password)?;

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

    /// Update a user.
    pub async fn update(pool: &PgPool, id: Uuid, input: UpdateUser) -> Result<Option<Self>> {
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
            // Nothing to update, just return the user
            return Self::find_by_id(pool, id).await;
        }

        query.push_str(&params.join(", "));
        query.push_str(&format!(" WHERE id = ${param_idx} RETURNING *"));

        // Use a simpler approach - fetch and update
        // In production, you'd use a query builder like sea-query
        let mut query_builder = sqlx::query_as::<_, User>(&query);

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

        let user = query_builder
            .fetch_optional(pool)
            .await
            .context("failed to update user")?;

        Ok(user)
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
        // Prevent deletion of anonymous user
        if id == ANONYMOUS_USER_ID {
            anyhow::bail!("cannot delete anonymous user");
        }

        let result = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to delete user")?;

        Ok(result.rows_affected() > 0)
    }

    /// Verify a password against this user's hash.
    pub fn verify_password(&self, password: &str) -> bool {
        if self.pass.is_empty() {
            return false;
        }

        let Ok(parsed_hash) = PasswordHash::new(&self.pass) else {
            return false;
        };

        Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok()
    }
}

/// Hash a password using Argon2id.
fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();

    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("failed to hash password: {e}"))?;

    Ok(hash.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_anonymous_user_id() {
        assert_eq!(ANONYMOUS_USER_ID, Uuid::nil());
    }

    #[test]
    fn test_password_hashing() {
        let password = "test_password_123";
        let hash = hash_password(password).unwrap();

        // Hash should start with Argon2 identifier
        assert!(hash.starts_with("$argon2"));

        // Verify should work
        let parsed = PasswordHash::new(&hash).unwrap();
        assert!(
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        );

        // Wrong password should fail
        assert!(
            Argon2::default()
                .verify_password(b"wrong_password", &parsed)
                .is_err()
        );
    }
}
