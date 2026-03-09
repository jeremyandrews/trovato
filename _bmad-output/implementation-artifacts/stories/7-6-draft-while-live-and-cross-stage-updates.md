# Story 7.6: Draft-While-Live & Cross-Stage Updates

Status: ready-for-dev

## Story

As a site editor,
I want to work on a draft version while the live version remains visible,
So that editorial work doesn't disrupt the published site.

## Acceptance Criteria

1. Live version remains publicly visible when Curated draft exists
2. Anonymous sees Live version, not draft
3. Importer updating Live item with Curated draft writes one revision with `other_stage_revisions` context

## Tasks / Subtasks

- [ ] Verify draft-while-live: create Curated draft of Live item (AC: #1)
- [ ] Verify anonymous sees Live version (AC: #2)
- [ ] Verify cross-stage update context (AC: #3)

## Dev Notes

- Draft-while-live: stage-specific item loading returns appropriate version per user role
- Cross-stage: `tap_item_save` context includes `other_stage_revisions`

### References

- [Source: docs/design/Design-Content-Model.md] — draft-while-live
