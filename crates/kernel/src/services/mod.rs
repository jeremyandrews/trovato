//! Kernel services for standard plugins.
//!
//! Services that provide runtime behavior for standard plugins.
//! Plugin WASM modules provide declarative configuration (permissions,
//! menus, migrations), while these services implement the logic.

pub mod audit;
pub mod content_lock;
pub mod email;
pub mod image_style;
pub mod locale;
pub mod oauth;
pub mod redirect;
pub mod tile;
