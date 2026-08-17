# Analysis: File-Based Seed Content for External Applications

**Date**: 2026-08-17
**Status**: Proposal, awaiting a direction decision before implementation

## Executive Summary

0.99.0 lets an external application ship its plugins, its templates and its
static assets from its own repository. The remaining question was how it ships
its content, so that a clean install renders a fixed demo without a network
dependency and without manual entry.

The answer is not "there is nothing"; it is "there is a half-path, and it is
wrong in ways a demo will notice". Content items are already a config entity
(`ConfigEntity::Item` / `ConfigItem`), already carried by `config export` and
`config import`, and the tutorial already ships fifteen seed conferences in
`docs/tutorial/config/seed-italian/` and documents importing them. That path has
no tests, one line of documentation, and an importer that writes items the
database will accept but the CMS cannot use properly: no revision, nothing
promotable to a front page, an anonymous author, a broken group id, and no search
embedding.

Three defects found alongside it are recorded with repro detail in section 2A,
because they bear on any bootstrap story and each needs its own change: `config
export` exits 1 on any database containing a tag, `config export` has no content
filter, and `config import` fails to parse eighteen of the seventy six files in the
tutorial's own config directory while reporting success.

So the work is mostly corrective, not additive. **Recommendation: promote content
out of the config entity list into its own `content export` / `content import`
pair, backed by a shared writer that the test fixtures also use.** The one
decision needed before implementation is whether content gets that separate CLI
surface or stays inside `config` as entity type `item`.

---

## 1. What Exists Today, Verified

Read in the tree at `main` (e6861d2), then exercised against a throwaway
database with the `trovato` binary.

### 1.1 Content is already a config entity

- `crates/kernel/src/config_storage/mod.rs:58` defines `ConfigItem`: `id`
  (defaulting to a fresh UUIDv7 when absent), `type`, `title`, `language`,
  `status`, `fields`, `created`, `changed`. Its doc comment says the
  database-managed fields "are populated with defaults on import".
- `ConfigEntity::Item(ConfigItem)` at `mod.rs:147`, entity type string `"item"`
  at `mod.rs:462`, described as "Content items (conferences, speakers, pages,
  etc.)".
- `entity_types::ITEM` is in `ENTITY_TYPE_ORDER` (`yaml.rs:46`), positioned after
  `item_type`, `category`, `tag`, `stage` and `url_alias`, so the dependency
  ordering for content is already correct.
- `DirectConfigStorage` implements all four operations for it:
  `load_item`, `save_item`, `delete_item`, `list_items`
  (`direct.rs:694` through `direct.rs:818`).
- `yaml.rs` serializes and deserializes it (`yaml.rs:190`, `yaml.rs:828`), so the
  on-disk convention is `item.<uuid>.yml`.

### 1.2 The tutorial already uses it

`docs/tutorial/config/seed-italian/` holds fifteen `item.<uuid>.yml` files.
`docs/tutorial/part-07-going-global.md:295` documents the command:

```
cargo run --release --bin trovato -- config import docs/tutorial/config/seed-italian
```

The files pin their uuids, set `language: it`, `status: 1`, and carry
field-level translations inside `fields` as
`field_description: {it: {...}, en: {...}}`, plus `field_topics` as an array of
tag uuids that match the `tag.<uuid>.yml` files in the parent directory.

`docs/tutorial/recipes/recipe-part-07.md:180` still says the directory "does not
exist yet", which is stale.

### 1.3 Zero tests, zero design documentation

`ConfigItem` appears nowhere in `crates/kernel/tests/`, nowhere in `plugins/`,
and nowhere in `docs/` except through the tutorial command above. It arrived in
the 0.99.0 squash, so git history offers no rationale.

---

## 2. What the Importer Actually Writes

Probe, reproducible from a clean checkout at `main` (e6861d2). Postgres 17 and
Redis from `docker compose up -d postgres redis`, then a database of its own so
nothing else is touched:

```
createdb seed_probe                        # or: psql -c 'CREATE DATABASE seed_probe;'
export DATABASE_URL=postgres://trovato:trovato@localhost:5432/seed_probe
export REDIS_URL=redis://localhost:6379

cargo run --bin trovato -- config import docs/tutorial/config
cargo run --bin trovato -- config import docs/tutorial/config/seed-italian
```

Then:

```sql
SELECT count(*)                                    AS items,
       count(current_revision_id)                  AS with_revision,
       sum(promote)                                AS promoted,
       count(DISTINCT author_id)                   AS authors,
       count(*) FILTER (WHERE id = item_group_id)   AS own_group,
       count(*) FILTER (WHERE search_vector IS NOT NULL) AS indexed
  FROM item;
SELECT count(*) AS revisions FROM item_revision;
```

```
 items | with_revision | promoted | authors | own_group | indexed
    15 |             0 |        0 |       1 |         0 |      15
 revisions: 0
```

Every prediction from reading `save_item` (`direct.rs:725`) holds:

| Property | Result | Why it matters |
|---|---|---|
| `item_revision` rows | **0** | `save_item` inserts into `item` only. `current_revision_id` stays NULL for all fifteen. Revision history is empty and the revert path has nothing to revert to. |
| `promote` | **0**, hardcoded at `direct.rs:737` | `Item::list_promoted` selects `promote = 1`. Seeded content can never reach the front page. |
| `author_id` | `uuid_nil()`, hardcoded at `direct.rs:749` | Every seeded item is authored by Anonymous. The FK is satisfiable because the nil user is seeded by `20260212000001_create_users.sql`, so this fails silently rather than loudly. |
| `item_group_id` | a fresh `Uuid::now_v7()` (`direct.rs:756`), never equal to `id` | `Item::create` sets `item_group_id = item_id`, "new items are their own group". Fifteen out of fifteen seeded items violate that invariant, which is the key the stage machinery uses to relate copies of one logical item across stages. |
| `search_vector` | populated for all 15 | Full text search works: the trigger is inside the insert. |
| `item_embeddings` / `item_embed_status` | **0 / 0** | No embedding and no queued intent, so seeded content is invisible to `SemanticSimilarity` gathers. This is the same defect that was found and fixed for plugin saves, documented at length in `crates/kernel/src/host/item.rs:18`. |
| `file_reference` | **0** | `sync_file_references` never runs, so the file to item index has no rows for seeded content. |
| per-item `url_alias` | **none** | The six aliases in the tutorial config are all gather routes. Pathauto runs from the route layer, not from any write path, so a seeded item is reachable only at its uuid route. |
| `stage_id` | live, hardcoded | Accidentally the right default for a demo, but it means staged content cannot round-trip at all. |
| re-import | 15 items, still 15 | Idempotency works, on the pinned uuid, through `ON CONFLICT (id) DO UPDATE`. |

---

## 2A. Three Adjacent Defects, With Repro

These are not part of the seeding design. They were found while verifying it, and
they are recorded here so the design decision is made with them known. Each is its
own piece of work.

### 2A.1 `config export` fails on any database containing a tag

**Repro**, continuing from the probe above (the tutorial config imports 32 tags,
so the database already qualifies):

```
$ cargo run --bin trovato -- config export /tmp/export-probe
Error: failed to list tag entities

Caused by:
    0: failed to list all tags
    1: no column found for name: slug
$ echo $?
1
```

**Root cause.** `DirectConfigStorage::fetch_all_tags`
(`crates/kernel/src/config_storage/direct.rs:363`) selects seven columns:

```rust
"SELECT id, category_id, label, description, weight, created, changed FROM category_tag ..."
```

`Tag` (`crates/kernel/src/models/category.rs:34`) has eight fields, including
`slug: Option<String>`, added by
`crates/kernel/migrations/20260307000001_add_category_tag_slug.sql`. The column
exists in the database; the query does not ask for it, so
`query_as::<_, Tag>` fails at row decode. It is the only `FROM category_tag`
select in that file, so nothing else compensates.

**Blast radius.** `config export` is unusable on every site that has a single tag,
which includes every site using categories, stages (stages are rows in the
`stages` category) or the tutorial config. `config import` is unaffected: it never
reads tags back.

**Fix.** Add `slug` to the select. The test that belongs with it exports a
database containing a tag and asserts the export succeeds, because the current
test suite never exports a populated database, which is why a one word omission
survived.

### 2A.2 `config export` has no content filter

`export_config` (`config_storage/yaml.rs:291`) iterates `ENTITY_TYPE_ORDER` and
calls `storage.list(entity_type, None)`. For `"item"`, `list_items`
(`direct.rs:776`) with a `None` filter is every row in the table.

**Consequence.** On a site whose importer has fetched ten thousand conferences,
`config export config/` writes ten thousand `item.<uuid>.yml` files next to the
thirty or so real config files. `config export --clean` then calls
`clean_stale_yml_files`, which deletes any `.yml` in the directory that this
export run did not write, so a hand authored seed file that has been edited but
not re-imported is deleted by an export. Content volume and config volume are not
the same problem, and one directory currently holds both.

This one is a consequence of the design decision in section 5, so it should be
resolved by that decision rather than patched ahead of it.

### 2A.3 `config import` reports success while failing to parse a quarter of its input

**Repro**, from a clean checkout:

```
$ cargo run --bin trovato -- config import docs/tutorial/config
Imported 58 config entities (docs/tutorial/config)
  category: 1
  gather_query: 6
  item_type: 2
  language: 2
  search_field_config: 6
  tag: 32
  url_alias: 6
  variable: 3
18 warning(s):
  ...
$ echo $?
0
```

Fifty eight entities from fifty eight files, eighteen files failing, seventy six
`.yml` files in the directory: the arithmetic closes, so nothing is being skipped
silently on top of the parse failures. The full list of the eighteen, grouped by
type for reading (the command emits them in directory read order):

```
failed to parse stage.live.yml: invalid stage YAML
failed to parse stage.incoming.yml: invalid stage YAML
failed to parse stage.curated.yml: invalid stage YAML
failed to parse stage.legal_review.yml: invalid stage YAML
failed to parse role.editor.yml: invalid role YAML
failed to parse role.publisher.yml: invalid role YAML
failed to parse role.viewer.yml: invalid role YAML
failed to parse menu_link.main.conferences.yml: invalid menu_link YAML
failed to parse menu_link.main.speakers.yml: invalid menu_link YAML
failed to parse menu_link.main.topics.yml: invalid menu_link YAML
failed to parse menu_link.main.cfps.yml: invalid menu_link YAML
failed to parse menu_link.footer.about.yml: invalid menu_link YAML
failed to parse menu_link.footer.contact.yml: invalid menu_link YAML
failed to parse tile.search_box.yml: invalid tile YAML
failed to parse tile.topic_cloud.yml: invalid tile YAML
failed to parse tile.footer_info.yml: invalid tile YAML
failed to parse tile.open_cfps_sidebar.yml: invalid tile YAML
failed to parse tile.conferences_this_month.yml: invalid tile YAML
```

Four stages, three roles, six menu links, five tiles. Nothing else in the
directory fails.

**Root cause**, one shape in four places. `deserialize_entity`
(`config_storage/yaml.rs:755`) deserializes these four types straight into the
full model struct, and each model requires fields that only the database can
know, which a hand authored file therefore omits:

| Type | File has | Struct additionally requires |
|---|---|---|
| `stage` | `id`, `category_id`, `label`, `visibility`, `is_default`, `weight` | `machine_name`, `created`, `changed` (and has no `category_id` field) |
| `role` | `name`, `label`, `permissions` | `id`, `created` (and has no `label` or `permissions` field, so role permissions cannot round-trip through config at all) |
| `menu_link` | `menu_name`, `path`, `title`, `weight`, `hidden`, `plugin` | `id`, `stage_id`, `created` |
| `tile` | `machine_name`, `label`, `region`, `tile_type`, `config`, `visibility`, `weight`, `status`, `plugin` | `id`, `stage_id`, `created`, `changed` |

The types that import cleanly either go through a purpose built export struct
(`TagExport`, `GatherQueryExport`, `VarYaml`, `ConfigItem`) or have a string key
with defaults for the rest (`category`, `language`, `item_type`). The `url_alias`
files import because they were machine generated and carry `id`, `stage_id` and
`created`. So the failure is not arbitrary drift: it is the four types that lack
an import shape.

**And it exits 0.** `print_config_summary` (`crates/kernel/src/main.rs:569`)
prints the warnings; `run_config_command` returns `Ok(())` regardless. A bootstrap
script that checks exit codes learns nothing, and CI has nothing to fail on.

**This lands harder than it looks.** KNOWN-ISSUES.md records that roles and
permissions, stages, and system configuration have no admin form and are managed
by editing YAML and running `config import`. Two of those three are in the failing
list, so for roles and stages the only management path the CMS offers is the one
that does not work on the examples the project ships, and the role file shape
shows permissions were never importable.

**Fix**, two parts that want to stay together: give the four types an import shape
(or defaults for the database managed fields), and add a test that imports
`docs/tutorial/config/` and asserts zero warnings, so the drift cannot come back
silently. Whether `config import` should exit non-zero on warnings is a separate
behaviour decision, raised as question 2 in section 6.

---

## 3. The Ownership Question

The kernel has a deliberate boundary: "The kernel enables; plugins implement"
(CONTRIBUTING.md). A content import path is exactly the kind of addition to argue
before writing, so here is the argument.

### 3.1 Plugin-owned seeding is closed by the frozen plugin contract

This is the finding that removes two options from the table rather than weighing
them.

- **A plugin cannot read a file.** `crates/wit/kernel.wit` exposes no filesystem
  interface of any kind. A plugin-owned loader would have to compile the seed
  content into its own wasm binary, which is not "a file-based way to ship seed
  content".
- **A plugin cannot write the rows a demo needs.** `save-item`
  (`host/item.rs:133`) treats a non-nil `id` in the payload as an update, so a
  plugin cannot pin a uuid on creation. The `CreateItem` it builds hardcodes
  `promote: Some(0)` and `sticky: Some(0)`, passes `stage_id: None`,
  `language: None` and `log: None`, and defaults `status` to 0 (unpublished) when
  absent (`host/item.rs:203` through `host/item.rs:215`).

So a plugin seeder cannot pin ids, cannot promote, cannot choose a stage, cannot
set a language, and cannot label its revisions. Fixing that means widening the
plugin surface, and the plugin contract does not change before 1.0 beyond
additive changes. Widening `save-item` to accept a pinned id would also change
the meaning of an existing field, which is not additive.

`tap_install` and `tap_enable` do exist (`plugin/info_parser.rs:291`), so the
*trigger* for plugin-owned seeding is available. The *capability* is not.

### 3.2 The minimality test, answered

CONTRIBUTING's test is whether another kernel subsystem depends on the thing, or
only feature routes. Three facts say this is infrastructure:

1. The kernel's own test fixtures depend on it. `TestApp::ensure_conference_items`
   (`tests/common/mod.rs:541`) is a content seeder, and it is *more correct* than
   the production importer: it writes the `item_revision` row and links
   `current_revision_id` (`tests/common/mod.rs:627` through `:646`). The kernel
   already contains two content writers, and the one used in production is the
   wrong one.
2. Correct item creation is not a feature. Revisions, group ids and index intent
   are invariants of the content model, which is kernel.
3. The surface already exists in the kernel. This proposal mostly moves and
   corrects `config_storage`'s item path. Declining it does not keep the kernel
   smaller; it keeps a broken path.

---

## 4. Options

### Option A: Finish the config path in place

Keep `item` as a config entity. Fix `save_item` to write through a correct create
path, widen `ConfigItem` with `promote`, `sticky`, `stage_id`, `author` and a
revision `log`, add a content filter to `export_config`, and test it.

**Pros**: smallest diff; no new CLI surface; nothing to relearn for anyone
already using `config import` for content, including the tutorial.

**Cons**: leaves content and config in one directory with one lifecycle. `config
export --clean` deleting hand written seed files stays a live hazard. "Config"
keeps meaning two different things, and the export filter becomes a flag that
exists only because of that conflation. Ships the volume footgun forever.

### Option B: A `content export` / `content import` pair

Move the item path out of the config entity list into `crates/kernel/src/content/`
with its own CLI subcommand, sharing the file naming, the two phase validate then
write structure, the warnings vocabulary and the `--dry-run` flag with
`config import`. `config export` stops emitting content; `content export` gains
the filters content needs (by type, by stage, by id list).

**Pros**: content and config get separate directories, separate lifecycles and
separate `--clean` semantics. The volume footgun is closed rather than flagged.
The format can carry what content needs (promote, stage, author, revision log, a
files manifest) without those fields looking odd on a config entity. Discoverable:
`trovato content import` is what someone looks for.

**Cons**: largest addition to the CLI surface, and a documented command changes
meaning, so the tutorial and `seed-italian/` move with it. Most of the code is
relocated rather than new, but the review is bigger.

### Option C: Documented bootstrap over what exists

No kernel change. A script composes `plugin install`, `config import` and a seed
step, plus documentation.

**Pros**: no kernel surface at all.

**Cons**: verified insufficient. Section 2 is a list of rows the importer does not
write; documentation cannot write them. Today's bootstrap produces a site with no
revisions, nothing promotable, no per-item aliases, no embeddings, eighteen
unparsed config files and an exit code of 0. A wrapper script is worth having, on
top of a fixed importer, not instead of one.

### Option D: A general fixtures loader

Factor a loader that both the CLI and the test fixtures use, so
`ensure_conference_items` and the importer stop being two implementations of one
idea.

**Pros**: removes the existing duplication; the correct writer gets exercised by
every test run, which is the strongest test coverage available.

**Cons**: not an alternative to A or B. This is the *internals* of whichever of
those is chosen, and it should be built that way.

---

## 5. Recommendation

**Option B, with Option D as its internals and a thin Option C wrapper on top.**

Reasoning, in the order that decided it:

1. Content is not config. It differs in volume by orders of magnitude, in
   lifecycle (config is authored once and versioned; content accumulates at
   runtime), and in export semantics (all config is meaningful to export; almost
   no content is). One directory and one `--clean` rule cannot serve both, and
   Option A's answer to that is a flag.
2. The footgun is worth removing, not documenting. `config export` currently
   dumps the entire content table, and `config export --clean` treats
   hand written seed files as stale. Under Option B that ceases to be possible.
3. The plugin route is closed (section 3.1), so the choice is only about where in
   the kernel this lives.
4. One writer, two callers. The kernel already has two content writers and uses
   the wrong one in production. Consolidating on the correct one, with the test
   fixtures as its second caller, is the change that keeps it correct.

### 5.1 Sketch: the file format

Same convention as config, in a content directory: `item.<uuid>.yml`, one item
per file, filename uuid must match the `id` inside.

```yaml
id: '031e0e55-59d2-4f4c-a2e2-81c50bce5b62'
type: conference
title: Codemotion Roma 2026
language: it
status: 1            # 1 published, 0 unpublished
promote: 1           # new: front page
sticky: 0            # new
stage: live          # new: machine name, not a raw uuid
author: admin        # new: username, resolved to author_id; falls back to anonymous
log: Seed content    # new: revision log for the created revision
created: 1741824000
changed: 1741824000
fields:
  field_description:
    it:
      value: "<p>...</p>"
      format: filtered_html
  field_topics:
    - c3d8982d-9060-4f05-8545-729a8cff7656
```

Three deliberate choices in that shape:

- **`stage` is a machine name, not a uuid.** Stage uuids are well known constants
  for live but arbitrary for application stages, and `Stage::find_by_machine_name`
  already exists.
- **`author` is a username, not a uuid.** User uuids are generated per install,
  so a uuid here would never resolve on a fresh database. An unresolvable
  username warns and falls back to anonymous, which is the current behaviour made
  explicit.
- **Everything else stays uuid keyed**, because uuids in files already work
  (section 5.2).

Files travel in a `files/` subdirectory of the content directory, with a
`file.<uuid>.yml` sidecar per file carrying `uri`, `filename`, `filemime`,
`filesize` and `status: 1`. Import copies the bytes under the upload root and
writes the `file_managed` row before the items that reference it, then
`sync_file_references` runs per item. Worth knowing: `can_serve_file`
(`item_service.rs:849`) does not gate unmanaged paths, so a seeded image renders
even with no `file_managed` row. The row is what makes it visible to the media
browser and to reference aware access control.

### 5.2 Sketch: id resolution

**There is no id remapping problem, and the design should not invent one.**

Items are UUIDv7, not serial. `ConfigItem` already accepts a pinned uuid and
`save_item` uses it as-is. References between content are uuid strings inside
`fields` JSONB with no foreign key, so item to item, item to tag and item to file
references round-trip exactly as written, in any file order. The tutorial's
`field_topics` arrays already prove this: they name tag uuids that the parent
directory's `tag.<uuid>.yml` files create.

What is missing is not resolution but **validation**. Nothing checks that a
referenced uuid resolves, so a typo produces a silently broken page. The proposal
is a third pass after all items are written, modelled on the deferred tag
hierarchy pass in `import_config` (`yaml.rs:553`):

1. **Validate**: parse every file, check filename against content id, check the
   `type` exists, check `stage` resolves, check `author` resolves.
2. **Write**: create or update each item through the shared writer.
3. **Resolve and verify**: walk each written item's fields for uuid shaped
   strings, check each against `item`, `category_tag` and `file_managed`, and
   report every dangling reference.

A machine name alias for items (`ref: conference/codemotion-roma-2026` resolving
to a uuid) is deliberately **not** proposed for the first version. Pinned uuids
already work; an alias layer adds a symbol table, an ordering constraint and a
second class of failure, in exchange for readability that a YAML comment provides
for free.

### 5.3 Sketch: the writer

One function, in `crates/kernel/src/content/`, used by the CLI and by
`TestApp::ensure_conference_items`:

- Takes a Postgres advisory lock, per CONTRIBUTING's rule for shared seeders and
  for the reason documented at `tests/common/mod.rs:47`, under its own lock key.
- Creates through `Item::create`, which writes the initial `item_revision` and
  links `current_revision_id` inside one transaction, then corrects
  `item_group_id` to equal `id`. `Item::create` currently mints its own uuid, so
  it needs an optional caller supplied id: that is the one model level change this
  requires.
- For an item that already exists, compares a content hash of title, status,
  fields, promote, sticky and stage. Unchanged means skip entirely; changed means
  `Item::update`, which writes a new revision with the seed's log message.
  Without the hash, every idempotent re-run would add a revision to every item,
  turning re-import into revision spam.
- Enqueues embed intent via `embed_index::enqueue_embed_job`, following the
  reasoning already written down at `host/item.rs:18`: record the intent, let the
  drain resolve it, dispatch no tap.
- Runs `sync_file_references`.
- Dispatches **no taps**. Seeding is not an editorial action, taps are not
  available to a CLI process with no request context, and the re-entrancy
  reasoning in `host/item.rs` applies.

### 5.4 Translations: explicitly split

- **Field level translations inside `fields` need nothing.** They already ride in
  the JSONB, exactly as `seed-italian/` does it, and are covered by the format
  above.
- **`item_translation` rows are out of scope for the kernel importer.** That table
  is created by a plugin migration
  (`plugins/trovato_content_translation/migrations/001_create_item_translation.sql`),
  so the kernel importing into it would have the kernel writing a plugin's
  schema. The kernel reading that table in `ItemService` is an existing wart and
  not something to build on. If file based translation rows are wanted, the
  translation plugin owns that path.

This should be stated in the format documentation rather than left for someone to
discover.

---

## 6. Questions for Stakeholder Review

1. **Is a separate `content` CLI surface acceptable, or should content stay inside
   `config`?** This is the decision everything else follows from.
2. **Should `--strict` (nonzero exit on any warning) be the default for content
   import, opt in, or absent?** A deterministic demo wants a bootstrap that fails
   loudly. Section 2A.3 shows what the current permissive behaviour hides. Changing
   the default for `config import` too would be a behaviour change on a documented
   command.
3. **Does the tutorial move with it?** Option B means `seed-italian/` becomes a
   content directory and `part-07` plus `recipe-part-07` change commands. Doing
   that in the same change keeps the tutorial honest; doing it separately keeps
   the diff smaller.

---

## 7. Action Items

Independent of the decision, the verified defects from section 2A. Each is its own
change, deliberately not bundled with the seeding work:

- [ ] **Section 2A.1.** `config_storage/direct.rs:365`: add `slug` to the
      `fetch_all_tags` select, with a test that exports a database containing a tag.
      `config export` currently exits 1 on any such database.
- [ ] **Section 2A.3.** Give `stage`, `role`, `menu_link` and `tile` an import
      shape, or defaults for the database managed fields they require, so the
      eighteen files in `docs/tutorial/config/` parse. Add a test that imports that
      directory and asserts zero warnings, or they drift again silently. The role
      shape needs a decision of its own: `label` and `permissions` are in the file
      and absent from the model, so role permissions have never been importable.
- [ ] Separately from either fix, decide whether `config import` should exit
      non-zero when it emits warnings (question 2). Today a quarter of the input
      can fail and the command still succeeds.
- [ ] `docs/tutorial/recipes/recipe-part-07.md:180`: `seed-italian/` exists;
      remove the "not yet created" note.

Following the decision:

- [ ] Decide question 1 before any code is written.
- [ ] Shared writer with the advisory lock, the revision, the group id, the embed
      intent and the file references, with `TestApp::ensure_conference_items`
      rewritten to call it.
- [ ] Optional caller supplied id on `Item::create`.
- [ ] Format support for `promote`, `sticky`, `stage`, `author` and `log`.
- [ ] Reference validation pass, with dangling references reported per file.
- [ ] Files: `files/` subdirectory, `file.<uuid>.yml` sidecars, `file_managed`
      rows written before the items that reference them.
- [ ] Remove content from `config export`, or filter it, per the decision.
- [ ] Documentation: the format, the translation split, and a one command
      bootstrap for an external application.
