# Story 33.3: Hierarchical Topic Taxonomy

Status: not started

## Story

As a **Ritrovo visitor**,
I want conferences tagged with topics in a browsable hierarchy (Languages > Systems > Rust),
so that I can discover conferences by interest area without knowing the exact tag name.

## Acceptance Criteria

1. Three-level topic taxonomy seeded during `tap_plugin_install` with the structure defined in the brief
2. All 26 confs.tech topic slugs mapped to taxonomy terms (see mapping table in brief)
3. `field_topics` on the `conference` item type holds category term references (added by plugin on install)
4. Story 33.2 queue worker populates `field_topics` when creating/updating conferences
5. `InCategory` Gather filter returns a term and all its descendants (e.g. "Languages" returns all Rust, Java, Python, etc. conferences)
6. A topic browser at `/topics` renders the full hierarchy as a nested list
7. Each topic page at `/topics/{slug}` shows a Gather filtered to that term (and descendants)
8. Breadcrumbs on topic pages show the full path (e.g. Languages > Systems > Rust)
9. Tutorial section covers: category system, `InCategory` operator, seeding taxonomy from a plugin
10. `trovato-test` blocks assert: taxonomy seeded correctly, `InCategory` query returns descendants, `/topics/rust` returns only Rust-tagged conferences

## Tasks / Subtasks

- [ ] Seed taxonomy in `tap_plugin_install` (AC: #1, #2)
  - [ ] Create top-level terms: Languages, Infrastructure, AI & Data, Web Platform, Security, General
  - [ ] Create second-level and third-level terms with correct parent references
  - [ ] Store confs.tech slug → term ID mapping in plugin config for use by importer
- [ ] Add `field_topics` to `conference` item type in `tap_plugin_install` (AC: #3)
  - [ ] Use `item_type_add_field()` host function (or equivalent) to append `RecordReference("category_term")` field
  - [ ] Set cardinality to unlimited (a conference can have multiple topics)
- [ ] Wire `field_topics` population into Story 33.2 queue worker (AC: #4)
  - [ ] Look up term ID for the topic slug from plugin config
  - [ ] Append term reference to `field_topics` on create/update
- [ ] Confirm `InCategory` gather operator traverses descendants (AC: #5)
  - [ ] Verify `has_tag_or_descendants` operator in `gather/types.rs` performs recursive category lookup
  - [ ] Add integration test if not already covered
- [ ] Create `/topics` browse page (AC: #6)
  - [ ] Gather query or custom route that renders the full taxonomy tree
  - [ ] URL alias: `/topics` → route
  - [ ] Template: `templates/gather/query--topic_browser.html` or custom admin route
- [ ] Create per-topic Gather and URL alias (AC: #7)
  - [ ] Gather `by_topic` with contextual filter: `field_topics InCategory {term_id from URL arg}`
  - [ ] URL alias pattern: `/topics/{slug}` → `/gather/by_topic?topic={term_id}`
  - [ ] Config YAML for reproducibility: `docs/tutorial/config/gather.by_topic.yml`
- [ ] Implement breadcrumbs on topic pages (AC: #8)
  - [ ] Template receives ancestor chain; renders `Languages > Systems > Rust`
- [ ] Write tutorial section 2.3 (AC: #9)
- [ ] Write `trovato-test` blocks (AC: #10)

## Dev Notes

### Taxonomy Seed Structure

Full hierarchy (term label / machine name):

```
Languages (languages)
  Systems (lang-systems)
    Rust (rust)
    Go (go)
    C (c)
    C++ (cpp)
    .NET (dotnet)
  JVM (lang-jvm)
    Java (java)
    Kotlin (kotlin)
  Web (lang-web)
    JavaScript (javascript)
    TypeScript (typescript)
    PHP (php)
    Python (python)
    Ruby (ruby)
  Mobile (lang-mobile)
    Android (android)
    iOS (ios)
  Functional (lang-functional)
    Elixir (elixir)
    Haskell (haskell)
Infrastructure (infrastructure)
  DevOps (devops)
  Networking (networking)
  IoT (iot)
  Performance (performance)
  Testing (testing)
AI & Data (ai-data)
  Data Engineering (data)
  Machine Learning (ml)
Web Platform (web-platform)
  CSS (css)
  UX (ux)
  GraphQL (graphql)
  WebAssembly (webassembly)
  Accessibility (accessibility)
  API (api)
Security (security)
  AppSec (appsec)
General (general)
  Leadership (leadership)
  Product (product)
  Open Source (opensource)
```

### confs.tech Slug → Taxonomy Term Mapping

| confs.tech slug | Term machine name |
|---|---|
| rust | rust |
| java | java |
| kotlin | kotlin |
| javascript | javascript |
| typescript | typescript |
| php | php |
| python | python |
| dotnet | dotnet |
| android | android |
| ios | ios |
| devops | devops |
| networking | networking |
| data | data |
| css | css |
| ux | ux |
| graphql | graphql |
| accessibility | accessibility |
| security | appsec |
| api | api |
| iot | iot |
| performance | performance |
| testing | testing |
| general | general |
| leadership | leadership |
| product | product |
| opensource | opensource |

### Adding `field_topics` to Content Type

The `conference` item type must gain a `field_topics` field of type `RecordReference("category_term")` with unlimited cardinality. Options:
1. Plugin calls a host function `item_type_add_field(type, field_def)` if it exists
2. Plugin runs a raw DB mutation via `db_exec()` to update `item_type.settings` JSONB directly

Check what field-mutation host functions exist in `crates/plugin-sdk/src/host_functions.rs`. If none exist, option 2 is the pragmatic path for now — document as a limitation and file a follow-up to add the host function.

### `InCategory` / `has_tag_or_descendants` Operator

The gather filter operator `has_tag_or_descendants` is defined in `gather/types.rs`. It should generate SQL like:

```sql
EXISTS (
  SELECT 1 FROM category_tag_hierarchy h
  WHERE h.descendant_id = item_category_ref.term_id
    AND h.ancestor_id = $term_id
)
```

Verify this is implemented in `gather/query_builder.rs`. If not, implement it as part of this story.

### `/topics` Browse Page

Options:
1. A custom kernel route at `/topics` that queries the category tree and renders a Tera template
2. A Gather with a custom `query--topic_browser.html` template

Option 1 is cleaner since Gathers are not designed for hierarchical tree rendering. If a plugin can register routes via a tap, use that. Otherwise implement as a kernel route gated by `plugin_gate!("ritrovo_importer")`.

### URL Aliases for Topic Pages

`/topics/{slug}` needs to resolve to the correct term. Approach:
- Store slug → term_id map in plugin config at install time
- Register a wildcard alias handler, or create individual URL aliases per term during `tap_plugin_install`

Individual aliases per term is the simpler approach and consistent with how the kernel alias system works.

### Key Files

- `plugins/ritrovo_importer/src/lib.rs` — taxonomy seed in `tap_plugin_install`, field_topics population in queue worker
- `crates/kernel/src/gather/query_builder.rs` — `has_tag_or_descendants` operator
- `crates/kernel/src/routes/` — `/topics` route (if custom) or gather route with template
- `templates/gather/query--topic_browser.html` — topic hierarchy template
- `docs/tutorial/config/gather.by_topic.yml` — config YAML
- `docs/tutorial/part-02-ritrovo-importer.md` — section 2.3

### Dependencies

- Story 33.1 complete
- Story 33.2 substantially complete (field_topics wired into queue worker)
- Category system (Epic 6 / `category` table, `category_tag_hierarchy`) must be functional
- `categories` plugin must be enabled or kernel must provide category term CRUD

### References

- Category model: `crates/kernel/src/models/` (category, category_tag, hierarchy)
- `has_tag_or_descendants`: `crates/kernel/src/gather/types.rs`
- Existing category admin: `crates/kernel/src/routes/admin_taxonomy.rs`

## Dev Agent Record

### Agent Model Used

### Debug Log References

### Completion Notes List

### File List
