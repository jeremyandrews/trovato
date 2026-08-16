//! Build script for the Phase 0 benchmark guest plugin.
//!
//! # Why this exists
//!
//! The guest is `#![no_std]` (wee_alloc, no libc) and is built for
//! `wasm32-wasip1` as a `cdylib` that never links wasi-libc. Byte-slice / `str`
//! comparison in the guest (e.g. `str::find` / `starts_with` in the JSON helpers)
//! lowers to a `memcmp` call, and with no libc and a wasip1 sysroot
//! `compiler_builtins` that does not export `memcmp` for a libc-less link, that
//! symbol is undefined at link time.
//!
//! By design the host supplies `memcmp` as a runtime import from the `env`
//! module (see `env.memcmp` in `src/host.rs`). Older Rust toolchains defaulted
//! rust-lld to emitting undefined wasm symbols as `env` imports, so `memcmp`
//! silently became `(import "env" "memcmp")` and the host stub satisfied it.
//!
//! Rust 1.96.0's rust-lld no longer does this: undefined symbols are a hard link
//! error (`undefined symbol: memcmp`). Passing `--import-undefined` restores the
//! original contract — the guest re-emits `(import "env" "memcmp")`, exactly what
//! the host already provides — so the benchmark measures the same shape it always
//! did. This lives in a build script (not `.cargo/config.toml`) because the
//! documented build command runs from the repo root via `--manifest-path`, and
//! cargo discovers `.cargo/config.toml` from the cwd, not the manifest directory.
//!
//! See `README.md` in this directory for the full regeneration procedure.

fn main() {
    // Restore the pre-1.96 behavior: undefined wasm symbols (here, `memcmp`)
    // become imports from the `env` module, satisfied by the host at runtime.
    println!("cargo::rustc-link-arg=--import-undefined");
}
