# Story 3.2: Tile Config Import & Rendering

Status: ready-for-dev

## Story

As a site administrator,
I want to import Tile definitions via YAML config files,
So that content blocks appear in the correct Slot regions without manual database setup.

## Acceptance Criteria

1. Six Tiles created via config import: site branding (Header), search box (Header), conferences this month (Sidebar), open CFPs sidebar (Sidebar), topic cloud (Sidebar), footer info (Footer)
2. Each Tile renders its content in the assigned Slot
3. Tiles within same Slot ordered by weight

## Tasks / Subtasks

- [ ] Create YAML config files for 6 tiles (AC: #1):
  - [ ] `tile.site_branding.yml` — Header slot
  - [ ] `tile.search_box.yml` — Header slot
  - [ ] `tile.conferences_this_month.yml` — Sidebar slot (Gather-backed)
  - [ ] `tile.open_cfps_sidebar.yml` — Sidebar slot (Gather-backed)
  - [ ] `tile.topic_cloud.yml` — Sidebar slot
  - [ ] `tile.footer_info.yml` — Footer slot
- [ ] Import config
- [ ] Create Tile templates in `templates/tiles/` (AC: #2):
  - [ ] `site-branding.html`, `search-box.html`, `gather-sidebar.html`, `topic-cloud.html`, `footer-info.html`
- [ ] Verify weight ordering (AC: #3)

## Dev Notes

### Architecture

- Tile model: `crates/kernel/src/models/tile.rs`
- Tile admin: `crates/kernel/src/routes/tile_admin.rs`
- Config import needs to support Tile entities — verify in `config_storage/yaml.rs`
- Gather-backed tiles: `conferences_this_month` and `open_cfps_sidebar` execute gather queries
- Tile templates: `templates/tiles/{template_name}.html`

### References

- [Source: docs/design/Design-Web-Layer.md] — Tile system design
- [Source: docs/tutorial/plan-parts-03-04.md#Step 4] — Tile configuration details
