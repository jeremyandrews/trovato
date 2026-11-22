The plugin runtime runs on wasmtime 49.0.6, up from 49.0.5, which fixes eight
advisories published on 2026-11-16 against every earlier 49.x release.
In RUSTSEC-2026-0568, a stale GC heap base pointer in compiled code let a guest
abort the host process, and RUSTSEC-2026-0573 aborted the process when a GC
heap's reservation was not a multiple of the page size. RUSTSEC-2026-0567 and
RUSTSEC-2026-0570 aborted builds without the `threads` or the `gc` cargo
feature. RUSTSEC-2026-0569 panicked in `Caller::get_module_export` when a host
function was not called from WebAssembly, and RUSTSEC-2026-0572 panicked when a
component cancelled a stream or future copy on the wrong end. The last two,
RUSTSEC-2026-0566 and RUSTSEC-2026-0571, are unbounded host memory growth from
empty writes in `wasmtime-wasi` and `wasmtime-wasi-http`, neither of which the
kernel links.

The ones that matter here are those a plugin can reach: a guest able to abort or
panic the host takes down every site the process serves, not only the plugin.
The upgrade is a lock file change only. The workspace already asked for
wasmtime `49`, the whole release family (`wasmtime-*`, `cranelift-*` and
`pulley-*`) moved together to the 49.0.6 release and its cranelift 0.136.6,
nothing else in `Cargo.lock` moved, and no Trovato code changes. Plugins already
built for `wasm32-wasip1` load exactly as before.
