//! Theme engine and template rendering.
//!
//! Provides Tera-based template rendering with template suggestion resolution
//! and RenderElement to HTML conversion.

mod engine;
mod render;

pub use engine::ThemeEngine;
pub use render::RenderTreeConsumer;
