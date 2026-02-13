//! HTTP middleware components.
//!
//! Provides rate limiting, metrics collection, and other request processing layers.

pub mod install_check;
pub mod rate_limit;

pub use install_check::check_installation;
pub use rate_limit::{
    RateLimitConfig, RateLimiter, categorize_path, get_client_id, rate_limit_response,
};
