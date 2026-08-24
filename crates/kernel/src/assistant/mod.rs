//! The AI Assistant: configuring one thing by conversation.
//!
//! A plugin declares [`registry::RegisteredScope`]s from `tap_assistant_scopes`;
//! the kernel collects them once at boot into an [`registry::AssistantRegistry`],
//! the same way it collects menus. Everything else the feature needs lives
//! beside its own kind: the conversation and proposal rows in
//! [`crate::models::assistant`], the turn loop in `services::ai_assistant`, and
//! the routes in `routes::assistant`.

pub mod registry;

pub use registry::{AssistantRegistry, RegisteredScope, ScopeRejection};
