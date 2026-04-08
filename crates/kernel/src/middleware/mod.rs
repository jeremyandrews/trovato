//! HTTP middleware components.
//!
//! Provides rate limiting, metrics collection, path alias resolution,
//! and other request processing layers.

pub mod api_token;
pub mod bearer_auth;
pub mod install_check;
pub mod language;
pub mod path_alias;
pub mod query_profiler;
pub mod rate_limit;
pub mod redirect;
pub mod security_headers;
pub mod tenant;

pub use api_token::authenticate_api_token;
pub use bearer_auth::authenticate_bearer_token;
pub use install_check::check_installation;
pub use language::negotiate_language;
pub use path_alias::{path_alias_fallback, resolve_path_alias};
pub use query_profiler::track_request_timing;
pub use rate_limit::{
    RateLimitConfig, RateLimiter, categorize_path, check_authenticated_rate_limit,
    check_rate_limit, get_client_id, rate_limit_response,
};
pub use redirect::check_redirect;
pub use security_headers::inject_security_headers;
pub use tenant::resolve_tenant;
