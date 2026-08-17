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
embedding. The export half of the pair fails outright on any database that has a
tag.

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

Probe: a fresh database, `config import docs/tutorial/config`, then
`config import docs/tutorial/config/seed-italian`, then inspection.

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

### 2.1 The export half is broken

```
$ trovato config export /tmp/probe
Error: failed to list tag entities
Caused by:
    0: failed to list all tags
    1: no column found for name: slug
```

`fetch_all_tags` (`direct.rs:364`) selects seven columns; `Tag`
(`models/category.rs:34`) has eight fields, including `slug`, added by
`20260307000001_add_category_tag_slug.sql`. `query_as::<_, Tag>` fails at row
decode, so `config export` exits non-zero on **any** database that contains a
tag, which is every real site. One line to fix, listed in Action Items.

### 2.2 Export has no content filter

`export_config` iterates `ENTITY_TYPE_ORDER` and calls `storage.list(entity_type,
None)`. For `"item"` that is every row in the table. On a site whose importer has
fetched ten thousand conferences, `config export` writes ten thousand
`item.<uuid>.yml` files into the config directory, and `config export --clean`
then treats any config file it did not write as stale. Content volume and config
volume are not the same problem, and one directory currently holds both.

### 2.3 The shipped bootstrap is not green

Importing the tutorial's own config directory produces eighteen parse failures
out of seventy four files, and exits 0:

```
18 warning(s):
  warning: failed to parse stage.live.yml: invalid stage YAML
  warning: failed to parse menu_link.main.conferences.yml: invalid menu_link YAML
  warning: failed to parse role.editor.yml: invalid role YAML
  warning: failed to parse tile.topic_cloud.yml: invalid tile YAML
  ... (all four stages, all six menu links, all three roles, all five tiles)
```

`stage.live.yml` carries `category_id` and no `machine_name`, `created` or
`changed`; the `Stage` struct requires the latter three. The files were written
against a shape the structs no longer have. So the reference application's
stages, menus, roles and tiles do not import, and `print_config_summary`
(`main.rs:569`) reports the warnings and returns `Ok(())`, so the command
succeeds. A bootstrap script that checks exit codes learns nothing.

This lands harder than it looks. KNOWN-ISSUES.md records that roles, stages and
system configuration have no admin form and are managed by editing YAML and
running `config import`. Those are three of the four kinds of file that fail to
parse, so for roles and stages the only management path the CMS offers is the one
that does not work on the examples the project ships.

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
   loudly. Section 2.3 shows what the current permissive behaviour hides. Changing
   the default for `config import` too would be a behaviour change on a documented
   command.
3. **Does the tutorial move with it?** Option B means `seed-italian/` becomes a
   content directory and `part-07` plus `recipe-part-07` change commands. Doing
   that in the same change keeps the tutorial honest; doing it separately keeps
   the diff smaller.

---

## 7. Action Items

Independent of the decision, verified defects:

- [ ] `config_storage/direct.rs:365`: add `slug` to the `fetch_all_tags` select.
      `config export` currently fails on any database with a tag. One line, and it
      wants a test that exports a database containing a tag.
- [ ] Regenerate or hand fix the eighteen `docs/tutorial/config/` files that no
      longer parse (four stages, six menu links, three roles, five tiles), and add
      a test that imports the tutorial config directory and asserts zero warnings.
      Without that test they will drift again.
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
