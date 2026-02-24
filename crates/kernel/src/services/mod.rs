//! Kernel services for standard plugins.
//!
//! Services that provide runtime behavior for standard plugins.
//! Plugin WASM modules provide declarative configuration (permissions,
//! menus, migrations), while these services implement the logic.

pub mod ai_chat;
pub mod ai_provider;
pub mod ai_token_budget;
pub mod audit;
pub mod comment;
pub mod content_lock;
pub mod email;
pub mod image_style;
pub mod locale;
pub mod oauth;
pub mod pathauto;
pub mod redirect;
pub mod role;
pub mod tile;
pub mod user;
