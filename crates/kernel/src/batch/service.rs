//! Batch operations service.

use std::sync::Arc;

use anyhow::{Context, Result};
use redis::{AsyncCommands, Client as RedisClient};
use tracing::{debug, error, info};
use uuid::Uuid;

use super::types::{BatchOperation, BatchProgress, BatchStatus, CreateBatch};

const BATCH_KEY_PREFIX: &str = "batch:";
const BATCH_TTL_SECS: i64 = 86400; // 24 hours

/// Service for managing batch operations.
#[derive(Clone)]
pub struct BatchService {
    redis: Arc<RedisClient>,
}

impl BatchService {
    /// Create a new batch service.
    pub fn new(redis: RedisClient) -> Self {
        Self {
            redis: Arc::new(redis),
        }
    }

    /// Create a new batch operation.
    pub async fn create(&self, input: CreateBatch) -> Result<BatchOperation> {
        let id = Uuid::now_v7();
        let now = chrono::Utc::now().timestamp();

        let operation = BatchOperation {
            id,
            operation_type: input.operation_type,
            status: BatchStatus::Pending,
            progress: BatchProgress::default(),
            params: input.params,
            result: None,
            error: None,
            created: now,
            updated: now,
        };

        self.save(&operation).await?;

        info!(
            batch_id = %id,
            operation_type = %operation.operation_type,
            "batch operation created"
        );

        Ok(operation)
    }

    /// Get a batch operation by ID.
    pub async fn get(&self, id: Uuid) -> Result<Option<BatchOperation>> {
        let key = self.operation_key(id);

        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to get Redis connection")?;

        let data: Option<String> = conn
            .get(&key)
            .await
            .context("failed to get batch operation")?;

        match data {
            Some(json) => {
                let operation: BatchOperation =
                    serde_json::from_str(&json).context("failed to parse batch operation")?;
                Ok(Some(operation))
            }
            None => Ok(None),
        }
    }

    /// Update a batch operation's status.
    pub async fn set_status(&self, id: Uuid, status: BatchStatus) -> Result<()> {
        let mut operation = self.get(id).await?.context("batch operation not found")?;

        operation.status = status;
        operation.updated = chrono::Utc::now().timestamp();

        self.save(&operation).await?;

        debug!(batch_id = %id, status = ?status, "batch status updated");
        Ok(())
    }

    /// Update a batch operation's progress.
    pub async fn update_progress(
        &self,
        id: Uuid,
        processed: u64,
        total: u64,
        current_operation: Option<String>,
    ) -> Result<()> {
        let mut operation = self.get(id).await?.context("batch operation not found")?;

        operation.progress.total = total;
        operation.progress.update(processed, current_operation);
        operation.updated = chrono::Utc::now().timestamp();

        // Only save status as running if it was pending
        if operation.status == BatchStatus::Pending {
            operation.status = BatchStatus::Running;
        }

        self.save(&operation).await?;
        Ok(())
    }

    /// Complete a batch operation with a result.
    pub async fn complete(&self, id: Uuid, result: Option<serde_json::Value>) -> Result<()> {
        let mut operation = self.get(id).await?.context("batch operation not found")?;

        operation.status = BatchStatus::Complete;
        operation.progress.complete();
        operation.result = result;
        operation.updated = chrono::Utc::now().timestamp();

        self.save(&operation).await?;

        info!(
            batch_id = %id,
            processed = operation.progress.processed,
            "batch operation completed"
        );

        Ok(())
    }

    /// Fail a batch operation with an error.
    pub async fn fail(&self, id: Uuid, error: &str) -> Result<()> {
        let mut operation = self.get(id).await?.context("batch operation not found")?;

        operation.status = BatchStatus::Failed;
        operation.error = Some(error.to_string());
        operation.updated = chrono::Utc::now().timestamp();

        self.save(&operation).await?;

        error!(
            batch_id = %id,
            error = %error,
            "batch operation failed"
        );

        Ok(())
    }

    /// Cancel a batch operation.
    pub async fn cancel(&self, id: Uuid) -> Result<()> {
        let mut operation = self.get(id).await?.context("batch operation not found")?;

        // Can only cancel pending or running operations
        if operation.status != BatchStatus::Pending && operation.status != BatchStatus::Running {
            anyhow::bail!(
                "cannot cancel operation in {} state",
                format!("{:?}", operation.status).to_lowercase()
            );
        }

        operation.status = BatchStatus::Cancelled;
        operation.updated = chrono::Utc::now().timestamp();

        self.save(&operation).await?;

        info!(batch_id = %id, "batch operation cancelled");
        Ok(())
    }

    /// Delete a batch operation.
    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        let key = self.operation_key(id);

        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to get Redis connection")?;

        let deleted: i64 = conn.del(&key).await.context("failed to delete batch")?;

        debug!(batch_id = %id, "batch operation deleted");
        Ok(deleted > 0)
    }

    /// Save a batch operation to Redis.
    async fn save(&self, operation: &BatchOperation) -> Result<()> {
        let key = self.operation_key(operation.id);
        let json = serde_json::to_string(operation).context("failed to serialize batch")?;

        let mut conn = self
            .redis
            .get_multiplexed_async_connection()
            .await
            .context("failed to get Redis connection")?;

        conn.set_ex::<_, _, ()>(&key, &json, BATCH_TTL_SECS as u64)
            .await
            .context("failed to save batch operation")?;

        Ok(())
    }

    /// Generate the Redis key for an operation.
    fn operation_key(&self, id: Uuid) -> String {
        format!("{}{}", BATCH_KEY_PREFIX, id)
    }
}

impl std::fmt::Debug for BatchService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatchService").finish()
    }
}
