# phase0-guest — WASM benchmark fixture

Minimal `#![no_std]` plugin (wee_alloc, no libc) compiled to `wasm32-wasip1`.
It backs the `trovato-phase0` WASM micro-benchmark harness: it exports the tap
functions (`tap_item_view`, `tap_item_view_full`) and the OQ-11 loop primitives
(`bench_noop_loop`, `bench_empty_loop`, `bench_field_loop`, `bench_alloc_loop`).

This package is **standalone** (its own `[workspace]`) so it can set
`panic=abort` + LTO and build for wasm independently of the root workspace, which
excludes it. The compiled `.wasm` is gitignored; you must build it before running
the harness.

## Build (regeneration)

Run from the **repo root**:

```bash
cargo build --release --target wasm32-wasip1 \
  --manifest-path benchmarks/phase0/guest/Cargo.toml
```

Output lands at
`benchmarks/phase0/guest/target/wasm32-wasip1/release/phase0_guest.wasm`, which is
one of the paths the harness searches (`src/main.rs::guest_wasm_path`).

Then run the harness (also from repo root):

```bash
cargo run --release -p trovato-phase0 -- --benchmark all      # gates 1-3
cargo run --release -p trovato-phase0 -- --benchmark oq11      # boundary primitives
cargo run --release -p trovato-phase0 -- --benchmark invoke    # FR-4a invoke path
```

## Why `build.rs` passes `--import-undefined` (toolchain-drift note)

Because the guest is `no_std` and links no libc, byte-slice / `str` comparison in
the guest (e.g. `str::find` in the JSON helpers) lowers to a `memcmp` call that
has no definition at link time — the `wasm32-wasip1` sysroot `compiler_builtins`
does not export `memcmp` for a libc-less link. **By design the host supplies
`memcmp` as a runtime import from the `env` module** (see `env.memcmp` in
`../src/host.rs`), so the guest is expected to emit `(import "env" "memcmp")`.

Older Rust toolchains defaulted rust-lld to turning undefined wasm symbols into
`env` imports, so this happened automatically. **Rust 1.96.0's rust-lld no longer
does** — undefined symbols are a hard link error:

```
rust-lld: error: ...: undefined symbol: memcmp
```

`build.rs` restores the original contract by emitting
`cargo::rustc-link-arg=--import-undefined`, so `memcmp` again becomes
`(import "env" "memcmp")`, satisfied by the host at runtime. This keeps the
benchmark measuring the same shape it always did (the recorded baselines were
taken with the host-supplied `env.memcmp`).

It lives in `build.rs`, not `.cargo/config.toml`, because cargo discovers
`.cargo/config.toml` relative to the **current working directory**, and the
documented build command runs from the repo root via `--manifest-path` — a
`guest/.cargo/config.toml` would be silently ignored.

### If a future toolchain bump strands the harness again

1. Reproduce: `cargo build --release --target wasm32-wasip1 --manifest-path benchmarks/phase0/guest/Cargo.toml`.
2. Inspect what the guest imports/needs: `wasm-tools print <the.wasm> | grep import`.
   The intended imports are the four `trovato:kernel/*` host functions plus
   `env.memcmp`. Anything else undefined is new drift.
3. If a *new* undefined symbol appears, decide whether the host should supply it
   as an `env` import (add a `linker.func_wrap("env", "<sym>", …)` stub in
   `../src/host.rs`, mirroring `memcmp`) — `--import-undefined` already turns it
   into an import; you only need the host stub.
4. Keep the fix **benchmark-only**. Do not touch kernel / plugin-sdk / WIT.
