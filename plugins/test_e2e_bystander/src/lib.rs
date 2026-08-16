//! Epic 2 end-to-end test fixture: the **bystander** for the FR-4a caller gate.
//!
//! Its compiled module imports `trovato:kernel/plugin-api` (via the SDK `invoke`
//! binding) and *nothing else* under `trovato:kernel/*`. The Story 2.4 suite loads
//! it under a manifest that does **not** declare `host_interfaces = ["plugin-api"]`
//! and asserts the WASM-1 load-time import-vs-declaration pre-check rejects it with
//! the exact declarative error — proving a plugin cannot reach `invoke` unless it
//! declares the capability, with a real SDK-compiled binary.
//!
//! It ships **no `.info.toml`** in-tree (the compiled `.wasm` is the only artifact,
//! matching the reference-app's `.wasm`-only plugin dirs), so `load_all`/the
//! in-tree load-smoke test skip it; the test supplies the (deliberately
//! insufficient) manifest itself.

/// The only export. References the `plugin-api` `invoke` import so the compiled
/// module's import section names `trovato:kernel/plugin-api` — the whole reason
/// this fixture exists. Never actually called by the test (the load fails at the
/// pre-check, before any dispatch).
///
/// # Safety
///
/// Raw `(ptr, len) -> i64` export ABI; the body only forces the `plugin-api`
/// import to be retained.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
pub extern "C" fn run(_ptr: i32, _len: i32) -> i64 {
    // The result is ignored; this exists solely to pull `plugin-api` into the
    // module's import section.
    let _ = trovato_sdk::host::invoke("nobody", "noop", "{}");
    0
}
