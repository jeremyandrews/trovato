//! Batch operations API for long-running tasks.
//!
//! This module provides an API for starting, monitoring, and retrieving
//! results of long-running background operations like bulk reindexing,
//! content migrations, and file processing.

mod service;
mod types;

pub use service::BatchService;
pub use types::{BatchOperation, BatchProgress, BatchStatus, CreateBatch};
