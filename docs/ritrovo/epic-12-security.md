# Epic 12 (C): Security Hardening

**Tutorial Parts Affected:** None directly (infrastructure changes are invisible to tutorial narrative)
**Trovato Phase Dependency:** Phase 6 (Rate Limiting, Hardening) — already complete
**BMAD Epic:** 42
**Status:** Not started
**Estimated Effort:** 3–4 weeks
**Dependencies:** Epic A (10) — form accessibility patterns inform field_access story
**Blocks:** None

---

## Narrative

*Trovato's security story is already strong. Parameterized queries everywhere. Template auto-escaping. Constant-time CSRF. SameSite=Strict. Argon2id. File upload MIME validation. Redis-backed rate limiting. The five-tier `tap_item_access` system with deny-wins semantics. These are not afterthoughts — they were day-one decisions.*

*What's missing are the HTTP-level headers that browsers need to enforce their own protections, and two infrastructure pieces that the Inclusivity-First research identified as kernel responsibilities: field-level access control and cryptographic primitives for plugins.*

The biggest security gap is the absence of Content-Security-Policy headers. CSP tells the browser which scripts, styles, and resources it may load — blocking XSS even when a sanitization layer is bypassed. Trovato can't ship CSP today because `base.html` includes inline JavaScript (the AJAX framework). This epic moves that JS to a static file, then enables strict CSP. The enforcement principle: **plugins cannot weaken the CSP.** A plugin can *request* that a specific CDN origin be added to the allowlist (via a tap), but cannot add `unsafe-inline` or `unsafe-eval`. The kernel merges plugin requests and rejects weakening directives.

The `field_access` tap fills the gap between item-level and field-level access control. Today, `tap_item_access` controls whether a user can see an item at all. But some fields within an item should be restricted — salary ranges visible only to HR, internal notes visible only to editors, PII visible only to the data owner. Drupal's `hook_field_access` solved this but was notoriously slow (O(fields x items x renders)). Trovato's version caches access decisions per role — field access is usually role-based, not per-request.

**Before this epic:** Strong foundations with gaps in HTTP headers and no field-level access control. CORS defaults to `*`. Plugins can't do cryptographic operations without bundling their own crypto libraries.

**After this epic:** Strict CSP. Full security header suite. Field-level access control. Plugin crypto primitives. CORS restricted. Session rotation on all privilege changes.

---

## Kernel Minimality Check

| Change | Why not a plugin? |
|---|---|
| CSP middleware | HTTP security headers are kernel infrastructure — plugins cannot inject middleware before the response |
| Security response headers (HSTS, X-Frame-Options, etc.) | Same — response headers are kernel-level |
| CORS configuration fix | Middleware configuration, not a feature |
| `field_access` tap | Access control is kernel infrastructure — plugins consume it, they don't define the mechanism |
| Max `per_page` in Gather | Query engine is kernel; this is a safety limit, not a feature |
| Session rotation on privilege changes | Session management is kernel |
| Crypto host functions | Host functions are kernel-provided plugin API |
| `SecretConfigProvider` | Config storage is kernel infrastructure |

Every item is infrastructure that plugins depend on or that enforces safety constraints plugins cannot override.

---

## BMAD Stories

### Story 42.1: Move Inline JS to Static File and Enable CSP

**As a** site operator concerned about XSS,
**I want** Content-Security-Policy headers on all responses,
**So that** browsers block unauthorized scripts even if a sanitization layer is bypassed.

**Acceptance criteria:**

- [ ] AJAX framework JavaScript moved from inline `<script>` in `base.html` to `static/js/trovato.js`
- [ ] `base.html` references the static file: `<script src="/static/js/trovato.js"></script>`
- [ ] CSP middleware added to the response pipeline, setting `Content-Security-Policy` header
- [ ] Default CSP: `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'`
- [ ] `style-src 'unsafe-inline'` permitted initially (inline `<style>` in base.html) — tracked as tech debt for future extraction to static CSS file
- [ ] `tap_csp_alter` hook allows plugins to request additional CSP source origins (e.g., `img-src cdn.example.com`)
- [ ] `tap_csp_alter` rejects weakening directives: `unsafe-inline` for `script-src`, `unsafe-eval`, `*` wildcard, `data:` for `script-src`
- [ ] CSP configurable via `CSP_REPORT_ONLY=true` env var for initial deployment (report-only mode)
- [ ] `CSP_REPORT_URI` env var for violation reporting endpoint
- [ ] No functional regression in AJAX behavior (forms, add-another, block editor all work with external JS file)

**Implementation notes:**
- Extract JS from `templates/base.html` to `static/js/trovato.js`
- Add CSP middleware in `crates/kernel/src/middleware/` — runs after route handlers, before response
- The tile service inline JS TODO (`services/tile.rs:140`) must also be resolved — move tile JS to static file
- `tap_csp_alter` is a new tap type — add to tap registry
- CSP nonce approach (per-request nonce for inline scripts) is an alternative to external file — but external file is simpler and more cacheable

---

### Story 42.2: Security Response Headers

**As a** site operator,
**I want** standard security headers on all HTTP responses,
**So that** browsers enforce clickjacking protection, HTTPS, and MIME type sniffing prevention.

**Acceptance criteria:**

- [ ] `X-Frame-Options: DENY` on all responses (prevents clickjacking)
- [ ] `Strict-Transport-Security: max-age=31536000; includeSubDomains` on all HTTPS responses
- [ ] HSTS only sent when request is HTTPS (check `X-Forwarded-Proto` or connection scheme)
- [ ] `X-Content-Type-Options: nosniff` on all responses (currently only on file downloads — extend globally)
- [ ] `Referrer-Policy: strict-origin-when-cross-origin` on all responses
- [ ] `Permissions-Policy: camera=(), microphone=(), geolocation=()` on all responses (disable browser APIs by default)
- [ ] Headers configurable via environment variables: `SECURITY_HSTS_MAX_AGE`, `SECURITY_FRAME_OPTIONS` (DENY or SAMEORIGIN), `SECURITY_REFERRER_POLICY`
- [ ] Headers applied via a single middleware layer, not scattered across route handlers
- [ ] Existing `X-Content-Type-Options` on file downloads still present (middleware sets it globally; file route's explicit header is now redundant but harmless)

**Implementation notes:**
- Add security headers middleware in `crates/kernel/src/middleware/security_headers.rs`
- Apply to the root router layer so all routes get headers
- Remove explicit `X-Content-Type-Options` from file route (now redundant) or leave it (idempotent)

---

### Story 42.3: CORS Configuration Fix

**As a** site operator,
**I want** CORS restricted to explicit origins by default,
**So that** cross-origin requests are only accepted from trusted domains.

**Acceptance criteria:**

- [ ] Default `CORS_ALLOWED_ORIGINS` changed from `"*"` to empty (no cross-origin requests allowed by default)
- [ ] When `CORS_ALLOWED_ORIGINS` is empty, CORS headers are not sent (browser blocks cross-origin requests)
- [ ] When `CORS_ALLOWED_ORIGINS` is explicitly set to `"*"`, CORS `Access-Control-Allow-Origin: *` is sent (opt-in permissive mode for development)
- [ ] CORS middleware sends `Vary: Origin` header when origin-specific (not `*`)
- [ ] Preflight `OPTIONS` requests handled correctly (Access-Control-Allow-Methods, Access-Control-Allow-Headers)
- [ ] `CORS_ALLOWED_METHODS` env var (default: `GET, POST, PUT, DELETE, OPTIONS`)
- [ ] `CORS_ALLOWED_HEADERS` env var (default: `Content-Type, Authorization, X-CSRF-Token`)
- [ ] Documentation updated in `.env.example` with CORS configuration examples

**Implementation notes:**
- Modify `crates/kernel/src/config.rs` — change default from `vec!["*".to_string()]` to `vec![]`
- Verify CORS middleware behavior when origins list is empty
- Update `.env.example` with commented CORS config

---

### Story 42.4: Field-Level Access Control Tap

**As a** plugin developer implementing role-based field visibility,
**I want** a `tap_field_access` hook,
**So that** I can control which fields are visible or editable per role without building my own access control layer.

**Acceptance criteria:**

- [ ] New `tap_field_access` tap added to the tap registry
- [ ] Tap signature: `tap_field_access(operation: "view" | "edit", item_type: &str, field_name: &str, user_context: &UserContext) -> FieldAccessResult` where `FieldAccessResult` is `Allow`, `Deny`, or `NoOpinion`
- [ ] Deny-wins aggregation across plugins (same pattern as `tap_item_access`)
- [ ] `NoOpinion` default — fields are accessible unless explicitly denied
- [ ] Results cached per `(role_set, item_type, field_name, operation)` tuple — cache invalidated when roles/permissions change
- [ ] Cache backed by Moka (same pattern as other per-key caches) with configurable TTL (default 5 minutes)
- [ ] Gather queries respect field access — denied fields excluded from SELECT clause (not filtered after fetch — must not send denied field data to the client at all)
- [ ] Item display respects field access — denied fields not rendered in templates
- [ ] Item edit forms respect field access — denied fields not included in form (not just hidden — absent from HTML entirely)
- [ ] Admin users bypass field access (same as item access bypass)
- [ ] Performance: field access check adds <1ms per item render for typical role configurations (cached path)
- [ ] At least 2 integration tests: one verifying deny, one verifying cache invalidation

**Implementation notes:**
- Add `tap_field_access` to `crates/kernel/src/tap/` registry
- Add `FieldAccessResult` type to `crates/plugin-sdk/src/types.rs`
- Modify `crates/kernel/src/content/` item rendering to filter fields before template context
- Modify `crates/kernel/src/content/form.rs` to filter fields before form building
- Modify `crates/kernel/src/gather/` query builder to exclude denied fields from SELECT
- The `Analysis-Field-Access-Security.md` design doc exists — follow its recommendations
- Performance critical: cache aggressively. Drupal's hook_field_access was slow because it ran per-field per-render with no caching.

---

### Story 42.5: Gather Max Page Size

**As a** kernel maintainer,
**I want** an upper bound on the `per_page` parameter in Gather queries,
**So that** a malicious or buggy request cannot fetch millions of rows.

**Acceptance criteria:**

- [ ] `GATHER_MAX_PAGE_SIZE` configuration (env var, default 100)
- [ ] Gather query execution clamps `per_page` to `min(requested, max_page_size)` — does not error, silently clamps
- [ ] If `per_page` is 0 or negative, defaults to the Gather definition's configured `items_per_page`
- [ ] API responses include `X-Max-Page-Size: 100` header (or configured value) so clients know the limit
- [ ] Admin UI pagination controls respect the max (no "show all" option that bypasses it)
- [ ] Existing Gather definitions with `items_per_page > max` still work — the definition's value is not clamped, only the runtime request parameter is

**Implementation notes:**
- Modify `crates/kernel/src/gather/` query execution — add clamp before LIMIT
- Add `GATHER_MAX_PAGE_SIZE` to `crates/kernel/src/config.rs`
- Simple change with outsized security benefit

---

### Story 42.6: Session Rotation on Privilege Escalation

**As a** security-conscious platform,
**I want** session tokens rotated whenever a user's privilege level changes,
**So that** session fixation attacks cannot exploit privilege escalation.

**Acceptance criteria:**

- [ ] `session.cycle_id()` called after: user login (already done ✅), user `is_admin` change, user role assignment change, user role removal
- [ ] Verify `cycle_id()` is called in `crates/kernel/src/routes/admin_user.rs` when `is_admin` is toggled
- [ ] Verify `cycle_id()` is called in role assignment routes (if the user being modified has an active session)
- [ ] For role changes to other users (admin modifying another user's roles): invalidate that user's session(s) rather than cycling the admin's session
- [ ] Session invalidation for other users uses Redis key deletion (the user's session key pattern)
- [ ] At least 2 integration tests: one for admin status change, one for role change

**Implementation notes:**
- `cycle_id()` is already called in `crates/kernel/src/routes/auth.rs` on login
- Add calls in `crates/kernel/src/routes/admin_user.rs` — in the handlers that modify `is_admin` and role assignments
- For modifying another user's session: look up their session in Redis and delete it, forcing re-authentication

---

### Story 42.7: Crypto Host Functions for Plugins

**As a** plugin developer implementing webhook HMAC signing or token generation,
**I want** cryptographic primitives available as host functions,
**So that** I don't need to bundle my own crypto library in WASM (which would be large, slow, and potentially insecure).

**Acceptance criteria:**

- [ ] `crypto_hmac_sha256(key: &[u8], message: &[u8]) -> Vec<u8>` host function — returns HMAC-SHA256 digest
- [ ] `crypto_sha256(data: &[u8]) -> Vec<u8>` host function — returns SHA-256 hash
- [ ] `crypto_random_bytes(len: u32) -> Vec<u8>` host function — returns cryptographically secure random bytes
- [ ] `crypto_constant_time_eq(a: &[u8], b: &[u8]) -> bool` host function — constant-time comparison for verification
- [ ] All functions documented in Plugin SDK with usage examples
- [ ] Functions use `ring` or `hmac`/`sha2` crate (already in dependency tree via Argon2 or session handling)
- [ ] WASM boundary handles byte arrays efficiently (avoid base64 encoding/decoding overhead where possible)
- [ ] At least 2 integration tests: HMAC sign-then-verify round-trip, random bytes uniqueness

**Implementation notes:**
- Add `crates/kernel/src/host/crypto.rs`
- Register host functions in the WASM host function registry
- These are building blocks — the webhook plugin uses `crypto_hmac_sha256` for payload signing, the OAuth2 plugin uses `crypto_random_bytes` for state parameters
- Keep the API minimal — add more primitives (AES, RSA) only when a plugin needs them

---

### Story 42.8: SecretConfigProvider for Sensitive Configuration

**As a** site operator deploying Trovato in production,
**I want** sensitive configuration (API keys, database passwords) storable outside the database,
**So that** secrets are not committed to git or stored in unencrypted database rows.

**Acceptance criteria:**

- [ ] `ConfigStorage` trait extended with a `SecretConfigProvider` variant
- [ ] `SecretConfigProvider` reads from environment variables (first implementation — simplest and most universal)
- [ ] Plugin config values prefixed with `env:` are resolved via `SecretConfigProvider`: `"api_key": "env:OPENAI_API_KEY"` → reads `OPENAI_API_KEY` from environment
- [ ] `env:` prefix resolution happens at config read time, not at storage time (the string `env:OPENAI_API_KEY` is what's stored)
- [ ] Admin UI config forms show `env:VARIABLE_NAME` as the stored value (not the resolved secret)
- [ ] Config export (`config export`) writes `env:VARIABLE_NAME` (not the resolved value) — safe to commit to git
- [ ] Config import (`config import`) accepts `env:` prefixed values
- [ ] At least one integration test: store `env:TEST_SECRET`, verify resolution, verify export doesn't leak
- [ ] Documentation: "Managing Secrets" section added to operational docs

**Implementation notes:**
- Modify `crates/kernel/src/config/` storage trait
- Resolution is a simple `if value.starts_with("env:")` check in the config read path
- This is deliberately simple — HashiCorp Vault or AWS Secrets Manager integration is a future `SecretConfigProvider` variant, using the same `env:` or `vault:` prefix convention
- The AI host function already reads API keys from env vars — this generalizes the pattern

---

## Plugin SDK Changes

| Change | File | Breaking? | Affected Plugins |
|---|---|---|---|
| `FieldAccessResult` type | `crates/plugin-sdk/src/types.rs` | No (new type) | None — plugins opt in by implementing `tap_field_access` |
| Crypto host function bindings | `crates/plugin-sdk/src/` | No (new functions) | None — plugins opt in by calling them |

**Migration guide:** No action required for existing plugins. New taps and host functions are opt-in.

---

## Design Doc Updates (shipped with this epic)

| Doc | Changes |
|---|---|
| `docs/design/Design-Web-Layer.md` | Add "Security Headers" section: CSP, HSTS, X-Frame-Options, CORS configuration. Document CSP enforcement principle (plugins cannot weaken). |
| `docs/design/Design-Plugin-SDK.md` | Add crypto host functions to host function reference. Add `tap_field_access` to tap reference. Document `FieldAccessResult` type. |
| `docs/design/Design-Query-Engine.md` | Add max page size documentation. Note field access filtering in SELECT clause. |
| `docs/design/Design-Infrastructure.md` | Add `SecretConfigProvider` to ConfigStorage section. Document `env:` prefix convention. |
| `docs/design/Analysis-Field-Access-Security.md` | Update with implementation notes from Story 42.4. Mark recommendations as implemented. |

---

## Tutorial Impact

| Tutorial Part | Sections Affected | Nature of Change |
|---|---|---|
| None | — | Security hardening is infrastructure — invisible to the tutorial narrative. No tutorial code blocks reference CSP headers, CORS config, or field access control directly. |

**Note:** If Part 4 (editorial engine) discusses access control, a brief mention of field-level access could be added, but this is optional — the tutorial focuses on item-level access which is already covered.

---

## Recipe Impact

None. Security infrastructure changes don't affect recipe command sequences.

---

## Screenshot Impact

None. Security headers are invisible in screenshots. Admin UI changes (if any from CSP-related JS extraction) are functionally identical.

---

## Config Fixture Impact

None. Security configuration is via environment variables, not YAML fixtures.

---

## Migration Notes

**Database migrations:** None. All changes are code-level (middleware, host functions, config).

**Breaking changes:**
- CORS default changes from `"*"` to empty. Sites that depend on cross-origin API access must explicitly set `CORS_ALLOWED_ORIGINS`. Add this to upgrade notes.
- CSP may break sites that load external scripts/styles without configuring them in `tap_csp_alter`. Deploy with `CSP_REPORT_ONLY=true` first.

**Upgrade path:**
1. Deploy with `CSP_REPORT_ONLY=true` and `CORS_ALLOWED_ORIGINS=*` (preserving current behavior)
2. Monitor CSP violation reports
3. Add legitimate sources to plugin `tap_csp_alter` implementations
4. Set `CORS_ALLOWED_ORIGINS` to actual allowed origins
5. Switch CSP to enforcement mode (`CSP_REPORT_ONLY=false` or remove the var)

---

## What's Deferred

- **Audit log plugin enhancements** — plugin territory. The kernel provides taps; the audit log plugin records what it wants.
- **Content locking enhancements** — plugin territory.
- **Rate limit configuration UI** — admin UI for rate limit tuning. Current config is env var only.
- **Web Application Firewall (WAF) rules** — plugin territory. Request inspection beyond rate limiting.
- **Two-factor authentication** — plugin territory. The kernel provides session management; 2FA is a login flow feature.
- **API key rotation UI** — plugin territory (OAuth2 plugin).
- **Security scanning/pentesting automation** — external tooling, not kernel.
- **Subresource Integrity (SRI)** — future enhancement for static file `<script>` and `<link>` tags. Requires hash generation at build time.

---

## Related

- [Design-Web-Layer.md](../design/Design-Web-Layer.md) — HTTP layer and middleware
- [Design-Plugin-SDK.md](../design/Design-Plugin-SDK.md) — Host functions and taps
- [Analysis-Field-Access-Security.md](../design/Analysis-Field-Access-Security.md) — Field access design analysis
- [Design-Query-Engine.md](../design/Design-Query-Engine.md) — Gather query builder
- [Epic A (10): Accessibility Foundation](epic-10-accessibility.md) — Form accessibility patterns referenced here
