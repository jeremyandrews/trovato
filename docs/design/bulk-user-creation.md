# Bulk account creation

Status: design only (B225). No kernel or CLI behaviour changes in this commit.

## The problem

`trovato user create` builds the full server state — database pool, Redis
connection, every enabled plugin compiled and instantiated, every kernel
service (items, roles, menus, themes, comments, locale, the vector store
check, and so on) — so that `tap_user_register` fires and the account is
audited exactly as the admin "Add user" form would audit it. That makes one
call cost about what starting the server costs. There is no path that pays
this cost once and creates many accounts; an operator who wants hundreds or
thousands of accounts today can only call `trovato user create` in a loop, or
write directly to the `users` table and lose every validation rule, tap and
audit row the single-account path gives for free.

## Measurement setup

All numbers below are from this run, on an 8 CPU Linux pod (dev job B225-2),
not the Mac Studio these commands are normally run on. PostgreSQL 16 and Redis
7 were already reachable at `127.0.0.1:5432` / `127.0.0.1:6379`. The
measurement database (`trovato_b225`) was migrated once with `trovato migrate`
and is otherwise empty.

The kernel was built with:

```
cargo build -p trovato-kernel --release
```

(29m22s cold, 23m17s on an incremental rebuild that touched only
`crates/kernel` — both on this 8 CPU worker, not CI's shards.)

"The standard plugins" are the fifteen that ship with `default_enabled` unset
(true by default) in their `.info.toml`, i.e. what a fresh install auto-enables
with no site-specific configuration:

```
trovato_audit_log, trovato_blog, trovato_categories, trovato_comments,
trovato_config_translation, trovato_content_locking, trovato_content_translation,
trovato_image_styles, trovato_locale, trovato_media, trovato_oauth2,
trovato_redirects, trovato_scheduled_publishing, trovato_seo, trovato_webhooks
```

found with:

```
find plugins -maxdepth 1 -mindepth 1 -type d | while read d; do
  n=$(basename "$d"); i="$d/$n.info.toml"
  [ -f "$i" ] && { grep -q default_enabled "$i" || echo "$n: implicit true"; }
done
```

All fifteen were built and copied into their plugin directories so the server
actually compiles and loads them, matching CI's own pattern:

```
cargo build --target wasm32-wasip1 --release \
  -p trovato_audit_log -p trovato_blog -p trovato_categories -p trovato_comments \
  -p trovato_config_translation -p trovato_content_locking -p trovato_content_translation \
  -p trovato_image_styles -p trovato_locale -p trovato_media -p trovato_oauth2 \
  -p trovato_redirects -p trovato_scheduled_publishing -p trovato_seo -p trovato_webhooks
cp target/wasm32-wasip1/release/<name>.wasm plugins/<name>/
```

`trovato user create` was then run once (throwaway) so the one-time
`plugin_status` auto-install row insert didn't skew the timed runs, then five
times, timed with the shell's `time` builtin, against the still-fresh
database (a new `measure_user_N` account each run, so none hit the
username-taken path):

```
for i in 1 2 3 4 5; do
  time ( printf '<password>\n' | ./target/release/trovato user create measure_user_$i \
           --mail measure_user_$i@example.com --role 'authenticated user' )
done
```

To break the single call into phases without changing committed behaviour,
`std::time::Instant` checkpoints and `eprintln!("B225_PHASE <name> <micros>")`
lines were added temporarily around each phase boundary in
`AppState::new` (`crates/kernel/src/state.rs`) and `create_user_from_cli`
(`crates/kernel/src/main.rs`), rebuilt, run, and then reverted — they are not
part of this commit's diff. Grepping a run's stderr for `B225_PHASE` gives the
phase breakdown below.

No plugin implements `tap_user_register` or any other tap the create path
fires (`grep -rn tap_user_register --include=*.rs .`, excluding
`/target/`, finds only the kernel's own declaration, dispatch site and
comments — zero plugin hits). The create path dispatches exactly one tap,
`tap_user_register`, from `UserService::create` /
`UserService::create_with_status` (`crates/kernel/src/services/user.rs:264-325`).

## Measured phase breakdown

Wall times for the five runs (with a password and one role,
`--role 'authenticated user'`): **3.260s, 3.390s, 3.456s, 3.469s, 3.572s** —
median **3.456s** (run 2). Its phase breakdown:

| Phase | Time | What it covers |
|---|---:|---|
| `config_load` | 0.1 ms | `Config::from_env()` |
| `precheck_pool_and_migration_check` | 7.2 ms | `run_user_command`'s own pool + `prepare_schema_for_cli`'s migration check — a **second**, separate connect happens inside `AppState::new` right after this; today's single `user create` connects to Postgres twice (`crates/kernel/src/main.rs:1264-1271` then `state.rs:339-344`) |
| `AppState::new` total | 2000.5 ms | everything below, summed |
| &nbsp;&nbsp;`db_connect` | 6.2 ms | pool creation + the (no-op, already-migrated) migration check inside `AppState::new` |
| &nbsp;&nbsp;`redis_connect` | 6.3 ms | Redis client + rate limiter + `PING` |
| &nbsp;&nbsp;`plugin_discover_install` | 26.4 ms | parse 49 plugin directories' `.info.toml`, auto-install lookup, enabled-set query |
| &nbsp;&nbsp;`plugin_load_compile` | **971.1 ms** | compile + instantiate the 15 standard plugins' WASM modules (`PluginRuntime::load_enabled`) |
| &nbsp;&nbsp;`plugin_migrations_and_other_services` | 990.4 ms | plugin migration check + constructing every other kernel service (items, roles, menus, themes, comments, locale, the vector-store check, tap registries, `tap_menu`/`tap_perm`/`tap_admin_nav` dispatch, …) — none of it needed by `user create` |
| `validation` | 3.8 ms | `new_account_problems` (uniqueness, format) + role name lookup |
| `create_insert_tap_audit` | 385.7 ms | Argon2id password hash + `INSERT INTO users` + `tap_user_register` dispatch (to 15 loaded, non-implementing plugins) + the `create`/`user` audit row |
| `role_grants` | 7.2 ms | one `SELECT` + one upsert + permission-cache invalidation + an audit row + a `UserRolesChanged` security event, for the one granted role |
| `cache_clear` | **1003.5 ms** | `bump_from_cli` → `cache::clear_everywhere`: bump the Redis generation counters, then an unconditional `tokio::time::sleep(generation::SETTLE)` |

Two isolating measurements explain the two largest phases:

- **Password cost.** Three more runs with `--no-password` (same 15 plugins
  loaded) put `create_insert_tap_audit` at 2.8–3.9 ms instead of ~386 ms —
  confirming essentially all of that phase is one Argon2id hash at this
  kernel's RFC 9106 parameters (64 MiB memory, 3 iterations, 4 lanes;
  `crates/kernel/src/models/user.rs:608-625`), not the insert or the tap
  dispatch.
- **Plugin compile cost.** One run with `PLUGINS_DIR` pointed at an empty
  directory (no plugins discovered, so no `JWT_SECRET`/OAuth2 warning either)
  dropped `plugin_load_compile` to 0.2 ms, `AppState::new` to 926.6 ms, and
  the whole call to 1.978s. So of the 3.456s median, compiling the 15 standard
  plugins costs about 970 ms, and everything else `AppState::new` builds
  regardless of plugins (items, roles, menus, themes, …) costs about another
  960–990 ms.
- **`cache_clear`'s 1003.5 ms is a fixed sleep, not Redis latency.**
  `generation::SETTLE` (`crates/kernel/src/cache/generation.rs:26,32`) is
  `POLL_INTERVAL.saturating_mul(4)` = 4 × 250 ms = exactly 1000 ms, run
  unconditionally at the end of every CLI command that writes behind a
  running server's back. It is the single largest fixed cost in the call,
  larger than compiling all 15 plugins.
- **No wasmtime compilation cache.** `create_engine`
  (`crates/kernel/src/plugin/runtime.rs:864-915`) builds a `wasmtime::Config`
  with the pooling allocator, epoch interruption and optional fuel, but never
  calls `cache_config_load` / `cache_config_load_default`. Every process
  invocation recompiles each plugin's WASM from the same bytes; nothing is
  reused between CLI runs.

## Options, with projected time for 1,000 accounts

The baseline — looping today's `trovato user create` 1,000 times, one process
per account, each supplying a password and one role — projects to
**1,000 × 3.456s ≈ 3,456s (57.6 minutes)** on this hardware.

### 1. A batch command (`trovato user import <file>`)

Builds `AppState` once, then creates every row through the same
`UserService::create` / `create_with_status` the single-account path already
uses, and clears caches once at the end.

**Projection.** Setup pays the measured `AppState::new` median once (2.0005s,
plugins included — the import tool still needs `trovato_audit_log` enabled so
`audit_created` writes a content row, so skipping plugin load is not free here
the way it is for option 2's isolated measurement). Per row, reusing the
measured components with no per-row password (see "Passwords" below):
`validation` (3.8 ms) + `create_insert_tap_audit` without a password (≈3 ms,
per the isolating run) + `role_grants` for one role (7.2 ms) ≈ **14 ms/row**.
For 1,000 rows: 2.0s + 1,000 × 0.014s = 2.0 + 14 = **16s**, plus one final
`cache_clear` (1.0s) ≈ **17s total** — about 200× faster than the loop.
If every row hashed its own password instead (see below), add
1,000 × 0.386s ≈ 386s, for **≈403s (6.7 min)** — which is why this design
does not recommend per-row hashing as the default.

**What it changes.** A new CLI subcommand and an input parser; no change to
`UserService` or the tap/audit path, so every account still gets exactly what
`user create` gives one.

**Risks.** A long-running CLI process holding one DB pool connection open for
the whole import; a bad row stopping (or not stopping) the rest; the file
containing plaintext passwords (addressed below); 1,000 audit + security-event
rows written in one run, which is expected (same rows `user create` would
write 1,000 times) but worth confirming the audit log plugin's indexes handle
without a dedicated load test.

### 2. Load only the plugins that implement the taps the create path fires

Since **no plugin implements `tap_user_register` today**, this option would,
right now, load zero plugins for a dedicated import tool — eliminating the
entire measured 971 ms `plugin_load_compile` phase, matching the isolated
"empty `PLUGINS_DIR`" measurement above (`AppState::new` at 926.6 ms,
whole call at 1.978s).

**Projection, still one process per account (no batching):** per account ≈
0.927s (`AppState::new`, 0 plugins) + 0.007s (pool/precheck) + 0.004s
(validation) + 0.386s (create, with password) + 0.006s (role grant) + 1.0s
(cache clear) ≈ 2.33s → **1,000 × 2.33s ≈ 2,330s (38.8 min)**. Without a
password, ≈1.95s/account → **≈1,950s (32.5 min)**. A real improvement over the
57.6-minute baseline, but nowhere near option 1, because it still pays
`AppState::new`'s non-plugin cost (≈0.93s) and the 1-second cache-clear sleep
**once per account**, not once per batch.

**What it changes.** `AppState::new` (or a narrower constructor path) would
need to know, before loading any plugin, which plugins declare which taps —
today that is only knowable by loading and instantiating each one
(`PluginInfo` from `.info.toml` carries no tap list). This needs either a
declared-taps field in `.info.toml` (checked against what the compiled module
actually exports, so a stale declaration cannot silently drop a hook) or a
cheap "does this module export this function" check that stops short of a
full instantiation.

**Risks.** Loading fewer plugins for a *server* is dangerous (other taps —
`tap_menu`, `tap_perm`, request-path taps — need every enabled plugin); this
option is safe only for a process that is known to do one narrow thing, such
as a dedicated import binary or an import subcommand that explicitly only
needs `tap_user_register`. It would need to keep its own exemption path
separate from `serve`'s.

### 3. Caching compiled plugin modules between CLI runs

Wasmtime has a built-in on-disk module cache (`Config::cache_config_load` /
`cache_config_load_default`), keyed on the module bytes and the compiler
settings, not enabled anywhere in `create_engine`
(`crates/kernel/src/plugin/runtime.rs:864-915`). Turning it on is a few lines.

**Projection.** The *first* invocation after enabling the cache still pays
today's measured 971 ms (nothing to reuse yet); every invocation after that,
for an unchanged set of 15 plugin `.wasm` files, would skip recompilation.
Projected per-account loop cost after the cache is warm: the measured 3.456s
minus the 971 ms `plugin_load_compile` phase ≈ **2.49s/account** →
**1,000 × 2.49s ≈ 2,490s (41.5 min)**. Combined with option 2 it is largely
redundant for the create path specifically (both remove the same ~971 ms),
but option 3 also speeds up every other plugin-loading CLI command
(`plugin`, `config import`, `cache clear`, …), where option 2's
tap-based narrowing does not apply.

**What it changes.** One `wasmtime::Config` call, plus a writable cache
directory and a decision about where it lives (per-host, shared across
servers, invalidated on a plugin rebuild).

**Risks.** Disk space and staleness (a cache keyed on content hash self-
invalidates when a `.wasm` file changes, so this is low-risk); needs a sanity
check that two kernel versions with different Cranelift codegen settings
don't collide on cache keys (wasmtime's key already includes compiler
version/flags).

### 4. Creating accounts through the running server (an authenticated admin endpoint)

No new `AppState` per account — the server is already warm with every plugin
loaded, and does not need the CLI's `cache_clear` sleep for its own
request path (a request reads the database directly; the fixed sleep in
`clear_everywhere` exists only so *other* server processes see a bump before
the CLI process exits, which does not apply to the server answering its own
request). The per-account floor this run measured is shared with option 1:
validation + insert + tap dispatch + role grant ≈ 14 ms — `UserService` does
not know whether its caller is the CLI or a route handler.

**This run did not measure the HTTP path itself** (login, CSRF token,
`require_admin`, the per-route rate limiter, response rendering) — there is no
JSON API for user creation today, only the session- and CSRF-protected
`/admin/people/add` form (`crates/kernel/src/routes/admin_user.rs:1912-1935`),
and driving it 1,000 times from a script needs a login flow, a CSRF token per
request or reused across requests, and `TROVATO_RATE_LIMIT_*` raised the way
`scripts/bench.sh` already documents doing for its own benchmarks. Per the
constraint that every number here comes from a measurement taken in this run,
option 4's total projected time is intentionally left open — it inherits the
~14 ms/account floor, plus an unmeasured HTTP/auth/session overhead per
request.

**What it changes.** Either a new JSON endpoint under `/admin` (auth +
permission-checked, reusing `UserService::create`) or a script that drives the
existing HTML form. A new endpoint is the smaller, more auditable change.

**Risks.** Exposes bulk creation over the network (more attack surface than a
CLI that already requires database access); needs its own rate-limit
carve-out or a very patient script; still needs the same input-format and
idempotency decisions as option 1, just over HTTP instead of a file.

## For the batch option: open questions, settled

- **Input format and columns.** JSON Lines, one object per row:
  `{"name": "...", "mail": "...", "roles": ["..."], "status": "active"|"blocked", "password": null | "<hash-spec>"}`.
  JSON Lines over CSV: roles are naturally a list (CSV needs a sub-delimiter
  convention), a row is independently parseable (a CSV file needs the header
  before any row means anything, and a byte cut mid-file is more likely to
  silently corrupt one field than desync the whole parse), and `serde_json`
  is already a kernel dependency with no new parser to validate. Unknown keys
  are rejected (catches a typo'd column name instead of silently ignoring
  it, the same spirit as `config import`'s existing validation).
- **Validate the whole file before any write.** Mirrors `config import
  --dry-run` precedent (`crates/kernel/src/main.rs:176-184`): parse every
  row, check every name and mail for both intra-file collisions and existing
  rows (one batched query, not N), check every named role exists, and report
  every problem with its line number before writing anything. A large file
  with a typo'd role name on row 999 should not leave 998 accounts created
  and the operator debugging why row 999 alone failed.
- **A bad row after validation passes (a role deleted mid-run, a DB error).**
  Skip and report, continue the rest, and end with a summary (`N created,
  M skipped: <line, reason>`). Stopping on first error means one unlucky row
  blocks every account after it in the same file; this is the same tradeoff
  `user create`'s own role-grant code already makes (`crates/kernel/src/main.rs:1504-1523`,
  "every grant is still tried, and the failure names what is missing").
- **Transactions and partial failure.** One transaction **per row**
  (insert + audit row + role grants), not one transaction for the whole
  file. A whole-file transaction would hold one pooled connection and
  row locks for the entire import and turn one bad row into an all-or-
  nothing failure for everyone after it; per-row transactions mean a crash
  mid-file leaves exactly the rows already committed, and combined with the
  idempotency rule below, simply re-running the same file finishes the rest.
- **Idempotency on rerun.** An existing name is skipped, not an error
  (reported in the summary), by default — the common case is re-running the
  same file after fixing a few rows or after a crash. A `--strict` flag makes
  an existing name a validation failure instead, for an operator who wants
  rerun to mean "nothing happened yet."
- **Passwords: never on the command line, and not a per-row hash by
  default.** The measured ~386 ms Argon2id cost (RFC 9106 parameters) is the
  single largest per-account cost after plugin compilation and the cache-clear
  sleep; hashing it for every row serially is what turns a 17-second import
  into a 6.7-minute one. Default: no password (`password: null`), matching
  `user create --no-password` — the account cannot sign in until
  `trovato user reset-password` or a reset-link flow sets one. An explicit
  `"password": "<argon2-hash>"` column is accepted for an operator migrating
  hashes from another system (never a plaintext password in the file); the
  import does not hash plaintext itself. A future reset-link-per-row mode is
  out of scope here (it needs the email service and a token flow this design
  does not touch).
- **Security events and audit rows per account.** Matching what `user create`
  already writes (`crates/kernel/src/services/user.rs:264-325`,
  `services/role.rs:40-65`): one `create`/`user` audit row (when
  `trovato_audit_log` is enabled, as it is by default); per granted role, one
  role-assignment row, one `roles`/`user` audit row and one
  `UserRolesChanged` security event. No security event is written for the
  account creation itself today (only for a later block/unblock or role
  change) — the batch path should not invent one `user create` doesn't have.
- **Progress output.** One line per row as `created|skipped <name>: <reason>`
  is too slow to watch for 1,000 rows and too fast to read; the way
  `scripts/bench.sh` reports a summary after the fact, `user import` should
  print a running count every N rows (or every second) and a final summary
  table, not a line per account.

## Recommendation

Build **option 1, the batch command**, with no-password-by-default rows
(this design's settled answer) as the only early target: it is the only
option whose projection (≈17s for 1,000 accounts) is close to what an
operator actually wants, and it needs no change to `UserService`, the tap
path or the audit trail — only a new CLI subcommand and an input parser.
Pair it with **option 3** (wasmtime's module cache) as a small, low-risk,
broadly-useful follow-on that also speeds up every other plugin-loading CLI
command, not represented in option 1's own projection since that already
pays plugin compile exactly once per batch. Option 2 is worth building later
if a long-running import or migration tool wants to load even fewer services
than `AppState::new` builds today, but it is not needed to hit the 1,000-
account target and needs an `.info.toml` taps declaration that doesn't exist
yet. Option 4 needs its own measurement of the HTTP/auth path before it can
be compared on numbers rather than architecture; it is not blocked by
anything here and could be scoped independently.

Next prompts, each one pull request, in order:

1. Add a `taps` list to plugin `.info.toml` (checked against what the
   compiled module actually exports at load time, so a stale declaration
   cannot silently drop a hook the module does implement) — prerequisite for
   option 2, and useful on its own for the boot log and admin UI to say what
   a plugin actually hooks.
2. `trovato user import <file>`: JSON Lines parser, whole-file validation
   pass (names/mails against each other and the database in one batched
   query, roles against `roles`), one `AppState::new`, one
   `UserService::create`/`create_with_status` call per row in its own
   transaction, skip-existing-by-default with `--strict`, one final cache
   clear, progress and summary output — per the settled answers above.
3. Enable wasmtime's on-disk module cache in `create_engine`
   (`crates/kernel/src/plugin/runtime.rs`), with a configurable cache
   directory and a note in `INSTALL.md`/deployment docs about where it lives
   on a multi-server site.
4. (Needs its own measurement first.) An authenticated JSON endpoint for
   account creation reusing `UserService::create`, with its own rate-limit
   policy, benchmarked the way `scripts/bench.sh` benchmarks read paths
   today, before deciding whether it is worth building as a second bulk path
   alongside `user import`.
