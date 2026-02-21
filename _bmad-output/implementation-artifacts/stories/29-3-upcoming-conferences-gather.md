# Story 29.3: "Upcoming Conferences" Gather with Pagination

Status: ready-for-dev

## Story

As a **site visitor**,
I want to see a list of upcoming conferences sorted by date,
so that I can discover conferences to attend.

## Acceptance Criteria

1. Gather definition created for `conference` Items with fields: name, start_date, end_date, city, country, online
2. Filter: `start_date >= current_date` (only future conferences shown)
3. Sort: `start_date` ascending (soonest first)
4. Pagination: 25 items per page with next/previous controls
5. Gather attached to `/conferences` route, accessible by anonymous users
6. Default rendering shows field values (name, dates, city, country, online status) -- no custom templates yet
7. Empty state handled gracefully (message when no upcoming conferences exist)
8. Tutorial Step 4 documentation written covering Gather definition, SQL generation, routing, and pagination

## Tasks / Subtasks

- [ ] Create the "Upcoming Conferences" Gather definition (AC: #1, #2, #3, #4)
  - [ ] Define base_item_type: "conference"
  - [ ] Define field selection: name, start_date, end_date, city, country, online
  - [ ] Add filter: start_date >= :current_date
  - [ ] Add sort: start_date ASC
  - [ ] Configure pager: items_per_page = 25
- [ ] Attach Gather to `/conferences` route (AC: #5)
  - [ ] Register route accessible to all users
  - [ ] Verify anonymous access works
- [ ] Verify default rendering (AC: #6)
  - [ ] Confirm field values display correctly
  - [ ] Confirm online boolean shows meaningful text
  - [ ] Confirm dates format readably
- [ ] Handle empty state (AC: #7)
  - [ ] Display appropriate message when no conferences match
- [ ] Write tutorial Step 4 documentation (AC: #8)
  - [ ] Gather definition syntax/structure
  - [ ] Under the Hood: generated SQL query
  - [ ] How to attach Gather to URL route
  - [ ] Pagination mechanics
  - [ ] Screenshot or description of the listing page

## Dev Notes

### Dependencies

- Story 29.1 (Define conference Item Type) must be complete
- Story 29.2 (Admin UI) should be complete (need seed data to see results)
- Epic 7 (Gather) provides all Gather infrastructure -- complete
- Epic 23 (Gather UI) provides admin UI for creating Gathers -- complete

### Gather Definition

```
GatherDefinition {
  base_item_type: "conference",
  fields: [name, start_date, end_date, city, country, online],
  filters: [
    { field: "start_date", op: Gte, value: ":current_date" }
  ],
  sorts: [
    { field: "start_date", direction: Asc }
  ],
  pager: { items_per_page: 25 }
}
```

This is a simplified version of the full Upcoming Conferences Gather from the Ritrovo Overview. Exposed filters, relationships, topics, and speaker joins come in Part 2.

### Key Files

- Gather definition storage (admin UI or config file)
- `crates/kernel/src/routes/gather.rs` -- Gather route handlers
- `templates/gather/` -- Gather display templates
- `docs/tutorial/part-01-hello-trovato.md` -- Tutorial chapter

### What's Deferred to Part 2

- Exposed filters (topic, country, online)
- Relationships (speaker join)
- Stage-aware filtering
- Search integration

### References

- [Source: docs/ritrovo/overview.md#Gather Definitions]
- [Source: docs/design/Design-Query-Engine.md]
- [Source: docs/ritrovo/epic-01.md#Step 4: Build Your First Gather]

## Dev Agent Record

### Agent Model Used

### Debug Log References

### Completion Notes List

### File List
