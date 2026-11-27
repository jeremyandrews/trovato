//! Form API with declarative forms, validation, and AJAX support.
//!
//! Forms are rendered through the same pipeline as content (via Tera templates).
//! The form system supports:
//! - Declarative form definition with typed elements
//! - CSRF token generation and verification
//! - Server-side validation with custom validators
//! - AJAX callbacks for multi-value fields
//! - Tap integration for form alteration, validation, and submission

pub mod ajax;
pub mod assets;
pub mod csrf;
mod service;
mod types;

pub use ajax::{AjaxCommand, AjaxRequest, AjaxResponse};
pub use csrf::{generate_csrf_token, verify_csrf_token};
pub(crate) use service::posted_values;
pub use service::{
    ADMIN_FORM, FORM_EXPIRED, FormResult, FormService, FormState, ProcessError, ValidationError,
};
pub use types::{ADD_ANOTHER, ElementType, Form, FormElement};
