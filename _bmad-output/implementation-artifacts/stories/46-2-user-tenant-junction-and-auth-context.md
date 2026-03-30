# Story 46.2: User-Tenant Junction and Auth Context

Status: ready-for-dev

## Story

As a **site operator managing multiple tenants**,
I want users to belong to one or more tenants with per-tenant roles,
so that user access can be scoped to specific tenants.

## Acceptance Criteria

1. Migration creates `user_tenant` junction table with columns: `user_id` (UUID FK to users.id), `tenant_id` (UUID FK to tenant.id), composite PK on (user_id, tenant_id), `is_active` (BOOLEAN DEFAULT TRUE), `created` (BIGINT)
2. Existing users are seeded with `DEFAULT_TENANT_ID` entries in `user_tenant`
3. Role assignments become tenant-scoped (role is per user-tenant pair, not global)
4. `UserContext` gains a `tenant_id: Uuid` field populated during authentication
5. Auth middleware authenticates the user globally, then tenant middleware resolves the tenant and verifies the user's membership in that tenant
6. Requests from a user not in the resolved tenant receive 403 Forbidden
7. Admin users (`is_admin = true`) can access all tenants without requiring junction entries
8. At least 2 integration tests: one verifying tenant membership enforcement, one verifying admin bypass

## Tasks / Subtasks

- [ ] Write migration SQL to create `user_tenant` junction table (AC: #1)
- [ ] Write migration SQL to seed existing users with `DEFAULT_TENANT_ID` entries (AC: #2)
- [ ] Modify role assignment schema/logic to be tenant-scoped (AC: #3)
- [ ] Add `tenant_id: Uuid` field to `UserContext` struct (AC: #4)
- [ ] Update auth middleware to populate `UserContext.tenant_id` after tenant resolution (AC: #5)
- [ ] Add tenant membership check: verify user exists in `user_tenant` for the resolved tenant (AC: #5, #6)
- [ ] Return 403 if non-admin user is not a member of the resolved tenant (AC: #6)
- [ ] Add admin bypass: skip membership check when `is_admin = true` (AC: #7)
- [ ] Write integration test for tenant membership enforcement (AC: #8)
- [ ] Write integration test for admin tenant bypass (AC: #8)

## Dev Notes

### Architecture

The auth chain becomes: session lookup -> user load (global) -> tenant resolution (from 46.3) -> membership check -> populate `UserContext` with `tenant_id`. For single-tenant deployments using the "default" resolution strategy, the membership check always succeeds against `DEFAULT_TENANT_ID`.

Role scoping means the `user_role` table (or equivalent) gains a `tenant_id` column. A user can be "editor" in tenant A and "viewer" in tenant B.

### Security

- Tenant membership check must happen on every request, not just login.
- Admin bypass is intentional: super-admins manage all tenants.
- The 403 response should not leak information about whether the tenant exists.

### Testing

- Create two tenants, assign a user to only one. Verify access to the assigned tenant succeeds and access to the other returns 403.
- Verify an admin user can access both tenants without junction entries.

### References

- `crates/kernel/src/models/user.rs` -- UserContext struct
- `crates/kernel/src/middleware/` -- auth middleware chain
- Story 46.3 for tenant resolution that this story depends on
