//! Epic 2 end-to-end test fixture: a plugin with **no `[capabilities]` table**
//! (`capabilities: None`).
//!
//! It imports no `trovato:kernel/*` interface, so it loads cleanly under deny-all.
//! Because `capabilities: None` means no `public_functions`, invoking its exported
//! `ping` must be rejected by the callee-consent gate with `permission-denied`,
//! and `plugin-exists` must report it as not invocable — even though it is
//! installed, enabled, and exports a real function. This proves the FR-4a
//! deny-by-default (Model C) end to end.

/// The only export: returns a small constant JSON via the `(ptr, len) -> i64`
/// memory protocol. Exported so the "capabilities None denies an *exported*
/// function" case is genuine, but never reached — the callee gate rejects the
/// invoke first.
///
/// # Safety
///
/// Raw export ABI; the returned buffer is leaked so it stays valid until the host
/// reads it synchronously after the call returns.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn ping(_ptr: i32, _len: i32) -> i64 {
    let buf: &'static mut [u8] = br#"{"pong":true}"#.to_vec().leak();
    let ptr = buf.as_mut_ptr() as i64;
    let len = buf.len() as i64;
    (ptr << 32) | len
}
