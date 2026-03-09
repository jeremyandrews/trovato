# Story 3.4: Main Navigation Menu

Status: ready-for-dev

## Story

As a site visitor,
I want a main navigation menu with links to Conferences, Open CFPs, Topics, and Speakers,
So that I can navigate to the primary sections of the site.

## Acceptance Criteria

1. Navigation slot contains menu: Conferences, Open CFPs, Topics, Speakers
2. Topics menu item has hierarchical children matching topic category tags
3. Each menu link resolves to correct page

## Tasks / Subtasks

- [ ] Create menu config: `menu.main.yml` (AC: #1)
- [ ] Create menu link configs (AC: #1, #2):
  - [ ] `menu_link.conferences.yml` — /conferences, weight 0
  - [ ] `menu_link.open_cfps.yml` — /cfps, weight 10
  - [ ] `menu_link.topics.yml` — parent, weight 20
  - [ ] `menu_link.topics_languages.yml` — child of topics
  - [ ] `menu_link.topics_infrastructure.yml` — child of topics
  - [ ] `menu_link.topics_ai_data.yml` — child of topics
  - [ ] `menu_link.speakers.yml` — /speakers, weight 30
- [ ] Import config
- [ ] Render menu in Navigation slot of page template
- [ ] Create `templates/macros/menu.html` macro for menu rendering

## Dev Notes

### Architecture

- Menu system: `crates/kernel/src/menu/registry.rs`
- Menu config import — verify support in `config_storage/yaml.rs`
- Hierarchical menus: parent_id field on menu links
- Menu rendering: Tera macro iterating menu tree with ul/li structure

### References

- [Source: docs/design/Design-Web-Layer.md] — menu system
- [Source: docs/tutorial/plan-parts-03-04.md#Step 5] — menu configuration
