The `cookie` crate is upgraded from 0.18.2 to 0.18.3, which fixes
RUSTSEC-2026-0594 (severity 6.5, medium): a process that forks after seeding
its random number generator could hand the same nonce to more than one
private (encrypted) cookie, letting two different cookies reuse the same
keystream. `cookie` arrives transitively through tower-sessions 0.14.0 and
tower-cookies 0.11.0, used by trovato-kernel and trovato-mcp for session and
private cookie handling, so every build of Trovato from `Cargo.lock` carried
the vulnerable version. The change is a lock file refresh only: the workspace
already asked for `cookie = "0.18"`, nothing else in `Cargo.lock` moved, and
no Trovato code or configuration changed. Operators building from source
should rebuild.
