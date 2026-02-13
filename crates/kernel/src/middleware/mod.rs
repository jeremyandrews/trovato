//! HTTP middleware components.
//!
//! Provides rate limiting, metrics collection, and other request processing layers.

pub mod rate_limit;

pub use rate_limit::{RateLimitConfig, RateLimiter, categorize_path, get_client_id, rate_limit_response};
