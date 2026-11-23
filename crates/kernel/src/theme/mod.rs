//! Theme engine and template rendering.
//!
//! Provides Tera-based template rendering with template suggestion resolution
//! and RenderElement to HTML conversion.

mod engine;
pub mod registry;
mod render;
mod viewer;

pub use engine::{
    ActiveThemes, BrokenTemplate, TemplateParseError, ThemeEngine, render_markdown, scope_request,
};
pub use render::RenderTreeConsumer;
pub use viewer::Viewer;
