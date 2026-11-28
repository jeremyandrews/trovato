# Content import: `trovato import` (B167)

A site moving to Trovato brings its content with it: users, items and their
revision history, comments, URL aliases, redirects and tiles, each with the id
and the dates it already has. Before this command, every port wrote
`item`, `item_revision`, `comment`, `url_alias`, `redirect`, `tile` and
`users` with SQL in one transaction and then restarted the server, because
nothing told the running servers to drop their caches. Those ports depended on
table layouts that are not an interface, and skipped every check the kernel
makes on a write.

`trovato import <file>` is the interface instead. A port writes a file in the
format below; the kernel validates every record the way its own forms do,
writes the whole file in one transaction, and tells the running servers.

## Why a command, and only a command

Choosing a record's id and its created and changed dates is a migration
power. No form, route, JSON API or plugin host function offers it, and this
design adds it to none of them:

- The importer is a module of the kernel crate (`crates/kernel/src/import/`),
  reached only from `trovato import` in `main.rs`. Its write statements are
  private to that module. No `CreateItem`, `UpdateItem`, `CreateComment` or
  other input struct gains an `id` or a date field, so no existing caller can
  set one by accident or by being asked to.
- The command runs as the operator, the same bypass every other command runs
  with (`docs/cli-reference.md`): whoever can run it already has the database.
  There is no acting user to gate, so the per-user write gates (publish,
  promotion, authoring, field access) are not asked; the checks on the
  *values* are, as for an administrator.
- Passwords are never read from argv or written to the log. A user record
  carries a password **hash**, never a password, and only a hash the kernel
  verifies.

## The file

Newline delimited JSON (`.ndjson`): one JSON object per line, UTF-8. Blank
lines and lines starting with `#` are skipped, so a port can comment its
output. Every record has:

| Key | Meaning |
|---|---|
| `type` | `user`, `file`, `item`, `revision`, `comment`, `alias`, `redirect` or `tile`. |
| `id` | The record's id, a UUID the caller chooses. It is the record's identity: importing the same id again updates that record, never a second one. |

and the keys of its type below. A key the type does not have is an error
(a misspelt `chnaged` fails the import rather than being ignored). Dates
(`created`, `changed`) are either Unix seconds (`1554112800`) or an RFC 3339
string (`"2019-04-01T10:00:00Z"`).

**Order.** A record may refer only to records on earlier lines or already
stored: users before the items they wrote, an item before its revisions and
comments, a parent comment before its replies. A port that writes the file in
that order (which a dump of the old site's tables naturally is) never notices
the rule; a reference to a later line fails with that line's number.

### `item`

The item as it is now: its current revision.

| Key | Required | Meaning |
|---|---|---|
| `item_type` | yes | Content type machine name; must exist. |
| `title` | yes | Non-empty, at most 1000 bytes (the forms' limit). |
| `author_id` | yes | A user id, stored or on an earlier line. |
| `created` | yes | Authored on. |
| `changed` | yes | Last changed; the current revision's date. Not before `created`. On a change, not before the stored `changed`. |
| `status` | no | 0 or 1; default 1. |
| `promote`, `sticky` | no | 0 or 1; default the content type's publishing defaults. |
| `language` | no | A configured language; default the site's default language. |
| `fields` | no | The field values, as the JSON API takes them; default `{}`. |
| `revision_id` | no | The current revision's id. Default: a new one. |
| `revision_author_id` | no | Who saved the current revision. Default `author_id`. |
| `log` | no | The current revision's log message. |

The comparison that decides *unchanged* covers the item: `title`, `author_id`,
`status`, `promote`, `sticky`, `language`, `fields`, `created` and `changed`.
`revision_id`, `revision_author_id` and `log` describe the revision a create
or a change writes, and do not make a change on their own.

### `revision`

A past revision of an item, for the history the revisions page lists.

| Key | Required | Meaning |
|---|---|---|
| `item_id` | yes | The item, stored or on an earlier line. |
| `author_id` | yes | Who saved it. |
| `title` | yes | As for `item`. |
| `created` | yes | When it was saved; not after the item's current revision. |
| `status` | no | 0 or 1; default 1. |
| `fields` | no | As for `item`. |
| `log` | no | Log message. |

### `comment`

| Key | Required | Meaning |
|---|---|---|
| `item_id` | yes | The item commented on. |
| `parent_id` | no | The comment replied to, on the same item. |
| `author_id` | yes | A user id. |
| `body` | yes | Non-empty. |
| `body_format` | no | A text format that exists; default `filtered_html`. |
| `subject` | no | At most 255 characters (the comment form's limit). |
| `status` | no | 0 unpublished, 1 published, 2 pending, 3 spam; default 1. |
| `created`, `changed` | yes | `changed` not before `created`. |

Whether the item's content type takes new comments is not asked: that setting
decides what visitors may post, and a closed thread keeps the comments it
already has. A stored comment is not moved to another item or parent (its
depth was set when it was written); its other values are updated in place.

### `alias`

| Key | Required | Meaning |
|---|---|---|
| `source` | yes | The system path, `/item/<id>` for an item. |
| `alias` | yes | The address, as the alias form normalises it (leading `/`). |
| `language` | no | Default the site's default language. |
| `created`, `changed` | yes | |

The stage is the live stage, as for every alias the forms create on it.

### `redirect`

Needs the `trovato_redirects` plugin's table.

| Key | Required | Meaning |
|---|---|---|
| `source` | yes | The old path, canonicalised as the redirect form does. |
| `destination` | yes | A local path or an `http(s)://` URL, as the form allows. |
| `status_code` | no | 301, 302, 303, 307 or 308; default 301. |
| `language` | no | Default none (every language). |
| `created` | yes | The table has no `changed`. |

### `tile`

| Key | Required | Meaning |
|---|---|---|
| `machine_name` | yes | As the tile form checks it. |
| `label`, `region` | yes | |
| `tile_type` | no | Default `custom_html`. |
| `config` | no | For `custom_html`, `{"body", "format"}`; the format must exist. |
| `visibility` | no | As the tile form stores it. |
| `weight` | no | Default 0. |
| `status` | no | 0 or 1; default 1. |
| `created`, `changed` | yes | |

### `user`

| Key | Required | Meaning |
|---|---|---|
| `name` | yes | As the "Add user" form checks it; unique ignoring case. |
| `mail` | yes | Unique ignoring case. |
| `status` | no | 0 blocked, 1 active; default 1. |
| `created` | yes | |
| `password_hash` | no | An Argon2 PHC string (`$argon2id$…`), the only format the kernel verifies. Anything else is refused. Without one the account has no password and cannot sign in until `trovato user reset-password` (or a password reset) sets one. |
| `roles` | no | Role names that exist; never the superuser flag. |

A hash from another system (Drupal's `$S$`, bcrypt, MD5) is not accepted and
not stored: the kernel could not verify it, so it would be an account that
looks like it has a password and cannot sign in.

A user record without `password_hash` leaves a stored password alone, so a
password set with `reset-password` after the first run survives every later
run. Roles listed are granted; none is taken away. The users table has no
`changed`. A refused hash is never repeated in the error. No
`tap_user_register` fires, so a migrated account gets no welcome mail.

Tiles are on the live stage; a `machine_name` another tile holds there is
refused, naming that tile.

### `file`

A file already copied into the files directory (`UPLOADS_DIR`), from the old
site's export. The importer never uploads, moves or rewrites a byte of it: it
only reads the file at `path` to check and measure it, then registers a
`file_managed` row pointing at that same path.

| Key | Required | Meaning |
|---|---|---|
| `owner_id` | yes | A user id, stored or on an earlier line. |
| `path` | yes | Where the bytes already sit, relative to the files directory. No absolute path, no `..`, no symlink leading outside it. |
| `filename` | yes | The name shown to users. Its extension must agree with `mime_type` ([`EXTENSION_MIME_TYPES`]). |
| `mime_type` | yes | Checked against the bytes' magic numbers and the allowlist, exactly as an upload's declared type is ([`FileService::validate_file`]). |
| `created` | yes | When the file was first stored on the old site. |

```json
{"type":"file","id":"0193a5a0-0000-7000-8000-00000000f001","owner_id":"0193a5a0-0000-7000-8000-000000000003","path":"2019/04/conference-photo.jpg","filename":"conference-photo.jpg","mime_type":"image/jpeg","created":"2019-04-01T10:00:00Z"}
```

Importing a `file` before the items that reference it (by its id in a File
field, or its `local://path` / `/files/path` embedded in a rich text field)
lets those items resolve it exactly as they would resolve an uploaded file:
`file_refs::sync_item_file_references` finds a managed file by id or URI alone
and does not care how it got there. A `file` record is written `Permanent`
(not the temporary status a fresh upload gets), since it is already attached
to the content that references it. Size is measured from the bytes on disk,
never trusted from the record. The file's storage path (and so its URI) is
fixed at creation: a later record for the same id may update its owner, name,
type and date, but not move it to another path.

## Upsert by id

Each record is looked up by its `id`:

- **Absent**: created, with that id and those dates.
- **Present and identical** to what the record says: nothing is written. No
  new revision, no `changed` bump. Counted *unchanged*.
- **Present and different**: updated. For an item that means a new revision,
  dated the record's `changed`, carrying its title, status and fields, with
  the item row pointing at it, as an edit through the form makes one. For the
  other types, the row is updated in place to what the record says.

A **revision** is the exception: revisions are immutable (the
`item_revision_immutable` trigger, unchanged here). A stored revision whose
record now says something different fails the import, naming the line; it is
never rewritten. An item's *current* state changes by a changed `item`
record, which adds a revision.

So a rerun of the same file writes nothing and reports 0 changes; editing one
record and rerunning reports exactly 1. Records absent from the file are left
alone: the importer never deletes.

## Validation

Every record is validated before it is written, and every check is one
the kernel already makes on its own writes:

- **Items and revisions**: the content type must exist; `fields` pass
  `field_value::check_json_fields` (the JSON API's check: each declared field
  in a shape its type reads, a long text's format one the writer may use) with
  the text formats an administrator may use, i.e. every stored format; the
  required fields the forms require (`compound::validate_required_fields`);
  then the checks `ItemService` runs on every create and update, in the same
  order: page builder pages against the component registry, owned category
  references, field cardinality and category references
  (`field_values::validate_field_values`), and the file references the fields
  hold (`file_refs`), which are recorded with the item as a form save records
  them. The title length is the forms' limit. A Link field's `uri` must be an
  absolute `http`/`https` URL or a local path; a List field's value must be one
  of its `settings.allowed_values` keys; a Telephone field is filled in with
  its E.164 form (when one can be derived from it) before the check runs, as
  on every other write path.
- **References**: every id a record names (author, item, parent comment) must
  be stored or on an earlier line.
- **The other types**: the checks their forms make, listed per type above.

A record that fails fails the whole import: the command prints
`line <n>: <what is wrong>`, exits nonzero, and nothing from the file is
written.

## One transaction

The whole file is applied in one database transaction: every record is
validated and written in turn inside it, and it commits only when the last
line has passed. A failure anywhere rolls all of it back. `--dry-run` runs
exactly the same validation and writes, then rolls back, and reports what it
would have done.

The database triggers keep everything derived current inside that
transaction, as they do for a form save: an item's `search_vector`
(`trg_item_search`, comments included through `trg_comment_search`), the
static search index queue (`item_search_index_queue_*`,
`comment_search_index_queue_*`, `url_alias_search_index_queue`), comment depth
and an item's comment count.

At commit, if anything changed, every cache kind is bumped once
(`cache::clear_everywhere`, the path `trovato cache clear` takes), so every
running server drops its copies and serves the imported content on its next
request, with no restart. One audit row (`import`) records the counts.

## What the import does not do, on purpose

- **No plugin taps** (`tap_item_presave`, `tap_item_insert`, `tap_item_update`,
  `tap_comment_insert`, `tap_user_register`, …). A presave tap may rewrite the
  fields, which would store something other than the file and make every rerun
  a change; the notification taps send mail to subscribers and new users, which
  a migration of ten years of content must not. Search plugins are reached
  through the search index queue the triggers fill.
- **No automatic aliases.** Pathauto runs on item creates through the forms;
  an import writes the aliases its file lists and no others, so a site keeps
  the addresses it had.
- **No content moderation transition.** An imported item's `status` is
  stored as given; on a moderated type its revisions carry no moderation
  state, which reads as published or draft from the status, as for content
  saved before the type was moderated.
- **No AI embeddings** are queued; the kernel's embed job runs on the next
  save of each item.
- **No deletes.** Records absent from the file are left as they are.
- **No file uploads or copies.** A `file` record registers a file a port has
  already placed in the files directory; the importer never fetches, uploads
  or writes bytes of its own. Image style derivatives are not generated at
  import time either — the existing on-demand route makes one the first time
  it is requested, the same as for an uploaded original.

## Output

```
$ trovato import site.ndjson
Imported site.ndjson (412 lines):
  user: 2 created, 0 updated, 0 unchanged
  item: 50 created, 0 updated, 0 unchanged
  revision: 100 created, 0 updated, 0 unchanged
  ...
Running servers have dropped their cached copies; the next request reads the database.
```

`--dry-run` prints `Would import`, and never bumps a cache.

## Inventory: how each record is created today

| Record | Entry point | Model write | Validates | Fires | Stamps |
|---|---|---|---|---|---|
| item | `ItemService::create_moderated` (`content/item_service.rs`) | `Item::create_with_file_refs` (`models/item.rs`), own tx | write gates (publish, promotion, authoring, field access), page builder, owned category references, `field_values::validate_field_values`, file references; routes add `check_json_fields` / the form builder and the title length | `tap_item_presave`, pathauto alias, `tap_item_insert`, `tap_item_update_index`, embed job, audit `create` | `Uuid::now_v7()` for item and revision, `created` (unless authored on is set by `administer content`) and `changed` now |
| item update | `ItemService::update_moderated` | `Item::update_with_file_refs`, own tx | the same, against the stored item | presave, pathauto, `tap_item_update`, index, item cache, audit `update` | new revision `now_v7()`, `changed` and revision `created` now |
| revision | only as a side effect of the two above | | immutable by trigger once written | | |
| comment | `CommentService::create_with_mail` (`services/comment.rs`) | `Comment::create` (`models/comment.rs`, any executor) | routes: item access, comments open, parent on the same item, body non-empty, subject ≤ 255 | `tap_comment_insert`, mail, audit | `now_v7()`, `created`/`changed` now |
| alias | `UrlAliasService::create` (`services/url_alias.rs`) | `UrlAlias::create` (`models/url_alias.rs`, pool) | `normalize_path`; unique `(alias, language, stage_id)` | menu cache bump, audit | `now_v7()`, `created`/`changed` now |
| redirect | `routes/admin_redirect.rs` | `Redirect::create` (`services/redirect.rs`, pool) | `canonicalize_source`, `validate_redirect_destination`, status code | local redirect cache clear | `now_v7()`, `created` now |
| tile | `TileService::create` (`services/tile.rs`) | `Tile::create` (`models/tile.rs`, pool) | routes: machine name, config, visibility; unique `(machine_name, stage_id)` | audit | `now_v7()`, `created`/`changed` now |
| user | `UserService::create_with_status_and_mail` (`services/user.rs`), `trovato user create` | `User::insert_hashed` (`models/user.rs`, any executor) | `new_account_problems` (`routes/admin_user.rs`): name, mail, password policy, name and mail taken | `tap_user_register`, audit | `now_v7()`, `created` the database default |
| file (upload) | `FileService::upload` (`file/service.rs`) | `FileService::store`, pool, own id | size, MIME allowlist, magic bytes, extension (`validate_file`) | none | `Uuid::now_v7()`, `created`/`changed` now, `Temporary` |

`trovato config import` (`config_storage/yaml.rs::import_config`) is the
pattern this follows: an operator command that checks the whole set first,
writes it in one transaction, then bumps every cache kind from the command
(`main.rs::bump_from_cli`).

## Delivered in three pull requests

1. The format, the command, items and revisions, and this document.
2. Comments, aliases and redirects.
3. Users and tiles.

Each stands alone: a record type a build does not know yet is refused by name,
so an older build fails a newer file loudly rather than skipping its lines.

A fourth pull request added the `file` record (B184): a port's files arrive on
disk ahead of the import run, and this is how they are registered without a
second upload of bytes the port already placed.
