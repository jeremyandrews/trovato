# Story 45.3: Revision Immutability Enforcement

Status: ready-for-dev

## Story

As a **platform maintaining audit trail integrity**,
I want database-level enforcement preventing modification of existing revisions,
so that the revision history is a tamper-evident append-only log that cannot be silently altered.

## Acceptance Criteria

1. A PostgreSQL trigger on `item_revision` raises an exception on any UPDATE operation with the message "item_revision rows are immutable"
2. DELETE operations are still allowed (required for CASCADE when parent items are deleted)
3. INSERT operations are still allowed (normal revision creation)
4. The trigger is deployed via a database migration
5. At least 1 integration test that attempts an UPDATE on `item_revision` and verifies it fails with the expected error
6. Documentation note explaining how to temporarily disable the trigger for exceptional data-fix migrations

## Tasks / Subtasks

- [ ] Write migration with trigger function: `BEFORE UPDATE ON item_revision` raises exception (AC: #1)
- [ ] Verify trigger does not block DELETE operations (AC: #2)
- [ ] Verify trigger does not block INSERT operations (AC: #3)
- [ ] Write integration test: INSERT a revision (succeeds), attempt UPDATE (fails with expected message) (AC: #5)
- [ ] Write integration test: DELETE a revision (succeeds, confirming CASCADE still works) (AC: #5)
- [ ] Add documentation note about disabling trigger for data-fix migrations (AC: #6)

## Dependencies

**Migration ordering constraint:** This story's migration must have a timestamp AFTER Epic A's JSONB backfill migration (Story 40.2). Epic A's migration UPDATEs `item_revision.fields` to backfill `alt: ""` on image blocks. If this trigger lands first, that migration fails. Since Epic A is Wave 1 and this is Wave 2, the natural ordering is correct — but migration timestamps must enforce it.

## Dev Notes

### Architecture

The trigger function is straightforward:

```sql
CREATE OR REPLACE FUNCTION prevent_item_revision_update()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'item_revision rows are immutable';
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER item_revision_immutable
    BEFORE UPDATE ON item_revision
    FOR EACH ROW
    EXECUTE FUNCTION prevent_item_revision_update();
```

This is a `BEFORE UPDATE` trigger so the UPDATE is rejected before any row modification occurs. DELETE is unaffected because the trigger only fires on UPDATE events.

### Security

This is a defense-in-depth measure. Even if application code has a bug that attempts to modify a revision, the database will reject it. This protects the audit trail integrity at the storage layer.

For exceptional data-fix migrations that legitimately need to modify revisions (e.g., backfilling a new column on existing rows), the migration should:

```sql
ALTER TABLE item_revision DISABLE TRIGGER item_revision_immutable;
-- perform data fix
ALTER TABLE item_revision ENABLE TRIGGER item_revision_immutable;
```

This pattern should be documented and used only in migrations, never in application code.

### Testing

- **UPDATE rejection test**: Insert a revision, attempt `UPDATE item_revision SET ... WHERE id = ...`, verify the operation fails with "item_revision rows are immutable".
- **DELETE allowance test**: Insert a revision, DELETE it, verify success. This confirms CASCADE from item deletion still works.
- **INSERT allowance test**: Insert a new revision after the trigger is in place, verify success.

### References

- Migration SQL files in `migrations/`
- `item_revision` table schema
- PostgreSQL trigger documentation
