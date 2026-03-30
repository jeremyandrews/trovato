# Story 45.4: Revision Integrity Verification

Status: ready-for-dev

## Story

As a **kernel maintainer**,
I want verified guarantees about revision behavior,
so that the content versioning system has documented, tested invariants that prevent data loss or audit trail corruption.

## Acceptance Criteria

1. Verified: restore-to-version creates a new revision (does not destructively overwrite or delete existing revisions)
2. Verified: no code path allows deleting individual revisions (only CASCADE from item deletion)
3. Verified: revision `created` timestamp is set by the kernel at INSERT time (not user-supplied)
4. Verified: revision `author_id` is set to the authenticated user (not user-supplied or overridable by plugins)
5. Any violations found are fixed as part of this story
6. Guarantees documented in `docs/Design-Content-Model.md` under a "Revision Invariants" section
7. At least 2 integration tests verifying the above invariants

## Tasks / Subtasks

- [ ] Audit restore-to-version code path: verify it creates a new revision with the restored content (AC: #1)
- [ ] Audit all code paths that interact with `item_revision` table: verify no path deletes individual revisions (AC: #2)
- [ ] Audit revision creation: verify `created` timestamp is set by kernel (e.g., `Utc::now()`) not from user input (AC: #3)
- [ ] Audit revision creation: verify `author_id` comes from authenticated session, not from request body or plugin context (AC: #4)
- [ ] Fix any violations found during audit (AC: #5)
- [ ] Document revision invariants in `docs/Design-Content-Model.md` (AC: #6)
- [ ] Write integration test: restore a previous version, verify a new revision is created and old revisions remain (AC: #7)
- [ ] Write integration test: verify revision `created` and `author_id` are kernel-controlled (AC: #7)

## Dev Notes

### Architecture

This is primarily a verification and documentation story. The expected invariants are:

1. **Restore = new revision**: When a user restores to version N, the kernel reads version N's fields and creates version N+1 with those fields. Versions 1..N remain intact. This is append-only semantics.

2. **No selective deletion**: The only way revisions are deleted is via CASCADE when the parent item is deleted. There should be no API endpoint, admin action, or service method that deletes a specific revision by ID.

3. **Kernel-controlled timestamps**: The `created` field on `item_revision` should be set in the Rust save path using `chrono::Utc::now().timestamp()`, not passed through from the request.

4. **Kernel-controlled authorship**: The `author_id` field should be read from the authenticated session (`UserContext`), not from form data or plugin-supplied values.

### Testing

- **Restore test**: Create an item (revision 1), update it (revision 2), restore to revision 1. Verify: revision 1 and 2 still exist, revision 3 exists with revision 1's field values.
- **Timestamp test**: Create a revision, read it back, verify `created` is within a few seconds of the current time (not zero, not a user-supplied value).
- **Author test**: Create an item as user A, verify the revision's `author_id` matches user A's ID. Attempt to supply a different author_id in the request — verify it is ignored.

### References

- `crates/kernel/src/content/` — item save and restore paths
- `docs/Design-Content-Model.md` — content model documentation
- `item_revision` table schema
