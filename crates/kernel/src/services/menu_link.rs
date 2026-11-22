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

/// Service for menu link writes.
#[derive(Clone)]
pub struct MenuLinkService {
    pool: PgPool,
}

impl MenuLinkService {
    /// Create the service.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create a link.
    pub async fn create(&self, input: CreateMenuLink) -> Result<MenuLink> {
        MenuLink::create(&self.pool, input).await
    }

    /// Update a link. `None` when there is no such link.
    pub async fn update(&self, id: Uuid, input: UpdateMenuLink) -> Result<Option<MenuLink>> {
        MenuLink::update(&self.pool, id, input).await
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
        }
        Ok(deleted)
    }
}
