The MCP server, `trovato-mcp`, is on rmcp 3.10, up from 0.16. Neither advisory
against 0.16 was reachable here: RUSTSEC-2026-0189 is in the Streamable HTTP
server transport and RUSTSEC-2026-0462 in the Streamable HTTP clients, and
`trovato-mcp` builds neither. It serves over STDIO with rmcp's default server
features plus `transport-io`, and it still does. The major version was overdue
anyway, and staying on 0.16 meant keeping a suppression for an advisory with a
fix available. The ten tools keep their names, input schemas and result shapes,
and the three resources and one resource template keep their URIs. rmcp 3
generates tool input schemas in a different form: an optional parameter
becomes a `["integer", "null"]` union instead of a nullable `integer`, and the
schema root loses its title and description. The server therefore generates
the schemas itself, exactly as 0.16 did, so a client sees the same schemas
byte for byte. A new test
serves the server over an in-process byte pipe the way `main.rs` serves it on
STDIO, sends `tools/list` as raw JSON-RPC, and checks the tool names, so a
future rmcp upgrade that drops, renames or adds a tool fails a test rather than
a client. The RUSTSEC-2026-0189 suppression is removed from
`.cargo/audit.toml`.

The advisory database that pull requests audit against moves from the
2026-10-06 commit to advisory-db commit 4505c66d of 2026-11-09. The rule that
picked it: the newest commit at which the tree audits with zero
vulnerabilities and no new suppression. The next commit in advisory-db
publishes RUSTSEC-2026-0459 against libsqlite3-sys 0.30.1, and no dependency
upgrade can fix that one. The crate is in `Cargo.lock` only because the lock
lists sqlx's SQLite driver; Trovato builds sqlx for PostgreSQL alone, so the
driver is never compiled. Every released sqlx, 0.9.0 included, caps
libsqlite3-sys below the fixed 0.39. The pin stops just short of that advisory
so it is not hidden from pull requests, and it stays open on the live audit
for a decision.
