//! Content management module.
//!
//! This module provides:
//! - ContentTypeRegistry: Manages content type definitions from plugins
//! - ItemService: CRUD operations with tap invocations
//! - FilterPipeline: Text format filtering for security
//! - FormBuilder: Auto-generated admin forms
//! - BlockTypeRegistry: Block type definitions and validation for block editor
//! - BlockRenderer: Server-side block rendering for Editor.js content

pub mod block_render;
pub mod block_types;
pub mod compound;
mod filter;
mod form;
mod item_service;
mod type_registry;

pub use block_render::render_blocks;
pub use block_types::{BlockTypeDefinition, BlockTypeRegistry};
pub use filter::{FilterPipeline, TextFilter};
pub use form::FormBuilder;
pub use item_service::ItemService;
pub use type_registry::ContentTypeRegistry;
