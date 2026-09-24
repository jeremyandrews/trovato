# Trovato security review brief

**For:** an external reviewer, either a Tag1 engineer who has not committed to
Trovato, or an outside firm. It assumes no prior knowledge of the codebase and
no access to the maintainer.

**Subject:** `github.com/jeremyandrews/trovato` at `main`, commit `6177ba6`,
release `v0.104.0` (tagged 2026-09-24).

**Written:** 2026-09-24.

**Why this exists.** `KNOWN-ISSUES.md` says it plainly: "Treat the security
posture as 'reviewed once, by the author' until that changes." Getting the
security findings from private development independently verified on a public
codebase is recorded as a 1.0 blocker (BL-66 in `docs/BACKLOG.md`). This brief
is the response to an issue the project files against itself. Nothing here is a
claim that the code is sound. It is a map of where the boundaries are, what
each is supposed to guarantee, and what already pins it, so a reviewer spends
their time on judgment rather than on orientation.

**Prior art, linked rather than repeated.**
[`docs/security-audit.md`](../security-audit.md) is the dependency advisory
policy: `cargo audit` in CI on every pull request and push to `main`, a
severity response table, and the rules for suppressing an advisory in
`.cargo/audit.toml` with a justification and a review date. Read it once. This
brief does not restate it, and dependency advisory handling is not the subject
of the review except where section 2 says otherwise.

**A note on the claims in this document.** Every file and line reference was
read at `6177ba6`. Where this brief says an invariant has no test, that is the
result of a search, and a search can miss things. Treat "no test" as "none
found, worth confirming" rather than as proof of absence.

---

## 1. Surfaces

Each surface below gives three things: where the code is, what the invariant is
supposed to be, and which tests pin it. Read the invariant as the claim under
review, not as a statement of fact.

### 1.1 The WASM plugin sandbox

**Where.** `crates/kernel/src/plugin/runtime.rs` (engine and load),
`plugin/limits.rs` (resource bounds), `plugin/info_parser.rs` (manifest),
`plugin/db_policy.rs` (table allowlist), `crates/kernel/src/host/*.rs` (the 13
host interfaces), `crates/wit/` (the WIT world).

**Runtime.** wasmtime is declared once as `wasmtime = "47"`
(`Cargo.toml:112`) and resolves to **47.0.4** (`Cargo.lock`). Only the kernel
and the phase0 benchmark consume it, both through the workspace pin, both with
default features. `wasmtime-wasi` is **not** in the dependency graph at all:
WASI is five hand written stubs that return `ENOSYS`
(`runtime.rs:919-933`). Engine configuration (`runtime.rs:800-820`) sets
threads off (the comment names RUSTSEC-2025-0118 as the reason), epoch
interruption on, fuel conditional, Cranelift `Speed`, and the pooling
allocator. SIMD, bulk memory, reference types and the wasm stack size keep
wasmtime 47 defaults; nothing configures them.

**Resource limits** (`plugin/limits.rs:39-76`), all overridable by environment
variable with a documented fallback (`config.rs:120-192`):

| Bound | Default | Override |
|---|---|---|
| Memory | 64 MiB | `PLUGIN_LIMIT_MEMORY_BYTES` |
| Table elements | 10,000 | `PLUGIN_LIMIT_TABLE_ELEMENTS` |
| Memories / tables / instances | 1 / 1 / 1 | `PLUGIN_LIMIT_*` |
| Tap epoch deadline | 10 s | **none** |
| Invoke epoch deadline | 10 s | **none** |
| Background tap deadline | 150 s | `PLUGIN_BACKGROUND_TAP_DEADLINE_SECS` |
| Fuel | **off** | `PLUGIN_ENABLE_FUEL`, `PLUGIN_FUEL_LIMIT` |

*Invariant:* a plugin cannot exhaust host memory or CPU, and a breach is
attributed to the plugin rather than failing anonymously. Growth breaches raise
a trap carrying the plugin name (`limits.rs:165-198`).

*Worth a reviewer's attention:* the epoch deadline charges **guest compute
only**. Time parked in a host call is credited back
(`tap/dispatcher.rs:364-377`), by design, labelled F-WALLCLOCK. Per host call
bounds exist separately (database 5 s, HTTP transfer budget 60 s), but a plugin
looping over slow host calls has no single overall deadline. Fuel being off by
default means epoch interruption is the only CPU bound in the shipped
configuration.

**Capability model.** A plugin ships `{name}.info.toml` with a `[capabilities]`
table parsed into six fields (`info_parser.rs:158-251`), every one deny by
default; an absent table denies everything.

| Capability | Enforced by | At |
|---|---|---|
| `host_interfaces` | `host::register_declared` | link time, `host/mod.rs:104-113` |
| `db_tables` | `DbPolicy::check_table` | every structured db call, `host/db.rs:286-291` |
| `raw_sql` | `DbPolicy::check_raw_sql` | `host/db.rs:745`, `:801` |
| `ai_background` | `authorize_ai_request` | `host/ai.rs:138-163` |
| `public_functions` | `function_is_public` | `host/plugin_api.rs:336-338` |
| `http_max_transfer` | `clamp_transfer_ceiling` | load time, `host/http.rs:89-93` |

*Invariant:* `host_interfaces` is enforced at **link** time, not per call. A
plugin importing an interface it did not declare fails to load
(`runtime.rs:867-882`), which is a stronger position than a per call check
because an undeclared import cannot be reached at all.

*Note for the reviewer:* manifest parsing validates only interface name
spelling against a known list. `db_tables` entries are not validated, not even
as SQL identifiers.

**The `db_tables` allowlist.** The effective set is the union of tables the
plugin's own migrations create and the manifest's explicit `db_tables`, derived
once at load (`db_policy.rs:147-171`).

*There is no prefix rule.* `check_table` (`db_policy.rs:177-186`) is exact
string membership. Nothing restricts a plugin to tables named after itself and
nothing refuses a kernel table. The project records the consequence at BL-105:
declaring the kernel's `users` table in `db_tables` is refused by no policy and
hands the plugin every column. That row is explicitly placed in the scope of
this review.

**`raw_sql` and the `query-raw` read only guard.** `host/db.rs:42-75`. The
guard is `is_read_only`, which takes the first whitespace delimited token after
stripping leading comments and accepts `SELECT` or `WITH`. `execute-raw`
additionally rejects a first keyword in a six entry `DDL_KEYWORDS` list.
`has_semicolons` rejects any semicolon anywhere.

*Invariant, as documented:* `query-raw` is read only. The SDK states it
(`crates/plugin-sdk/src/host.rs:208-210`).

*The known first keyword weakness:* a data modifying CTE (`WITH x AS (DELETE
... RETURNING *) SELECT * FROM x`) begins with `WITH`, carries no semicolon, and
writes. This is not hypothetical: the Argus plugin depends on it
(`plugins/argus/src/notify_ports.rs:285`). Recorded as BL-61 /
G-QUERY-RAW-FIRST-KEYWORD, open, targeted 1.0.x, graded not an escalation
because `raw_sql` already grants `execute-raw`.

*A second weakness, apparently unrecorded, found while writing this brief.*
`first_sql_keyword` skips a block comment by finding the **first** `*/`
(`host/db.rs:48-50`). PostgreSQL nests `/* */`. So `/* /* */ SELECT 1 */ DROP
TABLE t` reads as `SELECT` to the guard and executes as `DROP TABLE t` in
Postgres, with no semicolon to trip `has_semicolons`. This defeats the DDL
rejection on `execute-raw`, which is the one thing `raw_sql` is documented not
to grant, so its consequence is worse than BL-61's. The Rust was read directly;
the end to end behaviour is inferred from documented Postgres nesting and has
**not** been executed against a live database. Confirming or refuting it with
one query is the single highest value first task in this review.

Separately, `DDL_KEYWORDS` is a six entry denylist (`CREATE`, `DROP`, `ALTER`,
`TRUNCATE`, `GRANT`, `REVOKE`). It omits `COPY`, `CALL`, `DO`, `SET`, `LOCK`,
`REFRESH`, `REINDEX`, `COMMENT`, `VACUUM`, `ANALYZE` and others. A denylist of
first keywords is structurally the wrong shape for this guard.

Stacked queries are blocked twice: by `has_semicolons`, and independently
because `sqlx::query()` always carries an arguments vector and therefore always
uses the Postgres extended query protocol, which does not permit multiple
statements.

**Tests.** Limits: `plugin/limits.rs` (`default_limits_match_documented_constants`,
`memory_growth_over_cap_errors_with_attribution`,
`table_growth_over_cap_errors_with_attribution`) and against real guests in
`tap/dispatcher.rs` (`greedy_memory_plugin_stopped_at_limiter_cap_kernel_stays_healthy`,
`spin_loop_plugin_dies_at_epoch_deadline`, `fuel_exhaustion_traps_when_enabled`).
Interfaces: `tests/plugin_test.rs` (`register_declared_grants_only_named_subset`,
`load_rejects_undeclared_host_import_with_declarative_error`,
`in_tree_plugins_load_with_declared_subsets`). Allowlist: `plugin/db_policy.rs`
(`check_table_allows_listed_and_rejects_others_with_exact_message`,
`derive_none_capabilities_denies_all`), `host/db.rs`
(`do_select_rejects_undeclared_table` and its three siblings),
`tests/e2e_invocation_test.rs` (`db_sandbox_blocks_undeclared_raw_sql_during_invoke`).
Guards: `host/db.rs` (`read_only_guard`, `ddl_guard_rejects_ddl`,
`ddl_guard_allows_dml`).

**No test found for:** the wasmtime version pin or any engine hardening flag, so
a silent revert of `wasm_threads(false)` or `epoch_interruption(true)` would
pass CI; `has_semicolons`; the `WITH` acceptance in either direction, so
tightening it would break Argus with nothing to catch it at test time; the
three epoch deadline constants; `db_tables` entries being unvalidated at parse
time.

### 1.2 The HTTP host interface and the SSRF fence

**Where.** `crates/kernel/src/host/http.rs` (the plugin fence),
`crates/kernel/src/services/ai_provider.rs` (the AI fence).

**The plugin surface.** `trovato:kernel/http` `request` takes a JSON
`HttpRequest` of url, method, headers, body, timeout. **Any** method string,
**any** headers and **any** body are accepted: there is no header denylist, so
`Host`, `Authorization` and `X-Forwarded-*` are all settable by the plugin.
Limits: timeout 30 s default and 60 s maximum, response body 1 MB checked twice,
redirects capped at 10, streaming reads capped at 64 KB per read with a 1 MB
default transfer ceiling (16 MB manifest maximum), 8 concurrent handles, 60 s
transfer budget. The gate is the manifest declared `http` host interface. There
is no per plugin allowlist of destination hosts; `http_max_transfer` is a byte
ceiling, not a destination list.

**There are two fences, and they are not the same code.**

*Fence A, the plugin fence,* three layers (`host/http.rs`):
`check_url_policy` (`:363-389`) rejects non HTTP(S) schemes, `localhost`,
`*.local`, `*.internal`, `*.localhost`, and private IP literals;
`ValidatingResolver` (`:527-551`) resolves, denies if **any** resolved address
is private, and returns exactly those addresses so the connection is pinned,
which is what closes DNS rebinding; `revalidating_redirect_policy` (`:559-570`)
re-runs the policy per hop. Everything built from `build_outbound_client` gets
all three: the plugin `request` and `http-open` paths, the tap dispatch client,
the plugin install client, and the cron and queue worker client. Argus feed
fetches, Argus webhook notifications, the Turnstile captcha call and the update
check all inherit it.

*Fence B, the AI fence,* one layer: `validate_base_url`
(`ai_provider.rs:388-424`), a **string only** check. It rejects non HTTP(S)
schemes, private IPv4 literals, `localhost` and two Google metadata hostnames.
It does **not** resolve DNS, despite its own doc comment saying "Host must not
resolve to a private, loopback, or link-local IP range". It does not reject
credentials in the URL, restricts no ports, and does not block `.internal` or
`.local` suffixes that fence A does block. The AI HTTP client is a bare
`reqwest::Client::builder()` (`ai_provider.rs:475-478`) with no pinning
resolver and reqwest's default 10 hop redirect follow.

*Invariant, as intended:* no outbound request initiated by untrusted input
reaches a private, loopback, link local or metadata address, at request time or
after a redirect.

**The chat completion path does not re-validate the provider base URL.
Confirmed.** The base URL is stored as JSON in the `site_config` row keyed
`ai_providers`. `validate_base_url` has exactly four call sites repo wide:
`admin_ai_provider.rs:256` and `:436` (the create and edit admin forms),
`ai_provider.rs:803` (`embed`) and `:876` (`test_connection`). None is on a
chat path. `save_provider` (`ai_provider.rs:578-593`) does not validate. At
chat time `ai_chat.rs:491-495` formats the stored `base_url` into the request
URL and sends it. The same shape holds for the plugin `ai-request` host, the
assistant's tool calling, AI search and AI assist.

Who can set it: a user holding `configure ai`. The admin form does apply the
check, so a bare private literal is refused there, but three bypasses survive
to chat time because nothing revalidates: a hostname that resolves to a private
address, DNS rebinding after the form passes, and a bracketed IPv6 literal.
What they reach: any internal HTTP endpoint the site can route to, carrying the
provider's API key as an `Authorization` header and attacker chosen prompt text
as the body, with the response returned into the chat UI. That makes it a read
capable SSRF rather than a blind one. Recorded as BL-57 /
G-AI-BASEURL-UNCHECKED, open, graded a missing seatbelt on an already
administrative role rather than a privilege escalation.

*A finding beyond BL-57, apparently unrecorded.* **Bracketed IPv6 literals
defeat all three layers of the plugin fence, not just the AI one.**
`check_url_policy` calls `host.parse::<IpAddr>()` on `host_str()`, and the
`url` crate returns an IPv6 host **with** brackets, so `"[::1]".parse()` fails
and the entire IPv6 arm of `is_private_ip` is unreachable dead code. Layer 2
cannot compensate: hyper-util skips DNS resolution entirely for IP literals, so
`ValidatingResolver` is never invoked. Layer 3 re-runs the same bracket blind
check. Net effect: any plugin declaring `host_interfaces = ["http"]` reaches
`http://[::1]:PORT/` and `http://[fd00::...]/` on a dual stack host, which is
exactly the localhost sidecar case the fence exists to block. BL-57 mentions
IPv6 literals but scopes the gap to `validate_base_url` in the AI provider. The
suggested shape of a fix is to match on `url::Host` rather than `host_str`, in
both fences.

Also worth noting: `is_private_ip` treats neither IPv4 mapped IPv6
(`::ffff:127.0.0.1`) as loopback, in either fence.

**Tests.** Plugin fence, in `host/http.rs`: `open_applies_the_ssrf_fence`
(walks loopback, private, link local, `.internal` and a non HTTP scheme),
`one_shot_denies_dns_rebinding`, `streaming_denies_dns_rebinding`,
`rebinding_mixed_addresses_are_denied`,
`one_shot_denies_redirect_to_private_target`, `redirect_cap_is_preserved`.
AI fence, in `ai_provider.rs`: `base_url_rejects_non_http_schemes`,
`base_url_rejects_private_ips`, `base_url_rejects_cloud_metadata`,
`private_ip_detection`.

**No test found for:** either fence against a bracketed IPv6 literal, in any
form; the admin route rejecting a private base URL end to end
(`admin_ai_provider.rs` contains zero test functions and no integration test
posts to those routes, so only the pure function is covered and the wiring is
not); `save_provider` accepting an unvalidated URL; credentials in a URL; the
chat path's lack of validation, which is instead **depended on** by
`argus_pipeline_test.rs` and `argus_notify_test.rs`, so a future fix will
surface as two confusing integration failures rather than a clear red test.

### 1.3 Authentication

**Where.** `crates/kernel/src/session.rs`, `services/session_registry.rs`,
`middleware/session_tracking.rs`, `routes/auth.rs`, `models/user.rs`,
`lockout.rs`, `services/webauthn.rs`, `routes/webauthn.rs`,
`models/api_token.rs`, `middleware/api_token.rs`, `middleware/bearer_auth.rs`,
`form/csrf.rs`.

**Sessions.** Redis backed via `tower-sessions` 0.14.0 and
`tower-sessions-redis-store`. The session id is 128 bits from the library's
CSPRNG, rendered as 22 URL safe base64 characters; the kernel never constructs
one. The cookie carries only the id and is **not** hashed at rest, because the
store keys records by the bare id string (the audit trail does hash it).
Cookie flags (`session.rs:34-40`): `Secure` and `HttpOnly` hardcoded true,
`SameSite` configurable and defaulting to `Strict`, expiry `OnInactivity(24h)`,
no explicit `Path` or `Domain`.

*Invariant:* every authentication state change rotates the session id. There is
one seam, `setup_session` (`routes/auth.rs:194-211`), whose first act is
`cycle_id()`, and password login, passkey login, recovery, password change,
going passwordless and a username change all go through it.

*Worth attention:* there is **no absolute session lifetime**, only the 24 hour
idle timeout. "Remember me" is written to the session and read nowhere;
`REMEMBER_ME_SESSION_EXPIRY_DAYS` is dead code.

**Passwords.** Argon2id, v0x13, m=64 MiB, t=3, p=4, 32 byte output, per hash
`OsRng` salt (`models/user.rs:449-467`). An empty stored hash never verifies,
so a passwordless account cannot be logged into with an empty password. Reset
tokens and email verification tokens are both 32 random bytes hex, SHA-256 at
rest, single use via `used_at`, expiring at 1 hour and 24 hours respectively;
verification tokens are purpose scoped.

**Lockout** (`lockout.rs:10-17`): 5 failures in a 15 minute window locks for 15
minutes. It is keyed on the submitted **username string only**
(`lockout.rs:151-157`). There is no IP dimension. An unknown username, an
inactive account and a wrong password all increment the counter
(`routes/auth.rs:289-331`), so an attacker who knows a username can lock that
account, and can also create Redis keys for usernames that do not exist. The
only brake is the per IP `login` bucket at 5 per minute, which is enough to
lock one account in one minute and does not constrain a rotating source IP.

**WebAuthn.** `webauthn-rs` 0.5.5 with `danger-allow-state-serialisation`.
*Invariant, and it holds:* the relying party origin is pinned to `SITE_URL` and
the RP ID is that URL's bare host, built once at startup
(`services/webauthn.rs:87-108`). No code path reads the request `Host` header,
so the Host derived origin defect is **not** present. Challenge state lives in
the session with a kernel side 300 s TTL independent of the browser's advisory
timeout, and is removed **before** verification so a replayed finish has
nothing to verify against. The counter is re-checked by the kernel itself
rather than trusting the library; a regression flags and audits but
deliberately does not auto revoke. A passkey is a **full password replacement**,
not a second factor, and an account may drop its password entirely only when a
passkey and a non password recovery path both exist. The kernel sets no
explicit user verification policy, so whatever `webauthn-rs` 0.5.5 defaults to
applies.

**API tokens.** 32 random bytes hex, SHA-256 at rest, raw value returned once,
maximum 25 per user, optional expiry enforced in the lookup query, revocation a
hard delete scoped to the owner. **Tokens carry no scopes**: a token grants the
full permission set of its owning user. Revocation is **not immediate**, because
a 60 second process local cache serves deleted tokens
(`models/api_token.rs:11-19`) and `delete` does not invalidate it.

**CSRF.** Tokens are random rather than derived: 32 bytes hashed with a
timestamp, stored in the session as a bounded ring of 10, valid one hour,
single use, compared in constant time. *CSRF is not middleware.* There is no
central enforcement point; it is opt in per handler through three helpers plus
the form service. Known exempt state changing routes are all pre
authentication (JSON login, passkey login start and finish, password reset,
JSON register) plus the deliberate bearer exemption below. Because there is no
central layer, completeness of that exempt list cannot be asserted without
auditing every write handler.

**The bearer bypass rule.** Two systems share the `Authorization: Bearer`
header: opaque API tokens (`middleware/api_token.rs`) and OAuth JWTs
(`middleware/bearer_auth.rs`).

*The rule, and it is the correct one:* the exemption skips **CSRF only**, and
only on plugin API routes (`routes/plugin_api.rs:305`). It is keyed on an
`ApiTokenAuth` marker that the middleware sets **only when the token actually
authenticated the request**. A cookie is ambient credentials a browser attaches
to a cross site request by itself, which is what CSRF tokens exist to stop; a
bearer token is never attached automatically, so a request authenticated by one
carries no CSRF exposure. Permission checking is not skipped.

Both dangerous combinations were checked and both are handled safely. A request
carrying **both** a valid cookie and a bearer token: the cookie wins, the
middleware returns before inserting the marker (`api_token.rs:60-63`, insert at
`:111`), so CSRF still applies. A bearer header that **fails** to authenticate:
hard 401 before any handler runs, on both the token and the JWT path, so the
"failed bearer falls back to cookie auth while keeping the exemption"
combination cannot occur.

*A consequence apparently not reasoned about, found while writing this brief.*
`middleware/api_token.rs:85` writes `SESSION_USER_ID` into the session, which
the session layer then sets as a cookie. The middleware's own doc comment
acknowledges this and frames it as an efficiency. The security reading is
different: an API token presented once from anything holding a cookie jar
converts into an ambient, CSRF exposed cookie session for that user, valid 24
hours idle across the whole site, and that session **survives token revocation
entirely**, because revocation deletes the database row and touches neither the
session store nor the session index.

**Tests.** Sessions: `tests/session_management_test.rs` (index, two devices,
`a_revoked_sessions_next_request_fails`, `revoke_others_keeps_the_calling_session`,
`the_session_index_survives_a_cycle_id`). Passwords: `models/user.rs`
`test_password_hashing`, labelled a security regression test, asserting the
Argon2id parameters. WebAuthn: three test files covering registration,
authentication and management, including `passkey_login_cycles_the_session_id`,
`a_replayed_assertion_is_rejected`,
`a_counter_regression_is_rejected_and_flags_without_revoking`,
`another_accounts_credential_cannot_complete_this_flow`,
`revoking_the_last_way_into_an_account_is_blocked_and_audited`. CSRF and
bearer: `tests/plugin_api_test.rs`
(`a_bearer_authenticated_write_needs_no_csrf_token`,
`a_cookie_authenticated_write_without_a_csrf_token_is_refused`,
`a_form_token_cannot_be_replayed`,
`a_token_minted_for_one_session_does_not_verify_in_another`).

**No test found for:** the cookie plus bearer combination, which is the
invariant the entire exemption rests on and which is held up by six lines;
cookie flags, since no test reads a `Set-Cookie` header; **`lockout.rs` has zero
tests**, and the harness actively clears lockout state, so a regression
disabling lockout entirely would go green; the password reset flow end to end;
any test of `/api/tokens` at all, leaving expiry, the 25 token cap, owner
scoped deletion and the revocation lag unpinned; the CSRF token's one hour
expiry and the 10 token ring.

### 1.4 The permission model

**Where.** `crates/kernel/src/permissions.rs`, `models/role.rs`,
`routes/helpers.rs`, `plugin/permission_registry.rs`.
Documentation: `docs/admin-permissions.md`.

A permission is a bare string; the kernel's own set is one `&[&str]` constant
(`models/role.rs:35`). Roles carry permissions through `role_permissions`,
users get roles through `user_roles`, and a user's effective set is the union
over their roles plus the well known authenticated role. The two default roles
are fixed UUIDs 1 and 2 and neither can be deleted; the anonymous user is the
nil UUID.

*There is no extractor and no guard type.* The check is a plain async helper
called as the first statement of each handler (`routes/helpers.rs:101`), so
omitting it is silent and nothing at the type level or in a router layer forces
a new admin handler to carry one. The posture rests on roughly 111 correct call
sites.

**The superuser short circuit.** `users.is_admin` bypasses every permission
check, at three layers (`permissions.rs:57-64`, `routes/helpers.rs:115-118`,
`tap/request_state.rs:163-165`). There is no "user 1" magic; the column is set
by the installer on the first user and thereafter only by an existing
superuser. It is not grantable through a role, not in `KERNEL_PERMISSIONS`, and
not writable by config import or the CLI. BL-33 (fixed at `7ef8c15`) separated
this from the permission string `administer site`, which had been a forgeable
authority and is now an ordinary permission.

**Admin routes still gated on `is_admin`.** This is the list a reviewer should
have, because `docs/admin-permissions.md` claims at line 160 that exactly two
routes keep `require_admin`, and advertises itself at lines 3 to 5 as "the
whole list; it is what `crates/kernel/src/routes/` actually checks, not a
summary of intent". The code has nine handlers plus one inline check:

| Route | Method | File:line | Gate |
|---|---|---|---|
| `/admin/plugins` | GET | `routes/plugin_admin.rs:50` | `require_admin` |
| `/admin/plugins/toggle` | POST | `routes/plugin_admin.rs:138` | `require_admin` |
| `/admin/queue/dlq` | GET | `routes/admin_queue.rs:39` | `require_admin_json` |
| `/admin/queue/dlq/{id}/requeue` | POST | `routes/admin_queue.rs:86` | `require_admin_json` |
| `/admin/queue/dlq/{id}/delete` | POST | `routes/admin_queue.rs:120` | `require_admin_json` |
| `/admin/embed/status` | GET | `routes/admin_embed.rs:44` | `require_admin_json` |
| `/admin/embed/backfill` | POST | `routes/admin_embed.rs:75` | `require_admin_json` |
| `/admin/stage/switch` | POST | `routes/admin.rs:55` | `require_admin_json` |
| `/admin/stage/current` | GET | `routes/admin.rs:75` | `require_admin_json` |
| `/api/v1/user/export` | GET | `routes/api_v1.rs:218-227` | inline `u.is_admin`, no helper |

Only the first two are documented, and only for them is a rationale recorded
("they change who holds privilege rather than exercising it"). That rationale
does not cover the dead letter queue, the embed backfill or the stage switch,
which are operational rather than privilege changing. The effect runs both
ways: a delegated administrator holding every named permission still gets 403
on all nine, and conversely anyone who needs to requeue a dead job or switch an
editorial stage must be made a full superuser, which also hands them
`administer users` and every future permission.

**Plugin declared permissions.** A plugin returns `Vec<PermissionDefinition>`
from `tap_perm`, dispatched at boot and persisted to `plugin_permission`
(`plugin/permission_registry.rs`). That table is a **cache of declarations,
never a grant**: `role_permissions` remains the only place a permission is held.
Three rejections stop collision or escalation at declaration time: a name in
`KERNEL_PERMISSIONS`, a name another plugin already declared, and an empty or
over long name. A rejection is logged and skipped, never fatal.

*Note:* `crates/kernel/src/plugin/gate.rs` is **not** part of the permission
model despite the name. It is plugin auto enablement and conditional route
registration.

**Tests.** `tests/admin_permission_gates_test.rs`
(`a_role_holding_the_permission_reaches_the_admin_screen`,
`holding_one_admin_permission_does_not_open_the_others`,
`the_superuser_bypass_still_reaches_every_converted_screen`),
`tests/access_administration_pages_test.rs` (seven functions on the admission
permission), `tests/permission_grid_save_test.rs`
(`a_grant_the_grid_never_rendered_survives_a_save`, pinning the incident where
saving the grid destroyed 29 plugin grants on a live site),
`tests/plugin_permissions_test.rs`, `tests/one_administrator_test.rs`,
`tests/user_role_assignment_test.rs`
(`a_delegated_administrator_cannot_set_the_superuser_flag`).

**No test found for:** both `/admin/embed` routes and both `/admin/stage`
routes, which have no gating test at all; the DLQ gate is weak, asserting only
a non 200 for an unauthenticated request, so it would still pass if the gate
were downgraded to `require_login`; nothing asserts `docs/admin-permissions.md`
matches the code, which is why it drifted to "two routes" while the code has
nine.

### 1.5 The config import path

**Where.** `crates/kernel/src/config_storage/yaml.rs` (import),
`config_storage/direct.rs` (writes), `main.rs:103-112` (the CLI verb).

Imported config is a directory of `.yml` files on local disk read by a CLI
subcommand. **There is no HTTP upload and no admin screen for it**, so no
permission gates it: the requirement is shell access plus `DATABASE_URL`.
(`routes/admin_config.rs` is the site settings form, not import.)

| Question | Answer |
|---|---|
| Create or alter users? | No. No user entity type, no `save_user`. |
| Create or alter roles and permission grants? | **Yes**, with replace semantics: a permission the file does not name is revoked; an absent key leaves grants untouched. Cannot grant superuser. |
| Enable a plugin? | No. Plugin state is written only by the CLI or the toggle route. |
| Write arbitrary keys? | **Yes**, for `variable`: a straight upsert into `site_config` with no allowlist, no key validation and untyped JSON. |
| Schema check? | Yes, per entity type, serde based, in a parse pass that precedes every write; a failure writes nothing. |
| Signature or provenance? | **No.** Trust is filesystem trust. |
| Diff or preview? | `dry_run` validates and skips the write. It prints counts, not a diff. |

*Invariant:* an import is atomic. Parse and reference resolution both precede
any write, and a failure in either leaves the database untouched.

*Worth attention:* the `variable` entity is the widest surface here. Anyone who
can write the config directory owns every site setting the kernel or any plugin
reads. `models/site_config.rs:35` records that open user registration has
already been flipped this way. And because role permission import is replace
semantics while `dry_run` prints only counts, the single most security relevant
outcome, which grants a role file will revoke, is invisible before it happens.
A preflight that cannot show a revocation is not a preview.

Permission name validation draws on three sources: `KERNEL_PERMISSIONS`,
`plugin_permission` rows, and anything any role already holds. The third means
a typo granted once on a database validates there forever.

Config import also seeds **content**: the `item` entity writes rows directly
through `save_item`, with the author fixed to the nil UUID and the stage fixed
to live, and without creating a revision.

**Tests.** `tests/config_import_test.rs`:
`import_fails_and_writes_nothing_when_one_file_does_not_parse`,
`import_fails_on_schema_mismatch_not_just_malformed_yaml`,
`dry_run_fails_on_the_same_input_a_real_run_fails_on`,
`a_role_file_grants_exactly_the_permissions_it_declares`,
`a_role_file_without_the_key_leaves_existing_grants_alone`,
`an_unknown_permission_fails_validation_loudly`,
`a_permission_some_role_already_holds_is_accepted`,
`item_import_sets_promote_sticky_and_created_and_updates_them`.

**No test found for:** any bound on what a `variable` entity may write, which is
the absence of an invariant rather than an untested one.

### 1.6 The theme engine's HTML sanitisation

**Where.** `crates/kernel/src/theme/engine.rs`, `theme/render.rs`,
`content/filter.rs`, `content/block_render.rs`, `content/block_types.rs`,
`templates/`.

Tera 1.20.1. Autoescape is on by default for `.html`, `.htm` and `.xml` only,
and the kernel never calls `set_autoescape_suffixes`. `.txt` templates load
into the same instance and are **not** autoescaped; the five that exist are
plain text email bodies.

*Invariant:* template output is escaped by Tera, and everything reaching a
`| safe` is either kernel generated or already ammonia cleaned. The design
document states the stronger form: "Raw HTML from plugins is structurally
impossible."

`| safe` appears **54 times in `templates/`**, zero times in `plugins/`, and
zero times in kernel Rust source. That is the template layer's whole XSS
surface and it is enumerated in the surface notes below.

Three content formats exist (`content/filter.rs:38-69`): `plain_text`,
`filtered_html` and `full_html`, defaulting to `plain_text`. `full_html` is
unfiltered by design and gated on the `use full_html` permission.
`for_format_safe` downgrades `full_html` and anything unknown to `plain_text`,
and every public render path uses it.

Ammonia 4.1.4 is applied at five places with five different allowlists: the
filtered HTML pipeline, the markdown filter (defaults plus `class` on `code`,
`pre` and `span`, values allowlisted), Editor.js block text, block type text
fields, and the page builder.

Plugin supplied tags and attributes are clamped in Rust before reaching the
element templates (`theme/render.rs:42-56`), with a `SAFE_TAGS` allowlist
excluding `script`, `iframe`, `object`, `embed`, `form`, `style`, `link`,
`meta` and `input`, falling back to `span`.

**Tests.** Mostly unit rather than integration. `content/filter.rs` carries five
blocks labelled security regression tests plus `for_format_safe_rejects_full_html`
and `for_format_checked_downgrades_full_html`. `theme/engine.rs` has
`markdown_filter_sanitizes_dangerous_html`,
`an_arbitrary_class_on_a_div_is_still_stripped`,
`a_language_name_cannot_smuggle_anything`, `the_class_allowlist_is_per_element`.
`theme/render.rs` has `test_process_value_rejects_full_html`,
`test_render_markup_rejects_unsafe_tag`. `tests/theme_test.rs` contains **no**
XSS assertion.

**No test found for:** the claim attached to `templates/search.html:29` that the
search snippet is safe, where the escaping step is a SQL comment in
`search/mod.rs` asserted nowhere, so a query rewrite dropping it would be a
stored XSS with no failing test; the `.txt` no autoescape boundary;
`services/tile.rs:95`, which hardcodes `has_full_html = true` so a tile body
renders unfiltered HTML regardless of who wrote it, with a comment asserting
tiles are admin authored and nothing in the code enforcing it.

### 1.7 The security headers middleware

**Where.** `crates/kernel/src/middleware/security_headers.rs`, layered in
`main.rs:406-409`.

Six headers: the CSP, `X-Frame-Options: DENY`, `X-Content-Type-Options:
nosniff`, `Referrer-Policy: strict-origin-when-cross-origin`,
`Permissions-Policy: camera=(), microphone=(), geolocation=()`, and HSTS
`max-age=31536000; includeSubDomains` (no `preload`) over HTTPS only.

The CSP in full (`security_headers.rs:40-48`):

```
default-src 'self';
script-src 'self' 'wasm-unsafe-eval' https://cdn.jsdelivr.net;
style-src 'self' 'unsafe-inline' https://fonts.googleapis.com;
img-src 'self' data:;
font-src 'self' https://fonts.gstatic.com;
connect-src 'self';
frame-ancestors 'none'
```

**`'unsafe-inline'` is in `style-src` and confirmed.** `script-src` is clean:
no `'unsafe-inline'`, no nonce, no hash. The stated reason is at
`security_headers.rs:38-39`: "100+ inline `style=` attributes would break
without it; inline styles are a low XSS risk." `KNOWN-ISSUES.md` quantifies it
at 305 inline `style=` attributes across 44 templates, plus 22 `<style>` blocks.

*What it costs.* An attacker who lands any attribute or any `<style>` in the DOM
gets arbitrary CSS. That buys UI redressing (a fixed, full viewport overlay over
the real page or over a submit button), input value inference through attribute
selectors, `content:` text that impersonates site copy, and covert relayout of a
confirmation dialog. It does **not** buy script execution, and it does not buy
the classic CSS exfiltration channel, because `img-src 'self' data:` blocks
`background:url(https://attacker/)` and `font-src` is restricted. Two residual
leaks stay open: `style-src` permits an `@import` fetch to a Google host, and
`connect-src` does not govern CSS at all.

There is no nonce or hash mechanism anywhere; the policy is a compile time
constant with only `report-uri` appended. The design choice is explicit:
executable JavaScript must be a file under `static/js/`, and template data goes
in `<script type="application/json">` blocks. Enforcing by default;
`CSP_REPORT_ONLY` switches the header name, not the policy. The layer sits on
the outer app, so it covers every route including static files and the
fallback.

**Tests.** `security_headers.rs` unit tests cover every header value,
`hsts_is_written_only_over_https`, report only mode, and an unusable report URI
keeping the policy. `tests/login_page_csp_test.rs::the_login_page_runs_no_script_its_own_csp_blocks`
reads the live header, asserts `script-src` has no `'unsafe-inline'`, no nonce
and no hash, then asserts every `<script>` on the page is either
`type="application/json"` or has a same origin `src`.

**No test found for:** `style-src` contents at all, so nothing pins
`'unsafe-inline'` there as deliberate and nothing would notice it spreading to
`script-src` on a route other than `/user/login`; the headers being present on
any other route; `frame-ancestors`, `img-src`, `connect-src` or `font-src` as
served.

### 1.8 The page builder's CSS handling

**Where.** `crates/kernel/src/content/page_builder.rs`,
`content/page_builder_components.rs`, `templates/pb/`,
`plugins/trovato_page_builder` (off by default).

An editor supplies values that land inside a `style=` attribute on four
components: `backgroundImage` and `backgroundColor` on hero, `backgroundColor`
on cta and on section wrapper, and `gap` on columns. All are declared as free
strings in the component registry, and `validate_page` does not touch them.

The page builder's own ammonia builder is the loosest of the five:

```rust
// content/page_builder.rs:87-92
fn build_sanitizer() -> ammonia::Builder<'static> {
    let mut builder = ammonia::Builder::default();
    // TODO: restrict allowed CSS properties to a whitelist for production
    builder.add_generic_attributes(["class", "style"]);
```

Ammonia filters the `style` attribute only when `filter_style_properties` has
been called, and it never is, so **`style` is allowed on every element with no
CSS property filtering at all**.

Tera autoescape stops attribute breakout but not property injection: `"` and
`'` are escaped, but `;`, `:`, `(` and `)` are not HTML special, so a
`backgroundColor` of `red; position:fixed; inset:0; z-index:99999` is emitted
intact and survives ammonia.

**The causal link to `'unsafe-inline'` is real but narrower than it looks.** The
page builder produces no `<style>` block; the inline `style=` attributes across
the whole kernel template set are what require the directive, and the page
builder is one producer among several. It is, however, the concrete path by
which non kernel input reaches an inline style. `KNOWN-ISSUES.md` records the
gap (BL-67, class 1.0) and notes the plugin is off by default.

Its `iframe` allowance (added for YouTube embeds) is neutralised by the shipped
CSP, since there is no `frame-src` or `child-src` and `default-src 'self'`
applies. Either the embed feature does not work on a default install or a
deployment is expected to widen the CSP, and nothing records which.

**Tests.** `page_builder.rs::render_xss_in_props_sanitized` covers a `<script>`
in a text prop; `render_max_recursion_depth_errors`; the component validation
tests. **`crates/kernel/tests/` has no page builder test at all.**

**No test found for:** CSS injection through `backgroundColor`,
`backgroundImage` or `gap`; the `style` attribute surviving ammonia; the
`iframe` allowance.

### 1.9 The rate limiter

**Where.** `crates/kernel/src/middleware/rate_limit.rs`.

A fixed window counter in Redis, INCR plus EXPIRE in one Lua script. The module
doc says sliding window; a single key with one TTL is a fixed window, so a
caller can spend twice the limit across a boundary. **It fails open on a Redis
error** (`rate_limit.rs:365-371`), which means a Redis outage removes the
`login` and `recovery` limits entirely.

Two keys: per client IP, and separately per authenticated user.

**`X-Forwarded-For` is not trusted unconditionally.** It is honoured only when
the socket peer is in an explicit `TRUSTED_PROXIES` allowlist, and that
allowlist defaults to empty (`rate_limit.rs:541-573`). So the limiter is **not**
trivially bypassable by a spoofed header on a default install. Two residuals: a
deployment that sets a broad `TRUSTED_PROXIES`, and the standard caveat that
when a proxy is trusted the code takes the **first** XFF entry, which is client
supplied, so a proxy that appends rather than replaces lets a client choose its
own bucket.

Seventeen buckets. Counts are overridable by `TROVATO_RATE_LIMIT_<BUCKET>` or
`rate_limit.<bucket>`; windows are not. Selected values: `login` 5/min,
`register` 3/hour, `password` 5/min, `recovery` 5 per 15 min, `comment` 4/min,
`forms` 30/min, `api` 100/min, `static` 2000/min, `data_export` 1/hour, `mail`
100/hour per plugin.

The IP middleware covers all routes and the category is chosen by path. Five
buckets are checked explicitly in handlers instead. Password reset is covered
through the `recovery` bucket. `/api/v1/ai/assist` falls into the generic `api`
bucket at 100/min.

**Two buckets have no caller**: `profile` and `data_export` are defined,
documented and overridable, but no code calls `check` with either.
`data_export` has a written rationale about bounding GDPR export cost that is
therefore not being enforced.

**Tests.** `rate_limit.rs` covers the trust model well:
`untrusted_peer_ignores_x_forwarded_for` (the core invariant),
`peer_not_in_allowlist_is_untrusted`, `trusted_proxy_honors_x_forwarded_for`,
`trusted_proxy_x_forwarded_for_takes_first_ip`, plus the categorisation set and
the override precedence set. `tests/rate_limit_categories_test.rs` covers the
comment and AI search buckets.

**No test found for:** fail open on Redis failure; the per user limiter firing
at all; the `login` bucket actually biting on `POST /user/login`; the `recovery`
bucket; the fixed window double spend.

### 1.10 The AI assistant's describe and execute split

**Where.** `crates/kernel/src/assistant/registry.rs`,
`services/ai_assistant.rs`, `routes/assistant.rs`, `services/ai_tools.rs`.

A plugin declares scopes from `tap_assistant_scopes`, validated once at boot
into an `AssistantRegistry`; a malformed scope is dropped rather than failing
boot. Every tool is offered to the model. A Read tool executes inline. A Write
tool is dispatched in **Describe** mode only, producing a proposal row that a
person must click Apply on, and that Apply is the only Execute dispatch of a
write anywhere in the kernel.

*The prompt injection question, answered:* **the model cannot name a tool it was
not offered.** `ai_assistant.rs:844` looks the tool up against the scope's own
declared list rather than against whatever was sent to the model, and an unknown
name becomes a refused call. So the "trusting describe" failure mode is not
present at the tool name level.

*What a plugin scope can and cannot widen.* Three bounds hold. The permission
gate ANDs `use ai` and `use ai assistant` with the scope's own permission, so a
scope declaring an empty permission still needs both kernel permissions.
Dispatch is pinned to the declaring plugin, so a scope cannot invoke another
plugin's tools. Scope names are globally unique, so one plugin cannot shadow
another's. The residual is that the scope's permission string is whatever the
plugin wrote, and there is no kernel side allowlist of which permission strings
a plugin may claim.

*Arguments.* `check_arguments` verifies the arguments are an object, that
required keys are present and non null, and that declared **scalar** types
match. Objects, arrays, unions and untyped properties pass through; its own doc
says "This is not validation on the plugin's behalf". The model's arguments are
stored verbatim in the proposal and replayed verbatim on apply, so what stops a
bad write is the human reading the proposal and the plugin's own validation.

**The finding on this surface.** *Apply is not independently authorised.*
`resolve_proposal` (`routes/assistant.rs:846-944`) checks CSRF, authentication,
conversation ownership, proposal ownership, status still proposed, and scope
still registered. It does **not** call `may_open`, and does not check
`config.enabled` or `config.scope_enabled`, all three of which are enforced on
`post_message` (`:620`). So a user whose permission was revoked after a proposal
was created, or a user whose scope or whole assistant an operator has since
disabled, can still POST the apply URL and have the write dispatched. It is not
a cross user hole, because ownership is checked, and it is not prompt
injectable, because the model cannot reach apply. It is a revocation and kill
switch gap. The tool does run with the caller's real user context, so a plugin
that checks permissions itself will refuse, but that defence is per plugin and
optional rather than a kernel invariant.

**Tests.** `tests/ai_assistant_test.rs` is thorough on the intended design:
`a_write_tool_proposes_and_changes_nothing`,
`applying_a_proposal_is_what_makes_the_change`,
`somebody_elses_proposal_is_a_404`,
`a_bad_call_becomes_a_failed_result_and_the_loop_carries_on` (the "no such
tool" refusal), `the_invalid_scope_is_dropped_and_the_rejection_names_the_reason`,
`a_user_without_the_permission_is_refused_and_an_admin_is_not`,
`the_tool_call_limit_stops_the_loop_and_says_so`,
`a_denying_budget_refuses_the_turn_before_the_provider_is_called`.

**No test found for:** applying a proposal after the permission was revoked;
applying after the scope or the assistant was disabled; `/api/v1/ai/assist`
refusing a user who lacks `use ai chat`.

---

## 2. History

### 2.1 Advisories taken

Public history begins at the 0.99.0 squash `2ff3a62` (2026-08-16); anything
earlier is private development and has no public commit.

| Date | Commit | What moved | Advisories |
|---|---|---|---|
| 2026-08-16 | `b3039b7` | Triage only: suppressed as unreachable (the kernel builds exactly one `Engine`) | RUSTSEC-2026-0222 |
| 2026-08-16 | `900dcfc` | **wasmtime 43.0.0 to 47.0.3**, cranelift 0.130.0 to 0.134.3. 14 suppressions removed from `.cargo/audit.toml`. No source change | RUSTSEC-2026-0085 through -0096, -0114, -0222 |
| 2026-08-18 | `0d6146b` | h2 0.4.13 to 0.4.16, lockfile only. **No CHANGELOG entry exists for this one** | RUSTSEC-2026-0258 |
| 2026-09-01 | `a3357b5` | **wasmtime 47.0.3 to 47.0.4**, lockfile only, inside the existing `"47"` range | RUSTSEC-2026-0269 (8.8 high, filesystem sandbox escape via trailing slashes on paths and symlinks); RUSTSEC-2026-0268 (6.9 medium, guest driven host heap allocation through WASIp3 streams) |
| 2026-09-18 | `e2f6225` | rustls 0.23.37 to 0.23.45, rustls-webpki 0.103.13 to 0.103.15, lockfile only | RUSTSEC-2026-0285 (TLS 1.3 handshake messages accepted across encryption level boundaries) |

No CVE ids appear anywhere in the tree; only RUSTSEC ids.

**Current suppressions** in `.cargo/audit.toml`, which is the only audit config
and has not been touched since 2026-08-16:

| Id | Issue | Justification | Review date |
|---|---|---|---|
| RUSTSEC-2023-0071 | rsa timing sidechannel via sqlx-mysql | PostgreSQL only; no upstream fix | 2026-06-01, **lapsed** |
| RUSTSEC-2026-0141 | lettre TLS hostname verification, Boring backend | Not applicable: built with rustls | 2026-07-01, **lapsed** |
| RUSTSEC-2026-0194 | quick-xml quadratic attribute check (DoS) | Reachable only through plist and syntect parsing bundled theme files at startup; plist pins `^0.38` | 2026-07-09, **lapsed**, comment still reads "PENDING JEREMY REVIEW" |
| RUSTSEC-2026-0195 | quick-xml unbounded namespace allocation (DoS) | as above | 2026-07-09, **lapsed** |
| RUSTSEC-2026-0189 | rmcp DNS rebinding, CVSS 8.8 | Not compiled: `trovato-mcp` enables only `transport-io` and serves over STDIO | 2026-11-01 |

The project's own summary is worth quoting, because it is the claim under
review: "every wasmtime and cranelift advisory is fixed rather than suppressed.
Nothing about the plugin sandbox is being carried on a justification."

### 2.2 Security relevant changelog entries

Newest first. Dates are commit dates, which are authoritative where a release
heading disagrees.

**v0.104.0** carries no entry in the classic categories. Four are sandbox
resource bound work a reviewer of section 1.1 will want: `142f8ab` (worker CPU
budget counts guest execution, not host call waiting), `7d030a3` (a job parked
in a never returning host call no longer holds the whole drain pass open),
`2c1696f` (a cancelled cron run no longer leaves an immortal heartbeat renewing
the global cron lock forever), `4b91c6a` (drain budget, lock renewal ceiling,
background epoch budget moved into `ResourceLimits`).

**v0.103.0**, the access control release:

| Date | Entry | Commit |
|---|---|---|
| 2026-09-20 | New permission `access administration pages` gates `/admin`; migration grants it once to roles holding `administer site` | `ed6732e` |
| 2026-09-20 | **BL-33: the kernel had two notions of administrator.** `UserContext::is_admin()` was derived from the permission string `administer site`, a forgeable authority; now reads the `users.is_admin` column | `7ef8c15` |
| 2026-09-19 | **The permission grid stopped deleting grants it does not render** (it had destroyed 29 plugin grants on a live 0.102.0 site); plus a guard that a non superuser may only grant a role whose permissions they already hold, and only a superuser may set `is_admin` | `9859d11` |
| 2026-09-19 | `tap_perm` dispatched at boot; plugin permissions become visible and grantable; a malformed declaration is dropped and cannot claim a kernel permission | `6420e5f` |
| 2026-09-19 | **111 admin handlers across 17 files moved from `require_admin` to `require_permission`** | `1a4e6fa` |
| 2026-09-18 | Security: rustls bump clearing RUSTSEC-2026-0285 | `e2f6225` |
| 2026-09-18 | Plugin `mail` rate limited on every path (new bucket, 100/hour per plugin, checked inside the host function) | `bd28992` |
| 2026-09-18 | Static assets get their own rate limit bucket; every bucket made configurable | `bfaee27` |
| 2026-09-18 | Passkey login script moved out of the inline `<script>` the enforcing CSP blocks; `login_page_csp_test` added | `73a9160` |
| 2026-09-18 | `markdown` filter: bare `ammonia::clean` replaced with a builder allowing `class` on `code`, `pre` and `span` only, values allowlisted | `7904e24` |
| 2026-09-18 | `sitemap.xml` and `robots.txt` emit absolute XML escaped addresses; a plugin claiming those paths is refused at startup | `6b5c9b0` |
| 2026-09-18 | Remaining MIME arms added, which matters because every static response carries `nosniff`; same commit hardens body field snippet redaction to deny on either field name | `82c2e12`, `c4dffc5` |

**v0.102.0**: `a3357b5` (the wasmtime 47.0.4 advisory bump), `a01a103` (AI
assistant: every model write is a proposal a person applies; a conversation 404s
to everyone but its owner, administrators included), `8cc8b9f` (the assistant
contract, scope validation, `use ai assistant`).

**v0.101.0**: `c56d8a9` (plugin CSRF: header or `_token` field, single use,
session bound, and deliberately **not** minted for bearer authenticated
callers), `a6b30a1` (the `mail` host interface takes no recipient parameter, to
avoid being a spam relay; refuses control characters in subject and attachment
content type, which is header injection and smuggled `Bcc`), `32a2a39` (themed
plugin pages: the kernel **still does not sanitize a plugin's HTML**, stated as
a contract, and the two theme taps are deliberately left undispatched because
`csrf_token` and `user_is_admin` sit in the render context).

**v0.100.0**, the largest security release: `900dcfc` (wasmtime 43 to 47.0.3),
`d2557b1` (**load the viewer's real permissions everywhere**: the item front
page had been access checked against a hard coded list so anonymous got none,
`admin_user_context` fabricated `vec!["administer site"]` across 11 handlers,
and a failed permission load now fails closed rather than degrading silently),
`c88680f` (account deletion revokes password, passkeys, tokens and every
session, with a re-auth step up under its own session key so a login ceremony
cannot be completed as a deletion step up), `737c853` (data export excludes
session tokens, credential material and IP addresses, with tested absences),
`d110284` (an unusable `CSP_REPORT_URI` used to leave the response with **no CSP
at all**), `3c98c74` (admin record view binds the id as a parameter),
`2b9f2f9` (RSS descriptions in CDATA with `]]>` split so content cannot close
the section), `7e0faa1` (menu paths must be local absolute paths: no scheme, no
protocol relative `//host`, no `..`), `4808465` (`trovato_scolta` removed: it
declared `host_interfaces = []` while calling `ai-api`, so the linker would have
refused it), `eeddac3` (an unparseable registration mode resolves to closed).

**v0.99.0** carries no security entry. Everything in the two `0.2.0-beta`
sections predates public history and has **no locatable commit**; that includes
the original authentication, OAuth2, security headers, SSRF prevention, file
upload validation and WASM sandboxing entries.

*One ledger error found while writing this brief:* `docs/BACKLOG.md:453` maps
PR #71, the rustls advisory bump, to commit `7904e24`. That commit is "Keep the
class that makes a Markdown code block highlightable (#87)". The rustls squash
is `e2f6225`. A reviewer following the ledger lands on the wrong change.

### 2.3 Friction log and site report findings that are security relevant

The Argus plugin friction logs live in `plugins/argus/M1-FRICTION.md` through
`M4-FRICTION.md`. The consolidated ledger is `docs/BACKLOG.md`. **The site
report is not in this repository**: it is `docs/REPORT.md` in the sibling
`jeremyandrews/trovato-site`, 22 findings against v0.101.0 restatused at
v0.102.0. A reviewer who needs it should ask for that repository.

| Id | Severity as stated | Finding | Status |
|---|---|---|---|
| G-QUERY-RAW-FIRST-KEYWORD (BL-61) | Medium, security | `is_read_only` checks only the first keyword, so a data modifying CTE passes the `query-raw` guard and writes. Argus depends on it | **open**, class 1.0.x. No explicit status line in M4 |
| G-AI-BASEURL-UNCHECKED (BL-57) | Medium, security | `validate_base_url` is not called on the `ai-request` path and `save_provider` does not call it; string only, misses bracketed IPv6 | **open**, class 1.0 |
| G-SSRF-NO-TEST-ALLOWANCE | High, residual | The fence blocks loopback, so no local stub provider or fixture feed server is reachable and success paths are untestable on one machine | **open / residual**, recommended post 1.0. Earlier filed as G-SSRF-LOCAL and "accepted as a documented 1.0 limitation" |
| G-VIEW-OUTPUT-JSON-ENCODED | High | A `String` returning view tap was JSON serialized and appended undecoded, so plugin markup landed on the page with stray quotes | **closed** by the batch that shipped in 0.99.0, but re-filed as residual by netgrasp |
| G-CSRF-NO-BEARER-BYPASS (BL-60) | decided | Whether a bearer authenticated plugin api write needs a CSRF token | **fixed**, `2ff3a62`, closed. Decided rather than deferred |
| G-ADMIN-UI-IS-ADMIN-ONLY (BL-41) | Medium | Every `/admin/content/...` route gated on `require_admin` while the JSON item routes checked real permissions | **fixed** (#92) `1a4e6fa`, closed |
| G-SDK-NO-ESCAPE (BL-34) | Low, raised by netgrasp | The SDK ships no HTML escaping helper, the kernel keeps `html_escape` private, and the kernel does not escape view output, so every rendering plugin writes its own. In netgrasp the escaped value is a **hostname from DHCP option 12**, i.e. whatever an unauthenticated device on the LAN claims | **open**, class 1.0.x |
| G-USER-API-NO-ADMIN-BYPASS (BL-33) | Medium | `current-user-has-permission` ignored `administer site`, so a plugin's own check disagreed with every kernel route | **fixed** (#96) `7ef8c15` |
| G-TWO-WRITER-NO-CONTRACT (BL-28) | Medium | The db allowlist is per table, so no manifest can say a column belongs to another writer, and `raw_sql` reaches any table the role can | **open**, class post |
| G-ITEM-API-BYPASSES-ITEM-SERVICE (BL-25) | High, residual | `save-item` and `delete-item` call the model directly, so a plugin's item write fires no taps and **runs no access check** | **partly fixed**, otherwise open |
| BL-105 | | A plugin reaches the kernel's `users` table by declaring it in `db_tables`, which no policy refuses | **open**, class post, explicitly placed in this review's scope |
| BL-117 | | The admin sidebar lists screens a delegated role gets 403 on | **open** |
| Site report #8 | | The login page's inline script is blocked by the kernel's own CSP, so passkey sign in does not work on a default install | fixed at `73a9160` after the report |
| Site report #1 | | Static assets rate limited as API calls at 100/min per IP; behind a proxy every visitor shares one bucket | addressed in part by `bfaee27` |

**Two caveats a reviewer must carry.** `docs/BACKLOG.md` is verified at
`d3f4cd7` (2026-09-17), which predates the whole 0.103.0 fix series and all of
0.104.0, so several rows it calls open are closed by commits in section 2.2.
Read its statuses as "as of 2026-09-17". And `M4-FRICTION.md` carries no status
note at all, unlike M1 through M3, so none of its findings has a stated status
in the document itself.

### 2.4 Known issues that are security relevant

From `KNOWN-ISSUES.md`, scoped to 0.104.0:

- Security findings from private development are not independently verified.
  **1.0 blocker** (BL-66). This is the reason for this review.
- Page builder components accept arbitrary inline CSS. Open (BL-67).
- CSP `style-src` keeps `'unsafe-inline'` for 305 inline `style=` attributes
  across 44 templates, plus 22 `<style>` blocks. Open, 1.0.
- `script-src` has no `'unsafe-inline'`, nonce or hash, and four stock templates
  still depend on inline `<script>`, plus five with inline `on*=` handlers, all
  blocked by the enforcing policy. **Two of the blocked handlers are the
  `confirm()` guard on a delete, so those deletes happen without
  confirmation.** Open (BL-08), 1.0.
- A plugin enabled while the server runs registers its permissions only on
  restart. Open.
- The two theme taps are declared and not dispatched, because `csrf_token`,
  `user_is_admin` and `content` are in the render context and it is undecided
  which keys a plugin may overwrite. Open by decision.
- The frozen plugin contract is enforced by policy, not tooling:
  `cargo-semver-checks` cannot fail a breaking change under 0.x rules.
- An old pre freeze manifest passes the version check: `api_version = "0.2"` is
  accepted at `(0, 104)`. "The check is a compatibility gate and not a
  provenance check." Open (BL-77).
- Migrations only move forward; recovery means restoring the database.

---

## 3. Threat model

Five attackers. For each, what they start with, and what they must not be able
to do. The middle column is the capability the reviewer should assume; the right
column is the claim to attack.

| Attacker | Starts with | Must not be able to |
|---|---|---|
| **Anonymous visitor** | Network access to the site. No account. The `anonymous` role's permissions, which on a default install are read access to published content | Read unpublished, staged or access restricted content, or any field a format or permission restricts. Enumerate users or items (ids are UUIDv7 by design, so enumeration should be infeasible). Cause a write of any kind without authenticating. Land script or CSS in another visitor's or an administrator's page. Reach an internal network address through any site feature. Lock a named user out of their account, or hold a user's session. Exhaust the site with a volume any single client can generate |
| **Authenticated reader** | A valid account and session, the `authenticated` role, no content permissions | Everything above. Read or modify another user's account, sessions, API tokens, passkeys or exported data. Escalate to any content or administrative permission. Have a request forged by a third party site perform a write as them. Keep access after their account is blocked, their password is changed or their session is revoked |
| **Content editor** | An account with content permissions, and access to the admin screens those permissions open | Everything above. Reach an administrative screen their permissions do not name, in particular the ones still gated on `is_admin`. Grant themselves or anyone else a permission or role beyond what they hold. Store markup or CSS that executes or redresses for a reader or an administrator, including through the page builder, a content field at `full_html`, a tile, or a search snippet. Move content between stages they were not granted |
| **Plugin author** | A `.wasm` and an `.info.toml` an operator has installed, so the manifest is attacker written, with whatever capabilities it declares | Read or write outside the tables its manifest declares, including through `raw_sql`. Execute DDL, which `execute-raw` is documented to refuse. Reach a host interface it did not declare. Reach a private, loopback or metadata network address through the `http` interface, by any spelling including an IPv6 literal. Exhaust host memory or CPU, or wedge the kernel. Invoke another plugin's non public functions. Declare a permission that collides with or escalates over a kernel one. Act with a principal other than the real caller, in particular as a background principal without `ai_background`. Cause the kernel to emit its markup unescaped into a page |
| **Delegated administrator** | A role carrying named administrative permissions, but **not** the `users.is_admin` column | Everything above. Set `is_admin` on themselves or anyone else. Grant a role carrying permissions they do not themselves hold. Reach the nine `is_admin` gated routes. Read another user's exported data. Retain effective access after their permissions are revoked, including by applying an AI assistant proposal created while they still held them |

Two attackers deliberately out of scope. A **full superuser** acting against
their own site is not a threat model, since the flag grants everything by
construction. Anyone with **write access to the config directory or the
database** is likewise out of scope: config import has no permission gate, is
filesystem trust, and can rewrite arbitrary `site_config` keys by design.
Findings that require either are documentation gaps rather than
vulnerabilities, and should be filed as such.

---

## 4. The ask

### 4.1 Priority order

Work top down. If the budget runs out, it runs out at the bottom.

**P1. The plugin sandbox boundary.** This is the load bearing claim of the whole
architecture: an operator installs a third party `.wasm` and the kernel holds.
Confirm or refute the nested block comment bypass of the DDL guard in section
1.1, which is the single highest value question in this review and needs one
query against a live Postgres. Then the rest of the raw SQL surface: the `WITH`
acceptance, the shape of `DDL_KEYWORDS` as a denylist, and whether the
allowlist's lack of a prefix rule (BL-105) is exploitable in practice. Then the
resource bounds, including whether the host call credit in the epoch deadline
can be turned into an unbounded plugin lifetime.

**P2. Authentication and authorisation.** The bearer and CSRF interaction, in
particular the API token to cookie session conversion in section 1.3 and
whether it is reachable in a realistic deployment. The nine `is_admin` gated
routes and whether the mixed model leaves a reachable escalation. Account
lockout as a denial of service against a named user. Whether a blocked user's
existing sessions and tokens keep working.

**P3. SSRF.** The bracketed IPv6 bypass of the plugin fence in section 1.2,
which is the finding with the widest blast radius after P1, since it applies to
every plugin holding the `http` capability rather than only to an administrator
configured URL. Then the chat completion path and BL-57, and whether the two
fences should be one.

**P4. Cross site scripting and CSP.** The 54 `| safe` sites, prioritising the
search snippet and the tile body, both of which rest on unasserted invariants.
The page builder's unfiltered `style` attribute, and whether
`'unsafe-inline'` in `style-src` plus that attribute is exploitable end to end
rather than only in principle.

**P5. The AI assistant.** The apply path's missing permission and kill switch
checks in section 1.10. Whether the describe and execute split holds under an
adversarial model beyond the tool name check, which is sound.

**P6. Rate limiting and abuse.** Fail open on Redis failure, the fixed window
double spend, and the two buckets with no caller.

Two standing requests that cut across all six. First, **say when a stated
invariant is wrong rather than only when code is wrong**: several documents in
this repository claim more than the code does, and section 5 lists the ones
already found. Second, **say when a finding is already recorded**. This brief
names the BL numbers it knows about; re-reporting one is not a waste, but
marking it as prior art helps the triage.

### 4.2 What "done" means

A written report at `docs/security/REVIEW-<date>.md`, filed beside this brief,
where `<date>` is the date the report is delivered in `YYYY-MM-DD` form. So for
a report delivered on 15 October 2026: `docs/security/REVIEW-2026-10-15.md`.

It must contain a findings table with exactly these five columns:

| id | severity | surface | description | status |
|---|---|---|---|---|
| F-01 | high | sandbox | Nested block comment defeats the DDL guard on `execute-raw` | `fixed 4a1b2c3` |
| F-02 | medium | ssrf | Bracketed IPv6 literal bypasses the plugin fence | `accepted: no dual stack deployment before 1.0, tracked as BL-nnn` |

**Every row's status column must read either `fixed <commit>` or `accepted:
<reason>`.** Not "open", not "wontfix", not blank. A finding that is not fixed
is a decision someone made, and the reason is the useful part. The commit in a
`fixed` status is the squash commit on `main` that closes it.

The shape is not decoration. It is meant to be machine readable so a release
can gate on it, and the reviewer should treat the column set as fixed even
though, as section 5 of this brief records, no preflight greps it yet.

Beyond the table, the report should carry: the scope actually covered against
the priority list above, what was **not** looked at, the method (what was read,
what was executed, what was fuzzed if anything), and for each finding enough
reproduction detail that someone can confirm it without the reviewer present.
Findings that were investigated and found not to be real are worth a short
section of their own, because the next reviewer will otherwise investigate them
again.

---

## 5. Running it locally

Verified against the repository at `6177ba6`. **Where `INSTALL.md` and the code
disagree, this section follows the code, and says so.** Four documentation bugs
would otherwise cost a reviewer their first day.

### 5.1 Prerequisites

```bash
# Do not pick a Rust version. rustup reads rust-toolchain.toml and installs
# 1.96.0 plus rustfmt, clippy and the wasm32-wasip1 target automatically.
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cd trovato && rustc --version    # must print 1.96.0
```

`INSTALL.md` says "1.85+ recommended". That is stale; `rust-toolchain.toml`
pins 1.96.0 and rustup honours the file, so the pin wins in practice.

Also needed: PostgreSQL 15 or later (compose uses `pgvector/pgvector:pg17`,
which is what enables semantic search), Redis 7 or later, and, only if you
exercise site search indexing, the `pagefind` v1.3.0 CLI on `PATH`. Pagefind is
a real runtime dependency documented only in the `Dockerfile`, not in
`INSTALL.md`. No Node or npm anywhere.

### 5.2 Fastest path to a running server

Docker, prebuilt image, no Rust toolchain needed, serves on port 3001:

```bash
git clone https://github.com/jeremyandrews/trovato
cd trovato
cp .env.example .env
docker compose --profile full up -d
# open http://localhost:3001, which redirects to /install
```

Plain `docker compose up -d` with no profile starts only Postgres and Redis,
which is what the native path wants:

```bash
cp .env.example .env
docker compose up -d          # Postgres + Redis only
docker compose ps             # both must read healthy first
cargo run --release --bin trovato
# open http://localhost:3000, which redirects to /install
```

No separate migration step: the server migrates on startup. First run is a four
step web install wizard (welcome and requirements, admin account, site config,
complete), and it does not run again once installed.

**The installer's password minimum is 12, not the 8 `INSTALL.md` states.**

Environment variables all live in `.env.example`, loaded by `dotenvy`. The ones
that matter: `PORT`, `DATABASE_URL`, `REDIS_URL`, `SITE_URL`, `PLUGINS_DIR`,
`CRON_KEY`, `JWT_SECRET` (required by the OAuth2 plugin, minimum 32 bytes),
`TRUSTED_PROXIES` (empty by default, and section 1.9 explains why that matters),
and `TROVATO_RATE_LIMIT_<BUCKET>`.

### 5.3 Building the plugins

**`INSTALL.md:133` says plugin WASM modules ship precompiled and do not need
building. That is false for a git clone**: `*.wasm` is gitignored. A reviewer
following `INSTALL.md` verbatim gets a server with zero plugins and no error
explaining why. The prebuilt container image does carry them; the clone does
not.

The loader expects `PLUGINS_DIR/<dir>/<name>.wasm` where `<name>` matches the
`.info.toml` in that directory. There is **no single documented command** that
builds and installs them all for a native run; the following is assembled from
the `Dockerfile`'s own list and copy step:

```bash
cargo build --target wasm32-wasip1 --release \
  -p trovato_blog -p trovato_media -p trovato_redirects \
  -p trovato_audit_log -p trovato_scheduled_publishing \
  -p trovato_content_locking -p trovato_webhooks \
  -p trovato_image_styles -p trovato_oauth2 \
  -p trovato_categories -p trovato_comments \
  -p trovato_locale -p trovato_content_translation \
  -p trovato_config_translation -p trovato_block_editor \
  -p trovato_search -p trovato_ai -p trovato_seo \
  -p trovato_page_builder -p trovato_captcha \
  -p trovato_series -p trovato_spam \
  -p trovato_book -p trovato_contact \
  -p argus -p goose

for w in target/wasm32-wasip1/release/*.wasm; do
  n=$(basename "$w" .wasm); mkdir -p "plugins/$n"; cp "$w" "plugins/$n/";
done
```

Every plugin found under `plugins/` is auto installed and enabled on first run,
so excluding one means moving its directory out.

### 5.4 Running the tests

```bash
# Unit and doc tests: no database, no Redis, no plugins.
cargo test --all --lib
cargo test --all --doc

# The scripted local gate.
./scripts/pre-commit-check.sh

# Integration tests: need Postgres, Redis, and the plugin wasm built above.
cargo test --all
```

A local `cargo test --all` is a **stronger** gate than CI, which shards across
three separate databases, so two tests that contend for one fixture can land in
different shards and never meet.

**Gotcha one: some integration tests only pass on a virgin database.** A few use
fixed fixture names and assert exact row counts without cleaning up, so they
pass the first time and fail the second. `CONTRIBUTING.md` states the rule and
cites `KNOWN-ISSUES.md`, which does not actually contain it. Use a throwaway
database per full run:

```bash
createdb trovato_review_$(date +%s)
export DATABASE_URL=postgres://trovato:trovato@localhost:5432/trovato_review_<stamp>
cargo test --all
```

**Gotcha two: the suite shares one rate limit bucket.** Any test request without
an `X-Forwarded-For` header lands in the shared `127.0.0.1` bucket, 100 requests
a minute for the whole parallel suite. The limiter answers 429, but the failure
usually surfaces several frames away as a confusing 403, because a page that
answered 429 yields no CSRF token. It is load dependent and often does not
reproduce. Give each test file its own client IP, as
`login_page_test.rs` and three others already do, or serialize with
`cargo test --all -- --test-threads=1`.

### 5.5 Exercising the interesting surfaces

**Admin user:** only through the web installer. There is no CLI to create a
user; the `user` verb offers only `reset-password`, `role-add`, `role-remove`
and `roles`.

**API token:** log in in a browser, then `POST /api/tokens` from that session
with the cookie and an `X-CSRF-Token` header. The raw token is returned once.
Use it as `Authorization: Bearer <token>`. The documentation never shows how to
obtain a CSRF token for a raw curl workflow; the tests scrape it out of a
rendered form, and a reviewer scripting this will have to do the same.

**Enable a plugin:** `trovato plugin install <name>`, or `/admin/plugins` in the
UI. A plugin enabled while the server runs registers its permissions only on
restart, so restart after enabling.

**AI assistant:** this one has no offline mode, and the gap is worth
understanding because it is itself a finding. A real provider key is required,
supplied by **environment variable name**: the admin form stores the variable's
name, never the secret, and the server reads it from its own environment. The
only in tree stand in is `scripted_provider`, a hidden module that serves canned
responses on an ephemeral loopback port, and it **cannot be configured through
the admin UI**, because the provider form refuses a loopback URL as SSRF
prevention while the chat paths do not re-validate. The tests therefore write
the `site_config.ai_providers` row directly. That test comment is corroboration
of the finding in section 1.2, and a reviewer wanting an offline AI path must
either use a real key or copy `crates/kernel/tests/ai_assistant_test.rs`.

---

## 6. Corrections this brief makes to the repository's own documentation

Collected so they are not lost, and because several would mislead a reviewer.
None was fixed in the pass that wrote this brief.

1. `docs/admin-permissions.md:160` says exactly two routes keep `require_admin`.
   The code has nine handlers plus one inline check. The file advertises itself
   as the complete list of what the routes check.
2. `docs/alignment/intentional-divergences.md` §1 says the sandbox has "No
   network access". `crates/kernel/src/host/http.rs` gives plugins outbound HTTP
   behind a declared capability.
3. The same document's §7 says plugins "can't inject SQL because the WASM
   boundary only accepts structured operations", in the same paragraph that
   concedes `raw_sql`.
4. `docs/BACKLOG.md:453` maps PR #71 to commit `7904e24`. The rustls advisory
   bump is `e2f6225`; `7904e24` is an unrelated markdown change.
5. `INSTALL.md:133` says plugin WASM ships precompiled. It is gitignored.
6. `INSTALL.md` says the installer password minimum is 8. The code enforces 12.
7. `CONTRIBUTING.md:76` cites `KNOWN-ISSUES.md` for the fresh database rule.
   That entry is not there.
8. `validate_base_url`'s doc comment says "Host must not resolve to a private,
   loopback, or link-local IP range". The function never resolves anything.
9. `crates/kernel/src/middleware/rate_limit.rs:3` says sliding window. It is a
   fixed window.
10. `crates/plugin-sdk/src/host.rs:208` says "The kernel only allows SELECT and
    WITH statements", which is true and is exactly the problem: `WITH` covers
    data modifying CTEs.
11. `plugins/trovato_webhooks/src/lib.rs` carries a module comment describing
    "event driven webhook dispatch with HMAC-SHA256 signatures and exponential
    backoff retry". The file is 44 lines of a permission and a menu entry, with
    no dispatch, no HMAC and no HTTP. A reviewer reading the plugin list would
    assume a webhook egress surface exists.
12. `docs/RELEASING.md` has no step that reads a security review file, so the
    findings table shape in section 4.2 is a convention this brief establishes
    rather than one anything currently enforces.
