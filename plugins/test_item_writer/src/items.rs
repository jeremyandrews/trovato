//! The four `item-api` calls, through the SDK's bindings.
//!
//! Kept as a module of its own so the fixture names them in one place; the
//! round trip in `tap_queue_worker` is what checks the bindings end to end.

pub use trovato_sdk::host::{delete_item, get_item, query_items, save_item};
