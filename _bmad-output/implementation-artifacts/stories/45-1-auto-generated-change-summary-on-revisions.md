# Story 45.1: Auto-Generated Change Summary on Revisions

Status: ready-for-dev

## Story

As a **content editor reviewing revision history**,
I want each revision to include a structured change summary,
so that I can quickly understand what changed between versions without manually comparing field values.

## Acceptance Criteria

1. Migration adds a `change_summary` column (JSONB, NULLABLE) to the `item_revision` table
2. On item save, the kernel diffs the previous revision's fields against the new fields and stores a structured summary
3. The change summary includes `fields_added` and `fields_removed` lists (field names)
4. The change summary includes `fields_changed` with old/new values for scalar fields (truncated to 200 characters)
5. For text/rich-text fields, the summary includes `word_count_delta` (integer difference)
6. For blocks fields, the summary includes `blocks_added`, `blocks_removed`, and `blocks_changed` counts
7. For category and reference fields, the summary includes added/removed reference IDs
8. The first revision of an item has a NULL `change_summary` (no previous version to diff against)
9. `change_summary` is serialized to plugins as read-only data on revision objects
10. At least 3 integration tests covering different change scenarios

## Tasks / Subtasks

- [ ] Write migration adding `change_summary JSONB NULL` to `item_revision` (AC: #1)
- [ ] Define `ChangeSummary` struct with fields: `fields_added`, `fields_removed`, `fields_changed`, per-type metadata (AC: #3, #4, #5, #6, #7)
- [ ] Implement field diffing logic: compare previous revision JSONB fields against new fields (AC: #2)
- [ ] Handle scalar field changes: store old/new values truncated to 200 chars (AC: #4)
- [ ] Handle text fields: compute word count delta (AC: #5)
- [ ] Handle blocks fields: count added/removed/changed blocks (AC: #6)
- [ ] Handle category and reference fields: diff reference ID lists (AC: #7)
- [ ] Set `change_summary` to NULL for the first revision (no previous version) (AC: #8)
- [ ] Serialize `change_summary` in plugin-facing revision data as read-only (AC: #9)
- [ ] Write integration test: create item, update a scalar field, verify change summary (AC: #10)
- [ ] Write integration test: update a text field, verify word_count_delta (AC: #10)
- [ ] Write integration test: update blocks field, verify block counts (AC: #10)

## Dev Notes

### Architecture

The diffing logic lives in the content save path, after the new revision is constructed but before it is inserted. The kernel loads the most recent existing revision (if any), deserializes its fields, and compares them against the incoming fields. The resulting `ChangeSummary` is serialized to JSONB and stored alongside the revision.

The `ChangeSummary` struct:

```rust
struct ChangeSummary {
    fields_added: Vec<String>,
    fields_removed: Vec<String>,
    fields_changed: Vec<FieldChange>,
}

struct FieldChange {
    field_name: String,
    change_type: FieldChangeType,
}

enum FieldChangeType {
    Scalar { old: Option<String>, new: Option<String> },  // truncated to 200 chars
    Text { word_count_delta: i64 },
    Blocks { added: u32, removed: u32, changed: u32 },
    References { added: Vec<Uuid>, removed: Vec<Uuid> },
}
```

### Testing

- **Scalar change test**: Create an item with `title = "A"`, update to `title = "B"`, verify `fields_changed` contains `title` with old="A", new="B".
- **Text change test**: Create an item with a 100-word body, update to 150 words, verify `word_count_delta = 50`.
- **Blocks change test**: Create an item with 3 blocks, remove 1 and add 2, verify counts.
- **First revision test**: Verify `change_summary` is NULL on the initial save.

### References

- `crates/kernel/src/content/` — item save path and revision creation
- `item_revision` table schema
- Plugin SDK revision serialization
