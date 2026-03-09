# Story 7.2: Workflow Transition Graph

Status: ready-for-dev

## Story

As a site administrator,
I want editorial workflow transitions defined as a directed graph with permission requirements,
So that content moves through stages in a controlled manner.

## Acceptance Criteria

1. Transitions imported: incoming→curated, curated→live, live→curated, curated→incoming
2. Each transition requires specific permission
3. Users without permission rejected
4. Invalid transitions (e.g., incoming→live) rejected

## Tasks / Subtasks

- [ ] Create `variable.workflow.editorial.yml` config (AC: #1, #2)
- [ ] Import config
- [ ] Test valid transition with permission (AC: #1)
- [ ] Test transition without permission → rejected (AC: #3)
- [ ] Test invalid transition → rejected (AC: #4)

## Dev Notes

- Workflow: directed graph of allowed transitions with permission gates
- Verify formal workflow validation exists in kernel — may need implementation
- Config: variable storing transition graph as structured data

### References

- [Source: docs/tutorial/plan-parts-03-04.md#Part 4 Step 3]
