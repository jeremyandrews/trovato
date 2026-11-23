//! Menu link writes.
//!
//! The menu admin screens write links through here. Creating and updating are
//! the model's, which invalidates the navigation cache itself; deleting is a
//! transaction, so a link's children are moved up to its parent in the same
//! commit that removes it rather than in a statement before it that could
//! succeed alone. The kernel defines no menu link taps (`tap_menu` declares a
//! plugin's routes, it is not a write notification).

use anyhow::{Context, Result};
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{CreateMenuLink, MenuLink, UpdateMenuLink};
use crate::services::audit::AuditTrail;

/// Service for menu link writes.
#[derive(Clone)]
pub struct MenuLinkService {
    pool: PgPool,
    audit: AuditTrail,
}

impl MenuLinkService {
    /// Create the service.
    pub fn new(pool: PgPool, audit: AuditTrail) -> Self {
        Self { pool, audit }
    }

    /// Record one link write in the audit log.
    async fn audit(&self, action: &str, link: &MenuLink) {
        self.audit
            .record(
                action,
                "menu_link",
                link.id,
                None,
                serde_json::json!({ "menu": link.menu_name, "title": link.title, "path": link.path }),
            )
            .await;
    }

    /// Create a link.
    pub async fn create(&self, input: CreateMenuLink) -> Result<MenuLink> {
        let link = MenuLink::create(&self.pool, input).await?;
        self.audit("create", &link).await;
        Ok(link)
    }

    /// Update a link. `None` when there is no such link.
    pub async fn update(&self, id: Uuid, input: UpdateMenuLink) -> Result<Option<MenuLink>> {
        let link = MenuLink::update(&self.pool, id, input).await?;
        if let Some(link) = &link {
            self.audit("update", link).await;
        }
        Ok(link)
    }

    /// Delete a link, its children moving up to its own parent rather than
    /// to the top of the menu, all in one transaction. `false` when there is
    /// no such link.
    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        let mut tx = self
            .pool
            .begin()
            .await
            .context("failed to start menu link delete")?;
        let deleted = MenuLink::delete_in(&mut tx, id).await?;
        tx.commit()
            .await
            .context("failed to commit menu link delete")?;
        if deleted {
            crate::menu::invalidate_config_menus();
            self.audit
                .record("delete", "menu_link", id, None, serde_json::json!({}))
                .await;
        }
        Ok(deleted)
    }
}
