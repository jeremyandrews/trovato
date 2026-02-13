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

**Phase:** Deferred (not blocking current gates)

---

## Admin UI Completeness

### Missing Admin Pages

**Issue:** The current admin UI only covers content type management (`/admin/structure/types`). Many system endpoints lack UI access.

**Current State (Phase 5):**
- `/admin` - Dashboard (basic)
- `/admin/structure/types` - Content type CRUD
- `/admin/structure/types/{type}/fields` - Field management

**Missing UI for:**
- User management (list, create, edit, delete users)
- Role and permission management
- Category/tag management
- Content listing and editing
- Gather view configuration
- Stage management
- System configuration
- Menu management

**Constraint:** UI does not need to be polished, but all system functionality should be accessible through the admin interface. API-only endpoints are acceptable for automation but admins need UI access for manual operations.

**Phase:** Deferred (enumerate specific pages when implementing)

---

## Future Issues

(Add new deferred issues here as they are identified)
