//! Host functions for WASM plugins.
//!
//! These functions are imported by plugins and provide access to kernel services.
//! All string parameters use ptr+len pairs passed through WASM linear memory.

mod ai;
mod cache;
mod crypto;
mod db;
pub(crate) mod http;
mod item;
mod logging;
mod plugin_api;
mod queue;
mod request_context;
mod user;
mod variables;

use anyhow::{Result, anyhow};
use wasmtime::Linker;

use crate::plugin::PluginState;

pub use ai::register_ai_functions;
pub use cache::register_cache_functions;
pub use crypto::register_crypto_functions;
pub use db::register_db_functions;
pub use http::register_http_functions;
pub use item::register_item_functions;
pub use logging::register_logging_functions;
pub use plugin_api::register_plugin_api_functions;
/// Kernel-internal queue producer (P11f / D-52): enqueue a job the kernel owns
/// under a reserved `plugin_name`, reusing the P11d insert path. Not a
/// plugin-facing host function.
pub(crate) use queue::enqueue_kernel_job;
pub use queue::register_queue_functions;
pub use request_context::register_request_context_functions;
pub use user::register_user_functions;
pub use variables::register_variables_functions;

/// Registers one host interface's functions on a plugin `Linker`.
///
/// The value half of [`HOST_INTERFACE_REGISTRARS`] — each interface's
/// `register_*_functions` entry point.
pub type HostInterfaceRegistrar = fn(&mut Linker<PluginState>) -> Result<()>;

/// Every known host interface and the function that registers it on a `Linker`.
///
/// This is the single source of truth that lets the per-plugin linker (WASM-1)
/// register a *subset* of host interfaces from a plugin's declared
/// `host_interfaces`. The keys MUST equal
/// [`crate::plugin::KNOWN_HOST_INTERFACES`] — asserted by the D-20 map-completeness
/// guard in `tests/plugin_test.rs`. Each value is the interface's
/// `register_*_functions` entry point; the host module string each one registers
/// under is `trovato:kernel/<key>` (e.g. `trovato:kernel/logging`).
pub const HOST_INTERFACE_REGISTRARS: &[(&str, HostInterfaceRegistrar)] = &[
    ("logging", register_logging_functions),
    ("variables", register_variables_functions),
    ("request-context", register_request_context_functions),
    ("user-api", register_user_functions),
    ("cache-api", register_cache_functions),
    ("item-api", register_item_functions),
    ("db", register_db_functions),
    ("ai-api", register_ai_functions),
    ("queue", register_queue_functions),
    ("http", register_http_functions),
    ("crypto-api", register_crypto_functions),
    ("plugin-api", register_plugin_api_functions),
];

/// Register every known host interface with the linker.
///
/// Iterates [`HOST_INTERFACE_REGISTRARS`] so the map remains the single source
/// of truth. Behaviour is unchanged from the previous straight-line call list:
/// all 12 interfaces are registered. The per-plugin linker uses
/// [`register_declared`] instead to expose only a declared subset (WASM-1).
pub fn register_all(linker: &mut Linker<PluginState>) -> Result<()> {
    for (_iface, register) in HOST_INTERFACE_REGISTRARS {
        register(linker)?;
    }
    Ok(())
}

/// Register exactly the named host interfaces on the linker (deny-unless-declared).
///
/// Used by the per-plugin linker to expose only the host interfaces a plugin
/// declares under `[capabilities] host_interfaces` (WASM-1). WASI stubs are
/// registered separately as a baseline and are not part of this subset.
///
/// Unknown names are normally impossible here — manifest validation
/// ([`crate::plugin::PluginInfo`] parsing) already rejects interface names not in
/// [`crate::plugin::KNOWN_HOST_INTERFACES`] — but a defensive lookup-miss against
/// [`HOST_INTERFACE_REGISTRARS`] is surfaced as an error rather than silently
/// skipped.
///
/// # Errors
///
/// Returns an error if a named interface is not present in
/// [`HOST_INTERFACE_REGISTRARS`], or if any underlying `register_*` call fails.
pub fn register_declared(linker: &mut Linker<PluginState>, ifaces: &[String]) -> Result<()> {
    for iface in ifaces {
        let (_, register) = HOST_INTERFACE_REGISTRARS
            .iter()
            .find(|(name, _)| *name == iface.as_str())
            .ok_or_else(|| anyhow!("unknown host interface '{iface}'"))?;
        register(linker)?;
    }
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
        anyhow::bail!(
            "string read out of bounds: ptr={}, len={}, mem_size={}",
            ptr,
            len,
            data.len()
        );
    }

    let bytes = &data[ptr..ptr + len];
    String::from_utf8(bytes.to_vec())
        .map_err(|e| anyhow::anyhow!("invalid UTF-8 in WASM string: {e}"))
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
    write_bytes_to_memory(memory, store, ptr, max_len, value.as_bytes())
}

/// Helper to write raw bytes to WASM memory at the given location, truncating to
/// `max_len`. Returns the number of bytes written.
///
/// The string variant [`write_string_to_memory`] delegates here; the streaming
/// HTTP `http-read` host function (P11e / D-49) writes non-UTF-8-safe body chunks
/// directly.
pub fn write_bytes_to_memory(
    memory: &wasmtime::Memory,
    store: &mut impl wasmtime::AsContextMut,
    ptr: i32,
    max_len: i32,
    bytes: &[u8],
) -> Result<i32> {
    let ptr = ptr as usize;
    let max_len = max_len as usize;

    let write_len = bytes.len().min(max_len);

    let data = memory.data_mut(store);
    if ptr + write_len > data.len() {
        anyhow::bail!("bytes write out of bounds");
    }

    data[ptr..ptr + write_len].copy_from_slice(&bytes[..write_len]);
    Ok(write_len as i32)
}
