# Appendix: Deferred Issues

Issues identified during implementation that are out of scope for the current phase but need resolution.

## User Management

### Initial Admin User Setup

**Issue:** The design specifies that `is_admin` is "set during initial installation" (Design-Web-Layer.md, line 321) but does not specify the mechanism for creating the first admin user.

**Current State:** The users migration creates only the anonymous user. No admin user exists by default.

**Security Constraint:** Default admin credentials are unacceptable. Any hardcoded username/password becomes a hack vector - attackers actively scan for default credentials. The solution MUST require credentials to be set at install time.

**Options to Consider:**
1. **Install wizard** - First-run web UI prompts for admin credentials (preferred for self-hosted)
2. **CLI subcommand** - `trovato user create --admin` prompts for password interactively
3. **Environment variables** - `TROVATO_ADMIN_USER` / `TROVATO_ADMIN_PASS` for containerized deployments

**NOT acceptable:**
- Hardcoded defaults in migrations or code
- Well-known default passwords (admin/admin, etc.)
- Optional password setup (must be mandatory)

**Workaround (Development Only):** Insert admin user directly via SQL with a password you generate:
```sql
INSERT INTO users (id, name, pass, mail, is_admin, status)
VALUES (
    gen_random_uuid(),
    'admin',
    '$argon2id$v=19$m=19456,t=2,p=1$...',  -- YOUR hashed password
    'admin@example.com',
    TRUE,
    1
);
```

**Phase:** Resolved — web installer implemented in Phase 1.

---

## Admin UI Completeness

### Missing Admin Pages

**Issue:** The current admin UI only covers content type management (`/admin/structure/types`). Many system endpoints lack UI access.

**Current State (Phase 6 — largely resolved):**
- `/admin` — Dashboard with content, user, and system sections
- `/admin/structure/types` — Content type CRUD
- `/admin/structure/types/{type}/fields` — Field management
- `/admin/people` — User management (list, create, edit)
- `/admin/structure/categories` — Category/tag management
- `/admin/content` — Content listing with filters
- `/admin/structure/gather` — Gather query management
- `/admin/structure/tiles` — Tile layout management
- `/admin/config/ai` — AI provider configuration
- `/admin/plugins` — Plugin enable/disable
- `/admin/structure/aliases` — URL alias management
- `/admin/structure/menus` — Menu link management
- `/admin/reports/ai-usage` — AI usage dashboard

**Still missing:**
- Role and permission management UI (permissions configured via config import)
- Stage management UI (stages configured via config import)
- System configuration UI (site settings via config import or direct SQL)

**Phase:** Mostly resolved. Remaining gaps are configuration UIs that use config import.

---

## Inclusivity-First Deferred Items

### Cross-User Session Invalidation

**Issue:** When an admin changes another user's role or admin status, that user's active session should be invalidated. Currently only self-session rotation is implemented.

**Requires:** A user→session index in Redis (mapping user IDs to their session keys). The current session store does not maintain this index.

**Phase:** Deferred to when a multi-user admin workflow demands it.

### Full `tap_field_access` Integration

**Issue:** The `tap_field_access` tap type and `FieldAccessResult` SDK type exist, and `check_field_access()` is implemented on `ItemService`, but the dispatch to WASM plugins and full integration into Gather field exclusion, form field removal, and template field hiding is not yet wired.

**Phase:** Deferred until a plugin implements `tap_field_access`.

### Inline CSS Extraction from `base.html`

**Issue:** The CSP policy allows `style-src 'unsafe-inline'` because `base.html` has a large inline `<style>` block. Extracting to a static CSS file would allow removing `'unsafe-inline'` from the CSP.

**Phase:** Deferred as tech debt. Low security risk — inline styles are not an XSS vector.

### Full `tap_ai_request` Dispatch Integration

**Issue:** `AiRequestContext` and `AiRequestDecision` SDK types exist, but the `ai_request()` host function does not yet dispatch `tap_ai_request` before sending to the provider.

**Phase:** Deferred until an AI governance plugin is implemented.

### Per-AI-Feature Configuration

**Issue:** The `ai_features` config section and admin UI for per-operation enable/provider/model settings is planned (Story 47.6) but not yet implemented.

**Phase:** Deferred until multiple AI providers are in active use.

---

## Future Issues

(Add new deferred issues here as they are identified)
