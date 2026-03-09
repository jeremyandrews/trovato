# Story 1.2: CFP Gather Template

Status: ready-for-dev

## Story

As a site visitor,
I want the Open CFPs listing to use a dedicated template with CFP-specific styling,
So that I can quickly see deadlines and submission links.

## Acceptance Criteria

1. Open CFPs gather renders using `query--ritrovo.open_cfps.html` (not default gather template)
2. Each result displays the CFP deadline prominently
3. Each result includes a "Submit" link to the CFP URL
4. Template inherits from the base page layout

## Tasks / Subtasks

- [ ] Create `templates/gather/query--ritrovo.open_cfps.html` (AC: #1)
  - [ ] Display CFP deadline with prominent styling (AC: #2)
  - [ ] Include "Submit Talk" link using `field_cfp_url` (AC: #3)
  - [ ] Extend base page layout (AC: #4)
- [ ] Verify gather template resolution: `query--{query_id}.html` → `query.html`

## Dev Notes

### Architecture

- Gather templates live in `templates/gather/`
- Template naming: `query--{query_id}.html` where query_id matches the gather query machine name
- The `open_cfps` gather query already exists from Part 2 (defined in config)
- Existing template pattern: `templates/gather/query--ritrovo.upcoming_conferences.html`
- Conference fields available: `field_cfp_deadline`, `field_cfp_url`, `field_website`

### Key Files

- `crates/kernel/src/routes/gather_routes.rs` — dynamic gather route handling
- `templates/gather/query--ritrovo.upcoming_conferences.html` — existing pattern to follow

### References

- [Source: docs/tutorial/plan-parts-03-04.md#Step 1] — CFP template requirements
- [Source: crates/kernel/src/theme/engine.rs] — template suggestion resolution
