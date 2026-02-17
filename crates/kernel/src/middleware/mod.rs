//! HTTP middleware components.
//!
//! Provides rate limiting, metrics collection, path alias resolution,
//! and other request processing layers.

pub mod api_token;
pub mod install_check;
pub mod language;
pub mod path_alias;
pub mod rate_limit;

pub use api_token::authenticate_api_token;
pub use install_check::check_installation;
pub use language::negotiate_language;
pub use path_alias::resolve_path_alias;
pub use rate_limit::{
    RateLimitConfig, RateLimiter, categorize_path, get_client_id, rate_limit_response,
};
