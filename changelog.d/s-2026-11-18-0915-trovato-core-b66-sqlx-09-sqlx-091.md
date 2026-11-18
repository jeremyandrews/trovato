Trovato is on sqlx 0.9.1, up from 0.8.6, and the live advisory audit is clean.
Every sqlx 0.8 release capped libsqlite3-sys below 0.39, the release that fixes
RUSTSEC-2026-0459, so the lock could not take the fix even though Trovato never
compiles sqlx's SQLite driver; sqlx 0.9.1 lifted the cap, and the lock now
carries libsqlite3-sys 0.40. The same upgrade moved rsa behind an opt-in MySQL
feature, so RUSTSEC-2023-0071 left the lock too. Neither advisory was ever
reachable, and neither was ever suppressed.

`.cargo/audit.toml` has an empty ignore list. Its four remaining entries
(RUSTSEC-2023-0071, -2026-0141, -2026-0194 and -2026-0195) matched no crate in
`Cargo.lock` and are removed.

Queries behave as before. sqlx 0.9 refuses a query string that is not a
`&'static str` unless the caller marks it with `AssertSqlSafe`, so the places
that build SQL at run time (gather queries, the plugin database host functions,
plugin migrations, and a handful of model and test queries) now say so
explicitly; the SQL they send is unchanged. Trovato uses no compile-time
checked `query!` macros, so there is no offline query data to regenerate.

Three sqlx changes reach an operator's configuration rather than Trovato's
code. `PGHOSTADDR` (or `hostaddr` in `DATABASE_URL`) now follows libpq: it
skips DNS but `host` is still the name used for TLS. A password read from
`.pgpass` has its backslash escapes processed, as libpq does. And sqlx's
slow-acquire log line spells its field `acquired_after_secs`, not
`aquired_after_secs`, which matters only to anyone parsing those logs. A
deployment that sets its database through `DATABASE_URL` alone sees none of
these.
