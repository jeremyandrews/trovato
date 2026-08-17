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
pub mod session_tracking;
pub mod tenant;

pub use api_token::authenticate_api_token;
pub use bearer_auth::authenticate_bearer_token;
pub use install_check::check_installation;
pub use language::negotiate_language;
pub use path_alias::{path_alias_fallback, resolve_path_alias};
pub use query_profiler::track_request_timing;
pub use rate_limit::{
    ClientIp, RateLimitConfig, RateLimiter, categorize_path, check_authenticated_rate_limit,
    check_rate_limit, get_client_id, parse_trusted_proxies, rate_limit_response, resolve_client_ip,
};
pub use redirect::check_redirect;
pub use security_headers::{SecurityHeaders, inject_security_headers};
pub use session_tracking::track_session;
pub use tenant::{TenantResolution, resolve_tenant};
