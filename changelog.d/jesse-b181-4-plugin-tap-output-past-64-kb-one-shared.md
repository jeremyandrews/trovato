A tap or a plugin-served route (`tap_api`) could carry at most 64 KB of input
or output. Output failed first in practice — the `#[plugin_tap]` macro wrote a
tap's JSON result into a `static mut [u8; 65536]` and refused anything larger —
but input was capped the same way: the kernel's dispatcher wrote it at a
hardcoded offset 0 inside the plugin's own linear memory, which was only safe
because a fresh module's first page happened to be unused scratch space, not
because anything made that true above 64 KB. A plugin route's request body had
its own independent 256 KB cap in `routes/plugin_api.rs`, and the SDK's own
host-function helpers used a third number, 256 KB, for an unrelated protocol —
three guesses at what was conceptually one channel. The motivating case: a
site plugin could not render a 54 KB field through a tap, because the output
side of that channel was the tightest of the three.

Both directions now share one ceiling, `trovato_sdk::abi::PLUGIN_IO_MAX_BYTES`
— 8 MiB, comfortably above any real page and well inside the 64 MiB a plugin
may use in total for one call. Neither side writes into memory it does not
own any more: a plugin built against the current SDK exports an allocator,
`__trovato_alloc`, and the kernel asks it for input scratch space instead of
guessing an offset; a tap's output is leaked from a `Vec` the plugin itself
allocated instead of a fixed-size static buffer. Sending or returning more than
the limit is refused with a clear, documented error — never silent
truncation — and a buggy or hostile guest that lies about an output's length
is caught by the same check rather than reading whatever the claimed bounds
happen to overlap in memory, which is what the pre-existing protocol did.

**Plugin ABI, additive.** The plugin/kernel boundary is frozen: an
already-compiled `.wasm` must keep working. This is new surface, not a change
to existing surface, and the new allocator export's *presence* is the
compatibility signal the dispatcher checks, so no manifest version bump was
needed or made. A plugin compiled before this change has no such export and
runs through the old offset-0 protocol unchanged, at the old 64 KiB ceiling. A
plugin rebuilt against the current SDK exports the allocator and gets the
larger ceiling automatically — every in-tree plugin was rebuilt as part of
this change. `routes/plugin_api.rs`'s request-body cap now equals the shared
constant instead of carrying its own number, and the plugin API router raises
axum's framework-default 2 MB body limit to match, since the shared ceiling
sits above it.

A small tap now costs one extra guest call: the host asks the plugin's new
allocator for input scratch space before writing into it, instead of writing
at a hardcoded offset with no call at all. Measured over 500 calls of a
32-byte echo tap against the same benign fixture both ways: roughly 504 μs/
call before (writing at offset 0, no allocator call) versus roughly 630 μs/
call after (one allocator call added) in an unoptimized debug build. The
difference is the one extra call, not the allocation itself, and it is
dwarfed by the per-call cost both numbers already include — instantiating a
fresh `Store` and module per call — which a real request path's millisecond-
scale DB and network I/O will not perceive.
