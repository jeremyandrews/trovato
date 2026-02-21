# Story 28.5: Update Alignment Documentation

Status: review

## Story

As a **maintainer**,
I want the gap analysis and roadmap documentation to reflect current state,
So that contributors can trust the docs as an accurate picture of what's done and what remains.

## Acceptance Criteria

1. `docs/alignment/gap-analysis.md` updated: gaps #1, #2, #3, #5, #6 marked as resolved with implementation references
2. `docs/alignment/roadmap.md` updated: success criteria table reflects actual subsystem status
3. Roadmap "near-term" section updated to remove already-implemented items
4. Any new gaps discovered during Epic 28 documented

## Tasks / Subtasks

- [x] Update gap-analysis.md with resolution status for each gap (AC: #1)
- [x] Update roadmap.md success criteria table (AC: #2)
- [x] Update roadmap.md near-term recommendations (AC: #3)
- [x] Review and update intentional-divergences.md if needed

## Dev Notes

### Key Files

- `docs/alignment/gap-analysis.md`
- `docs/alignment/roadmap.md`
- `docs/alignment/intentional-divergences.md`

### Code Review Fixes Applied

- **Pathauto status corrected** — gap-analysis.md Gap #4 updated from "Defer" to "RESOLVED"; roadmap.md deferred table updated from "Being addressed" to "Resolved"
- **Local tasks status corrected** — gap-analysis.md Gap #17 updated to "RESOLVED"

## Dev Agent Record

### Implementation Plan

Documentation updates were completed in a prior session. This session verified accuracy of all references.

### Completion Notes

- **AC #1**: Gaps #1 (HTML Filter), #2 (Gather Admin UI), #3 (Email), #4 (Pathauto), #5 (Text Format Permissions), #6 (User Lifecycle Taps), #17 (Local Tasks) all marked RESOLVED with correct file references
- **AC #2**: Roadmap success criteria table shows 13/14 subsystems as Done; only Gap #14 (Blocks/Tiles) remains deferred to post-v1.0
- **AC #3**: Near-term section updated to reference Epic 28 stories for registration and pathauto
- **AC #4**: No new gaps discovered; intentional-divergences.md reviewed and remains accurate (no update needed)
- **Minor fix**: Corrected file path reference in Gap #2 from `admin_gather.rs` to `gather_admin.rs`

## File List

- `docs/alignment/gap-analysis.md` — all gaps marked resolved with implementation references
- `docs/alignment/roadmap.md` — success criteria and near-term sections updated

## Change Log

- 2026-02-21: Story implementation verified, minor file path fix applied, story marked for review
