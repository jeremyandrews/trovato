//! Batch operation types.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A batch operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchOperation {
    /// Unique operation ID.
    pub id: Uuid,

    /// Operation type (e.g., "reindex", "bulk_update").
    pub operation_type: String,

    /// Current status.
    pub status: BatchStatus,

    /// Progress information.
    pub progress: BatchProgress,

    /// Operation parameters.
    #[serde(default)]
    pub params: serde_json::Value,

    /// Result data (when complete).
    #[serde(default)]
    pub result: Option<serde_json::Value>,

    /// Error message (if failed).
    #[serde(default)]
    pub error: Option<String>,

    /// Unix timestamp when operation was created.
    pub created: i64,

    /// Unix timestamp when operation was last updated.
    pub updated: i64,
}

/// Batch operation status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BatchStatus {
    /// Operation is queued and waiting to start.
    Pending,

    /// Operation is currently running.
    Running,

    /// Operation completed successfully.
    Complete,

    /// Operation failed with an error.
    Failed,

    /// Operation was cancelled by user.
    Cancelled,
}

/// Progress information for a batch operation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BatchProgress {
    /// Total number of items to process.
    pub total: u64,

    /// Number of items processed so far.
    pub processed: u64,

    /// Current operation being performed.
    #[serde(default)]
    pub current_operation: Option<String>,

    /// Percentage complete (0-100).
    pub percentage: u8,
}

impl BatchProgress {
    /// Create a new progress tracker.
    pub fn new(total: u64) -> Self {
        Self {
            total,
            processed: 0,
            current_operation: None,
            percentage: 0,
        }
    }

    /// Update progress.
    pub fn update(&mut self, processed: u64, current_operation: Option<String>) {
        self.processed = processed;
        self.current_operation = current_operation;
        self.percentage = if self.total > 0 {
            ((processed as f64 / self.total as f64) * 100.0).min(100.0) as u8
        } else {
            0
        };
    }

    /// Mark as complete.
    pub fn complete(&mut self) {
        self.processed = self.total;
        self.percentage = 100;
        self.current_operation = None;
    }
}

/// Request to create a new batch operation.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateBatch {
    /// Operation type (e.g., "reindex", "bulk_update").
    pub operation_type: String,

    /// Operation parameters (type-specific).
    #[serde(default)]
    pub params: serde_json::Value,
}
