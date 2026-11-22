//! Theme engine and template rendering.
//!
//! Provides Tera-based template rendering with template suggestion resolution
//! and RenderElement to HTML conversion.

mod engine;
mod render;
mod viewer;

pub use engine::{ThemeEngine, render_markdown};
pub use render::RenderTreeConsumer;
pub use viewer::Viewer;
