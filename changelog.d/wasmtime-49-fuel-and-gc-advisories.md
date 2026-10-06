The plugin runtime runs on wasmtime 49.0.2. Five advisories stood against the
47.0.4 it was on, and no 47.x release fixes any of them, which is why the major
version moved rather than the patch: RUSTSEC-2026-0315, where `call_ref` and
exception `catch` could drop fuel accrued by a callee and so amplify a guest's
fuel budget exponentially; RUSTSEC-2026-0316, where dynamic record lifting could
allocate past the hostcall fuel limit; RUSTSEC-2026-0325, where mis-typed
WebAssembly tag imports could corrupt the GC heap; RUSTSEC-2026-0326, where
rooting for GC values live across `try_call` could be missing, corrupting the GC
heap by another route; and RUSTSEC-2026-0327, scored 9.3, a native stack buffer
overflow from an unvalidated result count on a component async-lifted callback.

Two of the five are fuel bypasses, and fuel is the sandbox's deterministic CPU
bound, so they are the ones that matter most here even though they score lowest.
Nothing in Trovato's own code had to change for the upgrade: `Config`,
`PoolingAllocationConfig`, `Store`, `Engine`, `Linker` and `ResourceLimiter` all
kept their surface across both major versions, and the one removal that the 48
and 49 release notes never mention, `Linker::define_name`, is a method the kernel
never called. `KERNEL_API_VERSION` is unchanged and plugins already built for
`wasm32-wasip1` load exactly as before.

Nothing turns a WebAssembly proposal on or off either, so the engine still
enables what it enabled on 47 plus wide arithmetic, which 49.0.0 made a default
for every embedder rather than something this configuration asks for. No shipped
plugin uses any of it.

The fuel regression test now asserts wasmtime's out-of-fuel trap by name instead
of accepting any failure. It accepted `Failed`, which is equally what a
memory-limit breach, a failed instantiation and every ordinary guest trap
produce, so it could not distinguish the fuel bound firing from the call breaking
for some other reason, which is the single thing it exists to establish. A spin
loop whose fuel stopped being charged would run on to the wall clock ceiling and
report a different error, and the test now notices.

The daily live audit had been failing since 2026-09-29 while reporting "no
vulnerability block in its output", with the advisories plainly present in the
log it attached to the same comment. `CARGO_TERM_COLOR: always` was set on a job
whose whole purpose is to machine-parse cargo's output, so every label reached
the parser wrapped in ANSI escapes and its `^Crate:` anchors never matched. The
variable is gone from that workflow, and the parse step strips escapes before
reading, so neither a future colour setting nor a cargo-audit default can break
it the same way again.

The pinned advisory database that pull requests audit against moved to the
2026-10-06 head of `rustsec/advisory-db`. It surfaces nothing beyond the five
advisories this change fixes, so the floor rises without anything being hidden
behind it.
