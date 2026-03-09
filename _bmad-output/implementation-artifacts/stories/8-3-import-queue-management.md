# Story 8.3: Import Queue Management

Status: ready-for-dev

## Story

As a site administrator,
I want an admin page showing Incoming items from the importer with approve and reject actions,
So that I can review and triage imported content.

## Acceptance Criteria

1. Page lists Incoming items sorted by import date (newest first)
2. Each item shows title, source, import date
3. "Approve" transitions to Curated (respects permissions)
4. "Reject" marks item as rejected

## Tasks / Subtasks

- [ ] Create import queue admin page/route (AC: #1, #2)
- [ ] Implement approve action → Incoming to Curated transition (AC: #3)
- [ ] Implement reject action (AC: #4)
- [ ] CSRF on all state-changing actions

## Dev Notes

- Filter content list by stage=Incoming, order by created DESC
- Approve: stage transition via workflow (requires permission)
- Reject: set active=false or delete (per policy)
- New admin route goes in appropriate `admin_*.rs` module, not `admin.rs`

### References

- [Source: docs/tutorial/plan-parts-03-04.md#Part 4 Step 5]
