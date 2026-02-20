# Story 26.9: Document Quarterly Boundary Review Process

Status: done

## Story

As a **maintainer**,
I want a documented process for periodic kernel boundary reviews,
so that kernel minimality is maintained over time and extraction debt doesn't accumulate.

## Acceptance Criteria

1. Quarterly review process documented in `docs/kernel-minimality-audit.md`
2. Process includes: re-run audit checklist against new kernel code, compare LOC trends, review plugin extraction backlog
3. Plugin extraction backlog section added to audit doc for tracking candidates
4. New subsystem rule documented: any proposed kernel subsystem requires justification for why it can't be a plugin or trait
5. Review cadence tied to major releases or quarterly, whichever comes first

## Tasks / Subtasks

- [x] Add "Ongoing Maintenance" section to `docs/kernel-minimality-audit.md` (AC: #1, #2, #5)
  - [x] Document quarterly review process steps (5-step checklist)
  - [x] Define review cadence (quarterly or major release, whichever comes first)
- [x] Document new subsystem justification rule (AC: #4)
- [x] Add plugin extraction backlog section (AC: #3)
- [x] Cross-reference from CLAUDE.md

## Dev Agent Record

### Agent Model Used

Claude Opus 4.6

### Completion Notes List

- Added Section 8 (Ongoing Maintenance) to `docs/kernel-minimality-audit.md` with three subsections:
  - 8.1 Quarterly Boundary Review — 5-step review process with 10% growth threshold trigger
  - 8.2 Plugin Extraction Backlog — table tracking redirect, image style, and email services with blockers
  - 8.3 New Subsystem Rule — 3-question justification requirement, linked to PR template
- Updated CLAUDE.md cross-reference to mention LOC baseline and quarterly review process

### File List

- `~ docs/kernel-minimality-audit.md` (added Sections 7-8)
- `~ CLAUDE.md` (updated cross-reference)
