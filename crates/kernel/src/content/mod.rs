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
mod file_refs;
mod filter;
mod form;
pub(crate) mod item_service;
pub mod page_builder;
pub mod page_builder_components;
pub mod page_meta;
mod record_type;
mod type_registry;

pub use block_render::render_blocks;
pub use block_types::{BlockTypeDefinition, BlockTypeRegistry};
pub use filter::{FilterPipeline, TextFilter};
pub use form::{FormBuilder, extract_reference_id};
pub use item_service::{ItemService, decode_view_output};
pub use page_meta::PageMeta;
pub use record_type::{RecordTypeDef, RecordTypeLoadError, RecordTypeRegistry};
pub use type_registry::ContentTypeRegistry;
