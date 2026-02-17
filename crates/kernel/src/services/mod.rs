//! Kernel services for standard plugins.
//!
//! Services that provide runtime behavior for standard plugins.
//! Plugin WASM modules provide declarative configuration (permissions,
//! menus, migrations), while these services implement the logic.

pub mod audit;
pub mod content_lock;
pub mod image_style;
pub mod locale;
pub mod oauth;
pub mod po_parser;
pub mod redirect;
pub mod scheduled_publishing;
pub mod translated_config;
pub mod translation;
pub mod webhook;
