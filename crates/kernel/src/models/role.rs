//! Role and permission models.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Well-known role IDs.
pub mod well_known {
    use uuid::Uuid;

    /// Anonymous user role (assigned to unauthenticated users).
    pub const ANONYMOUS_ROLE_ID: Uuid = Uuid::from_u128(1);

    /// Authenticated user role (assigned to all logged-in users).
    pub const AUTHENTICATED_ROLE_ID: Uuid = Uuid::from_u128(2);
}

/// Role record.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Role {
    pub id: Uuid,
    pub name: String,
    pub created: DateTime<Utc>,
}

impl Role {
    /// Find a role by ID.
    pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Self>> {
        let role = sqlx::query_as::<_, Role>("SELECT * FROM roles WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .context("failed to fetch role by id")?;

        Ok(role)
    }

    /// Find a role by name.
    pub async fn find_by_name(pool: &PgPool, name: &str) -> Result<Option<Self>> {
        let role = sqlx::query_as::<_, Role>("SELECT * FROM roles WHERE name = $1")
            .bind(name)
            .fetch_optional(pool)
            .await
            .context("failed to fetch role by name")?;

        Ok(role)
    }

    /// List all roles.
    pub async fn list(pool: &PgPool) -> Result<Vec<Self>> {
        let roles = sqlx::query_as::<_, Role>("SELECT * FROM roles ORDER BY name")
            .fetch_all(pool)
            .await
            .context("failed to list roles")?;

        Ok(roles)
    }

    /// Create a new role.
    pub async fn create(pool: &PgPool, name: &str) -> Result<Self> {
        let id = Uuid::now_v7();

        let role =
            sqlx::query_as::<_, Role>("INSERT INTO roles (id, name) VALUES ($1, $2) RETURNING *")
                .bind(id)
                .bind(name)
                .fetch_one(pool)
                .await
                .context("failed to create role")?;

        Ok(role)
    }

    /// Update a role's name.
    pub async fn update(pool: &PgPool, id: Uuid, name: &str) -> Result<Option<Self>> {
        let result =
            sqlx::query_as::<_, Role>("UPDATE roles SET name = $1 WHERE id = $2 RETURNING *")
                .bind(name)
                .bind(id)
                .fetch_optional(pool)
                .await
                .context("failed to update role")?;

        Ok(result)
    }

    /// Delete a role.
    pub async fn delete(pool: &PgPool, id: Uuid) -> Result<bool> {
        // Prevent deletion of well-known roles
        if id == well_known::ANONYMOUS_ROLE_ID || id == well_known::AUTHENTICATED_ROLE_ID {
            anyhow::bail!("cannot delete built-in role");
        }

        let result = sqlx::query("DELETE FROM roles WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .context("failed to delete role")?;

        Ok(result.rows_affected() > 0)
    }

    /// Get all permissions for this role.
    pub async fn get_permissions(pool: &PgPool, role_id: Uuid) -> Result<Vec<String>> {
        let permissions = sqlx::query_scalar::<_, String>(
            "SELECT permission FROM role_permissions WHERE role_id = $1",
        )
        .bind(role_id)
        .fetch_all(pool)
        .await
        .context("failed to get role permissions")?;

        Ok(permissions)
    }

    /// Add a permission to this role.
    pub async fn add_permission(pool: &PgPool, role_id: Uuid, permission: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO role_permissions (role_id, permission) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(role_id)
        .bind(permission)
        .execute(pool)
        .await
        .context("failed to add permission to role")?;

        Ok(())
    }

    /// Remove a permission from this role.
    pub async fn remove_permission(pool: &PgPool, role_id: Uuid, permission: &str) -> Result<()> {
        sqlx::query("DELETE FROM role_permissions WHERE role_id = $1 AND permission = $2")
            .bind(role_id)
            .bind(permission)
            .execute(pool)
            .await
            .context("failed to remove permission from role")?;

        Ok(())
    }

    /// Get all roles for a user.
    pub async fn get_user_roles(pool: &PgPool, user_id: Uuid) -> Result<Vec<Self>> {
        let roles = sqlx::query_as::<_, Role>(
            r#"
            SELECT r.* FROM roles r
            JOIN user_roles ur ON r.id = ur.role_id
            WHERE ur.user_id = $1
            ORDER BY r.name
            "#,
        )
        .bind(user_id)
        .fetch_all(pool)
        .await
        .context("failed to get user roles")?;

        Ok(roles)
    }

    /// Assign a role to a user.
    pub async fn assign_to_user(pool: &PgPool, user_id: Uuid, role_id: Uuid) -> Result<()> {
        sqlx::query(
            "INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(user_id)
        .bind(role_id)
        .execute(pool)
        .await
        .context("failed to assign role to user")?;

        Ok(())
    }

    /// Remove a role from a user.
    pub async fn remove_from_user(pool: &PgPool, user_id: Uuid, role_id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM user_roles WHERE user_id = $1 AND role_id = $2")
            .bind(user_id)
            .bind(role_id)
            .execute(pool)
            .await
            .context("failed to remove role from user")?;

        Ok(())
    }

    /// Get all permissions for a user (aggregated from all their roles).
    pub async fn get_user_permissions(pool: &PgPool, user_id: Uuid) -> Result<Vec<String>> {
        let permissions = sqlx::query_scalar::<_, String>(
            r#"
            SELECT DISTINCT rp.permission
            FROM role_permissions rp
            JOIN user_roles ur ON rp.role_id = ur.role_id
            WHERE ur.user_id = $1
            "#,
        )
        .bind(user_id)
        .fetch_all(pool)
        .await
        .context("failed to get user permissions")?;

        Ok(permissions)
    }
}
