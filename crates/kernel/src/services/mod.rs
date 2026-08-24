//! Kernel services.
//!
//! Core services (`user`, `role`) are always present in `AppState`.
//! Plugin-optional services (`comment`, `content_lock`, etc.) are
//! wrapped in `Option<Arc<...>>` and initialized only when the
//! corresponding plugin is enabled.

pub mod account_access;
pub mod ai_assistant;
pub mod ai_chat;
pub mod ai_provider;
pub mod ai_token_budget;
pub mod ai_tools;
pub mod audit;
pub mod comment;
pub mod content_lock;
pub mod email;
pub mod email_templates;
pub mod embed_index;
pub mod image_style;
pub mod locale;
pub mod oauth;
pub mod pathauto;
pub mod recovery_builtins;
pub mod recovery_flow;
pub mod redirect;
pub mod role;
pub mod session_registry;
pub mod tile;
pub mod user;
pub mod vector_store;
pub mod webauthn;
