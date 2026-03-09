# Story 8.1: Admin Content List with Filters

Status: ready-for-dev

## Story

As a site editor,
I want an admin content list page with filters for stage, type, author, and date,
So that I can find and manage content efficiently.

## Acceptance Criteria

1. Content list with columns: title, type, stage, author, last modified
2. Filter by stage (dropdown)
3. Filter by content type (dropdown)
4. Filter by author
5. Filter by date range
6. Filters combinable
7. List paginated

## Tasks / Subtasks

- [ ] Enhance admin content list template with filter dropdowns (AC: #1-#6)
- [ ] Wire filter parameters to content query (AC: #2-#5)
- [ ] Implement combinable filters (AC: #6)
- [ ] Add pagination (AC: #7)

## Dev Notes

- Admin content list: `crates/kernel/src/routes/admin.rs` or `admin_content.rs`
- Use admin macros from `templates/admin/macros/`
- SeaQuery for parameterized filter queries — never format!() with SQL

### References

- [Source: docs/tutorial/plan-parts-03-04.md#Part 4 Step 5]
