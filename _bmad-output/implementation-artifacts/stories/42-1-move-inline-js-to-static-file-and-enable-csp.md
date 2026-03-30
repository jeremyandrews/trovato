# Story 42.1: Move Inline JS to Static File and Enable CSP

Status: ready-for-dev

## Story

As a site operator concerned about XSS,
I want Content Security Policy headers on all responses,
so that even if an attacker finds an injection point, inline script execution is blocked by the browser.

## Acceptance Criteria

1. AJAX framework JS moved from inline `<script>` in `base.html` to `static/js/trovato.js`
2. `base.html` references the static file via `<script src="/static/js/trovato.js"></script>`
3. CSP middleware added in `crates/kernel/src/middleware/csp.rs`
4. Default CSP policy: `default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'`
5. `style-src 'unsafe-inline'` tracked as tech debt (documented in code comment with TODO reference)
6. `tap_csp_alter` hook added for plugins to request CSP directive additions
7. `tap_csp_alter` rejects weakening directives: `unsafe-inline` for `script-src`, `unsafe-eval`, wildcard `*`, `data:` for `script-src`
8. `CSP_REPORT_ONLY` env var: when `true`, header sent as `Content-Security-Policy-Report-Only` instead of `Content-Security-Policy`
9. `CSP_REPORT_URI` env var: when set, appends `report-uri <value>` directive to the policy
10. No functional regression in AJAX operations (form submissions, dynamic content loading)

## Tasks / Subtasks

- [ ] Extract inline JS from `templates/base.html` into `static/js/trovato.js` (AC: #1, #2)
- [ ] Update `base.html` to reference the new static file (AC: #2)
- [ ] Create `crates/kernel/src/middleware/csp.rs` with CSP middleware layer (AC: #3, #4)
- [ ] Add `CSP_REPORT_ONLY` and `CSP_REPORT_URI` config fields to `config.rs` (AC: #8, #9)
- [ ] Register CSP middleware on the root router (AC: #3)
- [ ] Resolve tile inline JS TODO in `crates/kernel/src/services/tile.rs` (AC: #1)
- [ ] Add `tap_csp_alter` to the tap registry in `crates/kernel/src/tap/` (AC: #6)
- [ ] Implement directive-weakening rejection logic in the tap handler (AC: #7)
- [ ] Add tech-debt comment on `style-src 'unsafe-inline'` (AC: #5)
- [ ] Write integration tests verifying CSP header presence and correct policy string (AC: #3, #4)
- [ ] Write integration test verifying report-only mode (AC: #8)
- [ ] Write integration test verifying tap rejection of weakening directives (AC: #7)
- [ ] Manual smoke test: AJAX form submissions still work with CSP enforced (AC: #10)

## Dev Notes

### Architecture

The CSP middleware should run as an Axum layer applied to the root router, injecting the header into every response. The policy string is built at startup from config + tap contributions, so per-request overhead is minimal (just header insertion). The `tap_csp_alter` hook runs once at startup (or on plugin reload) to collect plugin CSP additions, not on every request.

### Security

- `unsafe-inline` for `style-src` is required short-term because Tera templates and admin UI use inline styles. Track removal as a separate story.
- The tap rejection list is a hard-coded denylist -- plugins cannot override it. This prevents a compromised plugin from weakening CSP.
- `frame-ancestors 'none'` prevents clickjacking (supplements X-Frame-Options from Story 42.2).

### Testing

- Integration tests should assert the `Content-Security-Policy` header value on HTML responses.
- Test that `CSP_REPORT_ONLY=true` switches to the report-only header name.
- Test that `tap_csp_alter` can add `font-src https://fonts.googleapis.com` but cannot add `script-src 'unsafe-inline'`.

### References

- `templates/base.html` -- current inline JS location
- `crates/kernel/src/services/tile.rs` -- tile inline JS TODO
- `crates/kernel/src/tap/` -- tap registry
- `docs/ritrovo/epic-12-security.md` -- Epic 42 source
