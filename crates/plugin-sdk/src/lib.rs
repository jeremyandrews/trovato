//! Trovato Plugin SDK
//!
//! Types, traits, and host function bindings for Trovato WASM plugins.
//! Plugins depend on this crate and use its proc macros and builder APIs
//! to interact with the Kernel across the WASM boundary.

pub mod types;
pub mod render;

// Re-export proc macros
pub use trovato_sdk_macros::{plugin_tap, plugin_tap_result};

// Re-export serde_json for use in macro-generated code
#[doc(hidden)]
pub use serde_json;

pub mod prelude {
    pub use crate::types::*;
    pub use crate::render;
    pub use crate::{plugin_tap, plugin_tap_result};
}
