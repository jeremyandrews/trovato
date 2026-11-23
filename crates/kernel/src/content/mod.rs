//! Content management module.
//!
//! This module provides:
//! - ContentTypeRegistry: Manages content type definitions from plugins
//! - ItemService: CRUD operations with tap invocations
//! - FilterPipeline: Text format filtering for security
//! - FormBuilder: Auto-generated admin forms
//! - BlockTypeRegistry: Block type definitions and validation for block editor
//! - BlockRenderer: Server-side block rendering for Editor.js content

/// The longest item title any write path accepts, in bytes: the item form,
/// the JSON API and the MCP tools refuse a longer one alike.
pub const MAX_TITLE_LENGTH: usize = 1000;

pub mod block_render;
pub mod block_types;
pub mod body_field;
pub mod compound;
pub mod field_value;
pub(crate) mod file_refs;
mod filter;
mod form;
pub(crate) mod item_service;
pub mod moderation;
pub mod owned_vocabulary;
pub mod page_builder;
pub mod page_builder_components;
pub mod page_meta;
pub mod preview;
mod record_type;
pub mod reentry;
pub mod revision_compare;
pub mod submission;
pub mod text_format;
mod type_registry;

pub use block_render::render_blocks;
pub use block_types::{BlockTypeDefinition, BlockTypeRegistry};
pub use filter::{FilterPipeline, TextFilter};
pub use form::{
    FieldMode, FormBuilder, LOG_WEIGHT, STATUS_WEIGHT, SUBMIT_WEIGHT, extract_reference_id,
};
pub use item_service::{
    ADMINISTER_CONTENT, DELETE_REVISIONS, FileAccess, ItemRevisions, ItemService, ItemTranslation,
    PUBLISH_CONTENT, PageBuilderInvalid, RevisionAccess, RevisionDeleted, RevisionSelection,
    TRANSLATE_CONTENT, TranslationRefused, USE_STAGES, VIEW_REVISIONS, WriteDenied,
    decode_view_output,
};
pub use page_meta::PageMeta;
pub use record_type::{RecordTypeDef, RecordTypeLoadError, RecordTypeRegistry};
pub use type_registry::{AddFieldError, ContentTypeRegistry, CreateContentTypeError};
