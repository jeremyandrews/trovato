//! Trovato Plugin SDK
//!
//! Types, traits, and host function bindings for Trovato WASM plugins.
//! Plugins depend on this crate and use its proc macros and builder APIs
//! to interact with the Kernel across the WASM boundary.

pub mod types;
pub mod render;

pub mod prelude {
    pub use crate::types::*;
    pub use crate::render;
}
