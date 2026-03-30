# Story 42.4: Field-Level Access Control Tap

Status: ready-for-dev

## Story

As a plugin developer implementing role-based field visibility,
I want a `tap_field_access` hook,
so that I can control which fields are visible or editable based on the user's roles and context.

## Acceptance Criteria

1. New `tap_field_access` tap added to the tap registry
2. Tap signature: `(operation: view|edit, item_type: &str, field_name: &str, user_context: &UserContext) -> FieldAccessResult`
3. `FieldAccessResult` enum with variants `Allow`, `Deny`, `NoOpinion` added to `crates/plugin-sdk/src/types.rs`
4. Deny-wins aggregation: if any plugin returns `Deny`, access is denied regardless of other results
5. `NoOpinion` is the default -- fields are accessible unless explicitly denied
6. Results cached per `(role_set, item_type, field_name, operation)` tuple with Moka cache, 5-minute TTL
7. Cache invalidated on role or permission changes
8. Gather queries exclude denied fields from SELECT projections
9. Item display respects denied fields -- they are not rendered in templates
10. Edit forms exclude denied fields entirely (absent from HTML, not just hidden)
11. Admin users bypass field access checks (all fields accessible)
12. Cached lookup performance under 1ms per field
13. At least 2 integration tests covering view denial and edit denial

## Tasks / Subtasks

- [ ] Add `FieldAccessResult` enum to `crates/plugin-sdk/src/types.rs` (AC: #3)
- [ ] Register `tap_field_access` in the tap registry (AC: #1)
- [ ] Implement tap dispatch with deny-wins aggregation logic (AC: #2, #4, #5)
- [ ] Add Moka cache keyed by `(role_set, item_type, field_name, operation)` with 5-minute TTL (AC: #6)
- [ ] Add cache invalidation on role/permission change events (AC: #7)
- [ ] Modify Gather query builder to exclude denied fields from SELECT (AC: #8)
- [ ] Modify item display rendering to skip denied fields (AC: #9)
- [ ] Modify edit form builder to omit denied fields from HTML (AC: #10)
- [ ] Add admin bypass check before tap dispatch (AC: #11)
- [ ] Write integration test: field denied for view is not rendered (AC: #9, #13)
- [ ] Write integration test: field denied for edit is absent from form HTML (AC: #10, #13)
- [ ] Write integration test: admin user sees all fields despite deny (AC: #11)
- [ ] Benchmark cached lookup to verify <1ms (AC: #12)
- [ ] Create `docs/design/Analysis-Field-Access-Security.md` design document

## Dev Notes

### Architecture

The tap runs in the content rendering pipeline and the Gather query builder. For Gather, denied fields are removed from the SELECT list before query execution -- this is both a security measure (data never leaves the DB) and a performance optimization. The cache key uses a sorted, hashed role set rather than the full role list to keep keys compact. The Moka cache is shared across the service and uses `(u64_role_hash, item_type, field_name, operation)` as a composite key.

### Security

- Deny-wins is critical: a single plugin denying access must override any number of Allow responses. This prevents privilege escalation through plugin composition.
- Edit form exclusion must remove the field entirely from the HTML, not just hide it with CSS/JS. A hidden field could still be submitted via browser dev tools.
- Admin bypass uses the existing `is_admin` check from `UserContext`, consistent with other admin overrides in the system.
- Gather field exclusion prevents data leakage through API responses, not just template rendering.

### Testing

- Test with a mock plugin that denies `field_secret` for view -- verify the field value does not appear in the rendered HTML.
- Test edit form with denied field -- verify the `<input>` element is completely absent, not `type="hidden"`.
- Test that admin user still sees and can edit the denied field.
- Performance test: populate cache, measure 1000 lookups, assert p99 < 1ms.

### References

- `crates/kernel/src/tap/` -- tap registry
- `crates/plugin-sdk/src/types.rs` -- SDK types
- `crates/kernel/src/content/` -- item rendering
- `crates/kernel/src/content/form.rs` -- edit form builder
- `crates/kernel/src/gather/` -- query builder
- `docs/ritrovo/epic-12-security.md` -- Epic 42 source
