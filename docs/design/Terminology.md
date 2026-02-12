# Trovato: Terminology Mapping

Trovato uses its own terminology rather than Drupal's. This reference maps old Drupal terms to their Trovato equivalents.

## Renamed Concepts

| Drupal Term | Trovato Term | Notes |
|-------------|-------------|-------|
| Node | Item | Content items (not Rust AST nodes) |
| Hook | Tap | Extension points (e.g., tap_item_view, tap_form_alter) |
| Module | Plugin | CMS extensions loaded via WASM (not Rust `mod` keyword) |
| Views | Gather | Query builder engine |
| Block | Tile | Renderable content regions |
| Taxonomy | Categories / Tags | Hierarchical and flat classification |
| Entity | Record | Base content abstraction |
| Region | Slot | Theme layout regions |
| Workspace | Stage | Content staging environments (live, draft, campaign, etc.) |

## Unchanged Terms

Field, User, Permission, Menu, Cache, Kernel, Cron, Queue, Theme, Filter

## Database Table Renames

| Old                     | New                    |
| ----------------------- | ---------------------- |
| node                    | item                   |
| node_revision           | item_revision          |
| node_type               | item_type              |
| taxonomy_vocabulary     | category               |
| taxonomy_term           | category_tag           |
| taxonomy_term_hierarchy | category_tag_hierarchy |
| workspace               | stage                  |
| workspace_association    | stage_association      |
| workspace_deletion       | stage_deletion         |

## Naming Convention Notes

- "WASM module" (lowercase, the binary artifact) is still valid technical terminology
- "Plugin" (capitalized or in CMS context) replaces "Module" as the extensibility concept
- "Rust module" (`mod`) unchanged — refers to Rust's own module system
- hook_ prefix in function names → tap_ prefix (e.g., hook_node_view → tap_item_view)
- Compound terms follow: hook_form_alter → tap_form_alter, hook_menu → tap_menu
