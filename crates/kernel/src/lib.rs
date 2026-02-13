//! Trovato CMS Kernel Library
//!
//! This library exposes kernel internals for integration testing.
//! The main entry point for running the server is the `trovato` binary.

pub mod config;
pub mod content;
pub mod db;
pub mod form;
pub mod gather;
pub mod host;
pub mod lockout;
pub mod menu;
pub mod models;
pub mod permissions;
pub mod plugin;
pub mod routes;
pub mod session;
pub mod state;
pub mod tap;
pub mod theme;

// Re-export key types for testing
pub use config::Config;
pub use state::AppState;
