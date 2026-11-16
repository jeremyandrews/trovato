The lock file is refreshed to the newest release every declared dependency
requirement allows, which clears 41 of the 43 vulnerabilities the daily live
security audit had been reporting against `main`. The root cause was that the
lock was never refreshed after the 2026-10-06 wasmtime upgrade: fixed patch
releases were published and sat unused, while the pull request audit, which
runs against a pinned copy of the advisory database, stayed green because none
of the advisories existed at its pin.

The crates the advisories stood against, old to new: wasmtime 49.0.2 to 49.0.5
(seventeen advisories against the plugin runtime; still the 49 line, with
`KERNEL_API_VERSION` unchanged and plugins already built for `wasm32-wasip1`
loading as before), openssl 0.10.76 to 0.10.82, ammonia 4.1.4 to 4.2.5 (four
advisories, two of them sanitizer bypasses), rustls 0.23.45 to 0.23.46,
rustls-webpki 0.103.15 to 0.103.17, webpki-roots 1.0.6 to 1.0.12,
pulldown-cmark 0.13.3 to 0.13.6, and zstd 0.13.3 to 0.13.5 with zstd-safe 7.2.4
to 7.4.1 and zstd-sys 2.0.16 to 2.1.3, which wasmtime's module cache pulls in.
The same refresh clears the unsound-code warnings against tokio 1.50.0 (now
1.54.1), rustix 1.1.4 (1.1.6), event-listener 5.4.1 (5.4.2) and rand 0.8.5 and
0.9.2 (0.8.8 and 0.9.5). No `Cargo.toml` and no source file changed.

Two advisories remain, and neither has a fix a lock refresh can reach: rmcp
0.16.0 (RUSTSEC-2026-0462, in Streamable HTTP client transports the STDIO-only
MCP server does not compile), whose fix is the rmcp 3.x major version, and
libsqlite3-sys 0.30.1 (RUSTSEC-2026-0459), which sqlx's lock entry for
`sqlx-sqlite` carries and every released sqlx caps below the fixed 0.39.
