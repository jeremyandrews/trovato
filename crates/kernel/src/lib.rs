//! Trovato Kernel Library
//!
//! This library exposes kernel internals for integration testing.
//! The main entry point for running the server is the `trovato` binary.

// Items exposed for integration tests and plugin use; not all are consumed by the binary.
#![allow(dead_code)]

pub mod assistant;
pub mod audit;
pub mod batch;
pub mod cache;
pub mod circuit_breaker;
pub mod client_addr;
pub mod config;
pub mod config_storage;
pub mod content;
pub mod cron;
pub mod db;
pub mod error;
pub mod file;
pub mod form;
pub mod gather;
pub mod generate;
pub mod host;
pub mod lockout;
pub mod menu;
pub mod metrics;
pub mod middleware;
pub mod migrate;
pub mod migration_rule;
pub mod models;
pub(crate) mod net_policy;
pub mod pagination;
pub mod permissions;
pub mod plugin;
pub mod recovery;
pub mod routes;
pub mod search;
pub mod services;
pub mod session;
pub mod stage;
pub mod state;
pub mod system_log;
pub mod tap;
pub mod telemetry;
pub mod theme;
pub mod update_status;

// Re-export key types for testing
pub use config::Config;
pub use config_storage::{
    ConfigEntity, ConfigFilter, ConfigStorage, DirectConfigStorage, SearchFieldConfig, entity_types,
};
pub use models::stage::{LIVE_STAGE_ID, StageVisibility};
pub use stage::{
    ConflictInfo, ConflictResolution, ConflictType, PublishPhase, PublishResult, Resolution,
    StageService,
};
pub use state::AppState;
