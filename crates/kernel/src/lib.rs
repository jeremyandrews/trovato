//! Trovato CMS Kernel Library
//!
//! This library exposes kernel internals for integration testing.
//! The main entry point for running the server is the `trovato` binary.

pub mod batch;
pub mod cache;
pub mod config;
pub mod config_storage;
pub mod content;
pub mod cron;
pub mod db;
pub mod file;
pub mod form;
pub mod gather;
pub mod host;
pub mod lockout;
pub mod menu;
pub mod metrics;
pub mod middleware;
pub mod models;
pub mod permissions;
pub mod plugin;
pub mod routes;
pub mod search;
pub mod services;
pub mod session;
pub mod stage;
pub mod state;
pub mod tap;
pub mod theme;

// Re-export key types for testing
pub use config::Config;
pub use config_storage::{
    ConfigEntity, ConfigFilter, ConfigStorage, DirectConfigStorage, SearchFieldConfig,
    StageAwareConfigStorage, entity_types,
};
pub use stage::{
    ConflictInfo, ConflictResolution, ConflictType, PublishPhase, PublishResult, Resolution,
    StageService,
};
pub use state::AppState;
