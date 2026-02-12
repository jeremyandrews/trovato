//! Content management module.
//!
//! This module provides:
//! - ContentTypeRegistry: Manages content type definitions from plugins
//! - ItemService: CRUD operations with tap invocations
//! - FilterPipeline: Text format filtering for security
//! - FormBuilder: Auto-generated admin forms

mod filter;
mod form;
mod item_service;
mod type_registry;

pub use filter::{FilterPipeline, TextFilter};
pub use form::FormBuilder;
pub use item_service::ItemService;
pub use type_registry::ContentTypeRegistry;
