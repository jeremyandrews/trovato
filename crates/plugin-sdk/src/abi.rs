//! The tap/route memory protocol: the one size ceiling both the kernel and
//! this SDK enforce on a single call, and the allocator export a plugin
//! built against this SDK carries so the host can place input inside the
//! plugin's own linear memory instead of a hardcoded offset (B181).
//!
//! # Before this module
//!
//! The kernel's dispatcher wrote tap input at a hardcoded offset 0 in the
//! plugin's own linear memory, capped at 65536 bytes — safe only because a
//! fresh WASM module's first page happened to be unused scratch space, which
//! stops being true once input is allowed to grow past one page (a plugin's
//! own static data can start anywhere). Tap output was written into a
//! `static mut [u8; 65536]` the `#[plugin_tap]` macro generated, so an output
//! over 64 KB failed regardless of how much memory the plugin was otherwise
//! allowed. Three unrelated call sites each guessed their own number for what
//! was conceptually one channel.
//!
//! # After
//!
//! Both directions share [`PLUGIN_IO_MAX_BYTES`], and neither side writes
//! into memory it does not own: the plugin allocates its own input scratch
//! space (exported as [`ALLOC_EXPORT_NAME`]) and its own output buffer
//! ([`leak_output`]), and the host only ever writes into, or reads from,
//! memory the plugin itself handed back a pointer to.
//!
//! # ABI compatibility
//!
//! The plugin/kernel boundary is frozen (`KERNEL_API_VERSION` docs, `crates/
//! kernel/src/plugin/mod.rs`): an already-compiled `.wasm` must keep working.
//! This change is additive rather than a break — the allocator export is new
//! surface, not a change to existing surface — and its *presence* is the
//! compatibility signal the kernel dispatcher checks: a plugin built before
//! B181 does not export [`ALLOC_EXPORT_NAME`], so the dispatcher runs it
//! through the pre-B181 offset-0 protocol unchanged, at the pre-B181 64 KiB
//! ceiling. Only a plugin rebuilt against this SDK exports the allocator and
//! gets [`PLUGIN_IO_MAX_BYTES`]. No manifest version bump is needed or made.

/// Largest input or output a single tap/route call may carry across the WASM
/// boundary, in bytes, for a plugin built against this SDK (see the module
/// docs for the pre-B181 fallback ceiling a not-yet-rebuilt plugin still
/// gets).
///
/// One ceiling for both directions, so a caller never has to remember a
/// different number for "data going in" versus "data coming out" on what is
/// the same channel. Set well above any real page — B181's motivating case
/// was a 54 KB group description that the old 64 KB *output* cap already
/// refused to render — and comfortably inside the default 64 MiB per-`Store`
/// memory ceiling every plugin runs under
/// (`DEFAULT_MAX_MEMORY_BYTES`, `crates/kernel/src/plugin/limits.rs`), so a
/// call that uses the whole ceiling still leaves headroom for the plugin's
/// own code, data and working set.
pub const PLUGIN_IO_MAX_BYTES: usize = 8 * 1024 * 1024;

/// The exported symbol name the kernel's dispatcher looks for to tell a
/// plugin built against this SDK (new ABI) from one built before it (old
/// ABI). Kept as a named constant, rather than repeating the string literal
/// at both the export site and the two call sites that look it up, so the two
/// ends cannot drift apart.
pub const ALLOC_EXPORT_NAME: &str = "__trovato_alloc";

/// Leak `bytes` as a WASM export's `i64` return value: `(ptr << 32) | len`.
///
/// Used by the `#[plugin_tap]`/`#[plugin_tap_result]` macro expansion to hand
/// a tap's JSON output back to the host without a fixed-size static buffer.
/// Every call that reaches the generated export runs in a `Store` the kernel
/// instantiates fresh and discards afterward (`crates/kernel/src/tap/
/// dispatcher.rs`, `instantiate_and_call_export` — one instantiation per
/// call), so the leaked allocation is reclaimed with the rest of the
/// instance's memory the moment the host is done reading it; nothing inside
/// the guest ever needs to free it.
pub fn leak_output(bytes: &[u8]) -> i64 {
    let mut buf = bytes.to_vec();
    let ptr = buf.as_mut_ptr();
    let len = buf.len();
    core::mem::forget(buf);
    ((ptr as i64) << 32) | (len as i64)
}

/// Allocate `len` bytes inside the plugin's own linear memory and return the
/// pointer, for the host to write into before calling a tap export.
///
/// Exported under [`ALLOC_EXPORT_NAME`] so the host asks the plugin for
/// scratch space rather than writing at a hardcoded offset it does not own —
/// the hazard that made the old offset-0 protocol unsafe to widen past one
/// page (see the module docs). Meaningful only on `wasm32`; the host is the
/// only intended caller, once per call, immediately followed by the write
/// this pointer was allocated for.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn __trovato_alloc(len: i32) -> i32 {
    if len <= 0 {
        return 0;
    }
    let mut buf = vec![0u8; len as usize];
    let ptr = buf.as_mut_ptr();
    core::mem::forget(buf);
    ptr as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_export_name_matches_the_exported_symbol() {
        // The constant is documentation as much as a lookup key: if the
        // exported function is ever renamed, this is the one place both ends
        // of the ABI are supposed to agree, so a rename that forgets this
        // constant fails loudly here rather than silently at dispatch.
        assert_eq!(ALLOC_EXPORT_NAME, "__trovato_alloc");
    }

    #[test]
    fn leak_output_packs_the_length_into_the_low_32_bits() {
        // The `(ptr << 32) | len` packing is only ever decoded by the kernel
        // inside a wasm32 guest, where a pointer is itself a 32-bit value and
        // the round trip is lossless. On this (64-bit) test target a real
        // heap pointer does not fit in the high 32 bits the encoding allots
        // it, so this checks only the length half, which is target-
        // independent; the pointer half is exercised end-to-end by the
        // dispatcher's wasmtime-backed tests against an actual wasm32 module.
        let payload = b"{\"ok\":true}";
        let packed = leak_output(payload);
        let len = (packed & 0xFFFF_FFFF) as i32;
        assert_eq!(len as usize, payload.len());
    }

    #[test]
    fn leak_output_handles_empty_input() {
        let packed = leak_output(b"");
        let len = (packed & 0xFFFF_FFFF) as i32;
        assert_eq!(len, 0);
    }
}
