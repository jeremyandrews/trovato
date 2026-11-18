//! Bulk actions on items, and the executor that runs them as batches.
//!
//! One registry, [`BulkActionRegistry`], holds every action the content list
//! offers: the kernel's `publish`, `unpublish` and `delete`, and every action a
//! plugin declares in its manifest under `[[bulk_actions]]`, registered as
//! `"<plugin>.<name>"`. Submitting an action (the content list form or
//! `POST /api/batch`) goes through [`BulkExecutor::submit`], which checks the
//! action's permissions, records a [`BatchOperation`](super::BatchOperation)
//! and enqueues a kernel job under [`KERNEL_BATCH_PLUGIN`]. The cron drain,
//! the resident runner and a drain spawned at submit time all claim those jobs,
//! and each runs [`BulkExecutor::run`].
//!
//! The executor works in chunks of [`CHUNK_SIZE`]. Before each chunk it re-reads
//! the batch, so a cancelled batch stops, and re-resolves the submitting user's
//! permissions from the database, so a user who has lost the action's
//! permissions, been blocked or been deleted stops the run, which is marked
//! failed. It never acts with the background principal's authority: every write
//! is the submitting user's. Kernel actions make one [`ItemService`] call per
//! item; plugin actions load the chunk with one query, keep the items the user
//! may `edit`, and hand those to the declaring plugin's `tap_bulk_action` once.
//!
//! The design is section 4 of `docs/design/plugin-item-plumbing.md`.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

use super::{BatchOperation, BatchService, BatchStatus, CreateBatch};
use crate::content::{ItemService, PUBLISH_CONTENT};
use crate::models::{Item, UpdateItem, User};
use crate::permissions::PermissionService;
use crate::plugin::PluginRuntime;
use crate::tap::{RequestServices, RequestState, TapDispatcher, UserContext};

/// Reserved queue owner for batch jobs. A leading underscore can never be a
/// plugin machine name, so it cannot collide with a plugin's queue.
pub const KERNEL_BATCH_PLUGIN: &str = "__kernel_batch";

/// Queue name batch jobs are stored under.
pub const BATCH_QUEUE_NAME: &str = "bulk";

/// Items handled between two checks of the batch and the user.
pub const CHUNK_SIZE: usize = 100;

/// What an action does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkKind {
    /// Set `status` to 1 through [`ItemService::update`].
    Publish,
    /// Set `status` to 0 through [`ItemService::update`].
    Unpublish,
    /// Delete through [`ItemService::delete`].
    Delete,
    /// Hand the chunk to `plugin`'s `tap_bulk_action` as `name`.
    Plugin {
        /// The declaring plugin.
        plugin: String,
        /// The action's name inside the plugin.
        name: String,
    },
}

/// One registered bulk action.
#[derive(Debug, Clone, Serialize)]
pub struct BulkAction {
    /// The registered name: `publish`, `unpublish`, `delete`, or
    /// `"<plugin>.<name>"`.
    pub id: String,
    /// What the select shows.
    pub label: String,
    /// Every permission the user must hold to run it.
    pub permissions: Vec<String>,
    /// The past tense the finished message uses ("3 item(s) published.").
    #[serde(skip)]
    pub done: String,
    /// What it does.
    #[serde(skip)]
    pub kind: BulkKind,
}

impl BulkAction {
    /// Whether `user` holds every permission the action names.
    pub fn allowed(&self, user: &UserContext) -> bool {
        self.permissions.iter().all(|p| user.can(p))
    }

    /// The plugin that declared it, for a plugin action.
    pub fn plugin(&self) -> Option<&str> {
        match &self.kind {
            BulkKind::Plugin { plugin, .. } => Some(plugin),
            _ => None,
        }
    }

    /// The message a finished run shows, the text the form's flash always had.
    pub fn message(&self, succeeded: u64, failed: u64) -> String {
        if failed > 0 {
            format!(
                "{succeeded} item(s) {}. {failed} item(s) failed.",
                self.done
            )
        } else {
            format!("{succeeded} item(s) {}.", self.done)
        }
    }
}

/// Every bulk action the kernel knows, kernel actions first.
#[derive(Debug, Clone, Default)]
pub struct BulkActionRegistry {
    actions: Vec<BulkAction>,
}

impl BulkActionRegistry {
    /// The kernel's own actions, with the permissions the content list form
    /// has always required: a delete is a delete, and publishing or
    /// unpublishing changes an item's published state, which needs
    /// `publish content` on top of editing.
    pub fn kernel_actions() -> Vec<BulkAction> {
        let edit_and_publish = vec!["edit any content".to_string(), PUBLISH_CONTENT.to_string()];
        vec![
            BulkAction {
                id: "publish".into(),
                label: "Publish selected".into(),
                permissions: edit_and_publish.clone(),
                done: "published".into(),
                kind: BulkKind::Publish,
            },
            BulkAction {
                id: "unpublish".into(),
                label: "Unpublish selected".into(),
                permissions: edit_and_publish,
                done: "unpublished".into(),
                kind: BulkKind::Unpublish,
            },
            BulkAction {
                id: "delete".into(),
                label: "Delete selected".into(),
                permissions: vec!["delete any content".into()],
                done: "deleted".into(),
                kind: BulkKind::Delete,
            },
        ]
    }

    /// The kernel actions plus every action a loaded plugin declares. Whether
    /// the plugin is enabled is asked at use time, not here, so enabling or
    /// disabling a plugin needs no rebuild.
    pub fn from_plugins(runtime: &PluginRuntime) -> Self {
        let mut actions = Self::kernel_actions();
        let mut plugins: Vec<_> = runtime.plugins().values().collect();
        plugins.sort_by(|a, b| a.info.name.cmp(&b.info.name));
        for plugin in plugins {
            for decl in &plugin.info.bulk_actions {
                actions.push(BulkAction {
                    id: format!("{}.{}", plugin.info.name, decl.name),
                    label: decl.label.clone(),
                    permissions: decl.permissions.clone(),
                    done: "processed".into(),
                    kind: BulkKind::Plugin {
                        plugin: plugin.info.name.clone(),
                        name: decl.name.clone(),
                    },
                });
            }
        }
        Self { actions }
    }

    /// A registry holding exactly `actions`.
    pub fn from_actions(actions: Vec<BulkAction>) -> Self {
        Self { actions }
    }

    /// The action registered as `id`.
    pub fn get(&self, id: &str) -> Option<&BulkAction> {
        self.actions.iter().find(|a| a.id == id)
    }

    /// The actions `user` may run, in registry order, leaving out those of a
    /// plugin `enabled` says is off.
    pub fn available_to(
        &self,
        user: &UserContext,
        enabled: impl Fn(&str) -> bool,
    ) -> Vec<&BulkAction> {
        self.actions
            .iter()
            .filter(|a| a.plugin().is_none_or(&enabled) && a.allowed(user))
            .collect()
    }
}

/// Why a submission was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum SubmitError {
    /// No action is registered under that name (or its plugin is disabled).
    Unknown,
    /// The user does not hold the action's permissions.
    Denied,
    /// No items were selected.
    Empty,
}

/// The kernel services a [`BulkExecutor`] runs with.
pub struct BulkServices {
    /// The database.
    pub pool: PgPool,
    /// Every kernel action's write, and every plugin action's access decision.
    pub items: Arc<ItemService>,
    /// Where the batch's progress and result are recorded.
    pub batch: Arc<BatchService>,
    /// Dispatches `tap_bulk_action`.
    pub dispatcher: Arc<TapDispatcher>,
    /// The registered actions.
    pub registry: Arc<BulkActionRegistry>,
    /// Re-resolves the submitting user's permissions before each chunk.
    pub permissions: PermissionService,
    /// The services a plugin's `tap_bulk_action` runs with.
    pub services: RequestServices,
    /// The live set of enabled plugins, shared with the application state.
    pub enabled: Arc<parking_lot::RwLock<HashSet<String>>>,
}

/// Runs bulk actions as batches.
pub struct BulkExecutor {
    pool: PgPool,
    items: Arc<ItemService>,
    batch: Arc<BatchService>,
    dispatcher: Arc<TapDispatcher>,
    registry: Arc<BulkActionRegistry>,
    permissions: PermissionService,
    services: RequestServices,
    enabled: Arc<parking_lot::RwLock<HashSet<String>>>,
}

impl BulkExecutor {
    /// Build an executor over the kernel's services.
    pub fn new(services: BulkServices) -> Self {
        let BulkServices {
            pool,
            items,
            batch,
            dispatcher,
            registry,
            permissions,
            services,
            enabled,
        } = services;
        Self {
            pool,
            items,
            batch,
            dispatcher,
            registry,
            permissions,
            services,
            enabled,
        }
    }

    /// The registry this executor runs actions from.
    pub fn registry(&self) -> &BulkActionRegistry {
        &self.registry
    }

    /// Whether the plugin behind an action is enabled; kernel actions always are.
    fn is_live(&self, action: &BulkAction) -> bool {
        action
            .plugin()
            .is_none_or(|p| self.enabled.read().contains(p))
    }

    /// The actions `user` may run now.
    pub fn available_to(&self, user: &UserContext) -> Vec<&BulkAction> {
        let enabled = self.enabled.read();
        self.registry
            .available_to(user, |plugin| enabled.contains(plugin))
    }

    /// Record a batch running `action_id` over `ids` for `user`, and enqueue the
    /// job that runs it. The caller nudges a drain with
    /// [`crate::cron::CronService::drain_bulk_jobs`].
    pub async fn submit(
        &self,
        action_id: &str,
        ids: Vec<Uuid>,
        user: &UserContext,
    ) -> Result<std::result::Result<BatchOperation, SubmitError>> {
        let operation = match self.create(action_id, ids, user).await? {
            Ok(operation) => operation,
            Err(refused) => return Ok(Err(refused)),
        };
        crate::host::enqueue_kernel_job(
            &self.pool,
            KERNEL_BATCH_PLUGIN,
            BATCH_QUEUE_NAME,
            &json!({ "batch_id": operation.id }),
            0,
            0,
        )
        .await
        .context("enqueue batch job")?;
        Ok(Ok(operation))
    }

    /// Record a batch running `action_id` over `ids` for `user`, checked as
    /// [`Self::submit`] checks it, without enqueueing it: the caller runs it
    /// with [`Self::run`]. The content list does this for a selection of one
    /// chunk or less, which finishes within the request as it always has.
    pub async fn create(
        &self,
        action_id: &str,
        ids: Vec<Uuid>,
        user: &UserContext,
    ) -> Result<std::result::Result<BatchOperation, SubmitError>> {
        let Some(action) = self.registry.get(action_id).filter(|a| self.is_live(a)) else {
            return Ok(Err(SubmitError::Unknown));
        };
        if !action.allowed(user) {
            return Ok(Err(SubmitError::Denied));
        }
        if ids.is_empty() {
            return Ok(Err(SubmitError::Empty));
        }
        let operation = self
            .batch
            .create(
                CreateBatch {
                    operation_type: action.id.clone(),
                    params: json!({ "item_ids": ids }),
                },
                user.id,
            )
            .await?;
        Ok(Ok(operation))
    }

    /// The submitting user as they are now, from the database, bypassing the
    /// permission cache. `None` for a user who was deleted or blocked.
    async fn submitter(&self, id: Uuid) -> Result<Option<UserContext>> {
        let Some(user) = User::find_by_id(&self.pool, id).await? else {
            return Ok(None);
        };
        if !user.is_active() {
            return Ok(None);
        }
        let permissions = self.permissions.load_user_permissions(&user).await?;
        Ok(Some(crate::permissions::context_from_permissions(
            &user,
            permissions,
        )))
    }

    /// Run the batch `id` to the end, or until it is cancelled or its user may
    /// no longer run it. A batch that is not pending is left alone: another
    /// drainer has it, or it already ran.
    ///
    /// Errors are only those before the run starts; once it has started, a
    /// failure is recorded on the batch, so a retry of the job finds it no
    /// longer pending and does nothing.
    pub async fn run(&self, id: Uuid) -> Result<()> {
        let Some(operation) = self.batch.get(id).await? else {
            return Ok(());
        };
        if operation.status != BatchStatus::Pending {
            return Ok(());
        }
        let ids: Vec<Uuid> = operation
            .params
            .get("item_ids")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .unwrap_or_default()
            .unwrap_or_default();
        let total = ids.len() as u64;
        self.batch.update_progress(id, 0, total, None).await?;

        let outcome = self.run_chunks(&operation, &ids).await;
        let recorded = match outcome {
            Ok(Finished::Done { succeeded, failed }) => {
                let message = self
                    .registry
                    .get(&operation.operation_type)
                    .map(|a| a.message(succeeded, failed))
                    .unwrap_or_default();
                self.batch
                    .complete(
                        id,
                        Some(json!({
                            "succeeded": succeeded,
                            "failed": failed,
                            "message": message,
                        })),
                    )
                    .await
            }
            Ok(Finished::Cancelled) => Ok(()),
            Ok(Finished::Stopped(reason)) => self.batch.fail(id, &reason).await,
            Err(e) => {
                warn!(batch_id = %id, error = %e, "bulk action failed");
                self.batch
                    .fail(id, "The action stopped because of an internal error.")
                    .await
            }
        };
        if let Err(e) = recorded {
            warn!(batch_id = %id, error = %e, "could not record the end of a bulk action");
        }
        Ok(())
    }

    async fn run_chunks(&self, operation: &BatchOperation, ids: &[Uuid]) -> Result<Finished> {
        let total = ids.len() as u64;
        let (mut succeeded, mut failed, mut processed) = (0u64, 0u64, 0u64);
        for chunk in ids.chunks(CHUNK_SIZE) {
            match self.batch.get(operation.id).await? {
                Some(current) if current.status == BatchStatus::Cancelled => {
                    return Ok(Finished::Cancelled);
                }
                None => return Ok(Finished::Cancelled),
                Some(_) => {}
            }
            let action = self
                .registry
                .get(&operation.operation_type)
                .filter(|a| self.is_live(a));
            let Some(action) = action else {
                return Ok(Finished::Stopped(format!(
                    "Stopped after {processed} of {total} items: the action is no longer available."
                )));
            };
            let user = self.submitter(operation.owner_id).await?;
            let Some(user) = user.filter(|u| action.allowed(u)) else {
                return Ok(Finished::Stopped(format!(
                    "Stopped after {processed} of {total} items: you no longer have permission \
                     to run this action."
                )));
            };

            let ok = match &action.kind {
                BulkKind::Plugin { plugin, name } => {
                    self.plugin_chunk(plugin, name, chunk, &user).await?
                }
                kind => self.kernel_chunk(kind, &action.id, chunk, &user).await,
            };
            succeeded += ok;
            failed += chunk.len() as u64 - ok;
            processed += chunk.len() as u64;
            self.batch
                .update_progress(
                    operation.id,
                    processed,
                    total,
                    Some(format!("{processed} of {total}")),
                )
                .await?;
        }
        Ok(Finished::Done { succeeded, failed })
    }

    /// One [`ItemService`] call per item, as the user; returns how many
    /// succeeded. An id that names no item is a failure.
    async fn kernel_chunk(
        &self,
        kind: &BulkKind,
        action: &str,
        chunk: &[Uuid],
        user: &UserContext,
    ) -> u64 {
        let mut ok = 0;
        for id in chunk {
            let done = match kind {
                BulkKind::Delete => self.items.delete(*id, user).await,
                _ => {
                    let update = UpdateItem {
                        title: None,
                        status: Some(i16::from(*kind == BulkKind::Publish)),
                        fields: None,
                        promote: None,
                        sticky: None,
                        log: Some(format!("Bulk {action}")),
                    };
                    self.items
                        .update(*id, update, user)
                        .await
                        .map(|saved| saved.is_some())
                }
            };
            match done {
                Ok(true) => ok += 1,
                Ok(false) => {}
                Err(e) => warn!(item_id = %id, error = %e, action, "bulk action item failed"),
            }
        }
        ok
    }

    /// Load the chunk with one query, keep what the user may edit, and hand
    /// that to the plugin once; returns how many the plugin says succeeded,
    /// never more than it was given.
    async fn plugin_chunk(
        &self,
        plugin: &str,
        name: &str,
        chunk: &[Uuid],
        user: &UserContext,
    ) -> Result<u64> {
        let items = Item::find_by_ids(&self.pool, chunk).await?;
        let mut editable = Vec::with_capacity(items.len());
        for item in &items {
            match self.items.check_access(item, "edit", user).await {
                Ok(true) => editable.push(item.id),
                Ok(false) => {}
                Err(e) => warn!(item_id = %item.id, error = %e, "bulk action access check failed"),
            }
        }
        if editable.is_empty() {
            return Ok(0);
        }
        let given = editable.len() as u64;
        let input = json!({ "action": name, "item_ids": editable }).to_string();
        let state = RequestState::new(user.clone(), self.services.clone());
        let result = self
            .dispatcher
            .dispatch_to_plugin("tap_bulk_action", &input, plugin, state)
            .await;
        let succeeded = result
            .and_then(|r| serde_json::from_str::<serde_json::Value>(&r.output).ok())
            .and_then(|v| v.get("succeeded").and_then(serde_json::Value::as_u64))
            .unwrap_or(0);
        Ok(succeeded.min(given))
    }
}

/// How a run ended.
enum Finished {
    Done { succeeded: u64, failed: u64 },
    Cancelled,
    Stopped(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_with(perms: &[&str]) -> UserContext {
        UserContext::authenticated(
            Uuid::now_v7(),
            perms.iter().map(|p| p.to_string()).collect(),
        )
    }

    fn plugin_action() -> BulkAction {
        BulkAction {
            id: "p.archive".into(),
            label: "Archive".into(),
            permissions: vec!["archive content".into()],
            done: "processed".into(),
            kind: BulkKind::Plugin {
                plugin: "p".into(),
                name: "archive".into(),
            },
        }
    }

    #[test]
    fn the_kernel_actions_keep_the_forms_permissions() {
        let r = BulkActionRegistry::from_actions(BulkActionRegistry::kernel_actions());
        let editor = user_with(&["edit any content"]);
        let publisher = user_with(&["edit any content", PUBLISH_CONTENT]);
        let deleter = user_with(&["delete any content"]);
        let ids = |u: &UserContext| -> Vec<String> {
            r.available_to(u, |_| true)
                .iter()
                .map(|a| a.id.clone())
                .collect()
        };
        assert!(ids(&editor).is_empty());
        assert_eq!(ids(&publisher), vec!["publish", "unpublish"]);
        assert_eq!(ids(&deleter), vec!["delete"]);
    }

    #[test]
    fn a_plugin_action_shows_only_when_enabled_and_permitted() {
        let mut actions = BulkActionRegistry::kernel_actions();
        actions.push(plugin_action());
        let r = BulkActionRegistry::from_actions(actions);
        let archivist = user_with(&["archive content"]);
        assert_eq!(r.available_to(&archivist, |_| true).len(), 1);
        assert!(r.available_to(&archivist, |_| false).is_empty());
        assert!(r.available_to(&user_with(&[]), |_| true).is_empty());
    }

    #[test]
    fn the_finished_message_is_the_forms_flash() {
        let publish = &BulkActionRegistry::kernel_actions()[0];
        assert_eq!(publish.message(3, 0), "3 item(s) published.");
        assert_eq!(
            publish.message(3, 2),
            "3 item(s) published. 2 item(s) failed."
        );
    }
}
