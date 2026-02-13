//! Host functions for WASM plugins.
//!
//! These functions are imported by plugins and provide access to kernel services.
//! All string parameters use ptr+len pairs passed through WASM linear memory.

mod cache;
mod db;
mod item;
mod logging;
mod request_context;
mod user;
mod variables;

use anyhow::Result;
use wasmtime::Linker;

use crate::plugin::PluginState;

pub use cache::register_cache_functions;
pub use db::register_db_functions;
pub use item::register_item_functions;
pub use logging::register_logging_functions;
pub use request_context::register_request_context_functions;
pub use user::register_user_functions;
pub use variables::register_variables_functions;

/// Register all host functions with the linker.
pub fn register_all(linker: &mut Linker<PluginState>) -> Result<()> {
    register_logging_functions(linker)?;
    register_variables_functions(linker)?;
    register_request_context_functions(linker)?;
    register_user_functions(linker)?;
    register_cache_functions(linker)?;
    register_item_functions(linker)?;
    register_db_functions(linker)?;
    Ok(())
}

/// Helper to read a string from WASM memory.
///
/// # Safety
/// Caller must ensure ptr and len are valid within the memory bounds.
pub fn read_string_from_memory(
    memory: &wasmtime::Memory,
    store: &impl wasmtime::AsContext,
    ptr: i32,
    len: i32,
) -> Result<String> {
    let ptr = ptr as usize;
    let len = len as usize;

    let data = memory.data(store);
    if ptr + len > data.len() {
        anyhow::bail!("string read out of bounds: ptr={}, len={}, mem_size={}", ptr, len, data.len());
    }

    let bytes = &data[ptr..ptr + len];
    String::from_utf8(bytes.to_vec())
        .map_err(|e| anyhow::anyhow!("invalid UTF-8 in WASM string: {}", e))
}

/// Helper to write a string to WASM memory at the given location.
///
/// Returns the number of bytes written.
pub fn write_string_to_memory(
    memory: &wasmtime::Memory,
    store: &mut impl wasmtime::AsContextMut,
    ptr: i32,
    max_len: i32,
    value: &str,
) -> Result<i32> {
    let ptr = ptr as usize;
    let max_len = max_len as usize;
    let bytes = value.as_bytes();

    let write_len = bytes.len().min(max_len);

    let data = memory.data_mut(store);
    if ptr + write_len > data.len() {
        anyhow::bail!("string write out of bounds");
    }

    data[ptr..ptr + write_len].copy_from_slice(&bytes[..write_len]);
    Ok(write_len as i32)
}
