//! Tap system for plugin extension points.
//!
//! Taps are named extension points that plugins can implement. When a tap is invoked,
//! all plugins that implement it are called in weight order (lower = higher priority).

mod dispatcher;
mod registry;
mod request_state;

pub(crate) use dispatcher::{ExportCallError, instantiate_and_call_export};
pub use dispatcher::{TapDispatcher, TapResult};
pub use registry::{TapHandler, TapRegistry};
pub use request_state::{RequestServices, RequestState, UserContext, new_field_access_cache};
