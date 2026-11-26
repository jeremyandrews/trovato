# Migrating a site to Trovato

A migration has two halves. The **structure** (content types, categories,
roles, text formats, menus) is configuration, and moves with
`trovato config import` (`docs/cli-reference.md`). The **content** moves with
`trovato import`: the old site's records, each keeping its id and its dates,
written through the kernel's own checks in one transaction. Neither half
writes Trovato's tables directly, so neither depends on their layout, and
neither needs a server restart.

## Importing content

Write one JSON object per line for each record, in the order the records
depend on each other: the authors before the items they wrote, an item before
its past revisions. Every record has a `type` and an `id`; the id is a UUID
the port chooses and keeps, because it is the record's identity on every
later run. A port that has numeric ids derives a UUID from each, the same
way every time (a UUIDv5 of `"<table>:<old id>"` under a namespace of the
port's own, for example), so a rerun finds the records the last run wrote.

The full format is in [`docs/design/import.md`](design/import.md): users,
items and their revisions, comments, URL aliases, redirects and tiles.

### Accounts first

The authors come first, with their old ids turned into UUIDs and their
sign-up dates. A password moves only as an Argon2 hash (`$argon2id$…`), the
one format Trovato verifies; an account from a system that hashed another way
comes without one, and its owner sets a password through a reset (or the
operator runs `trovato user reset-password`):

```json
{"type":"user","id":"0192f0a0-0000-7000-8000-00000000a001","name":"archivist","mail":"archivist@example.com","created":"2015-09-01T12:00:00Z","roles":["editor"]}
```

The examples below assume that line was imported (in its own file, or at the
top of the same one).

### A worked example

An article written in 2019, edited twice, last in 2021, by
`0192f0a0-0000-7000-8000-00000000a001`:

```json
{"type":"item","id":"0192f0a0-0000-7000-8000-000000000101","item_type":"page","title":"Moving the archive","author_id":"0192f0a0-0000-7000-8000-00000000a001","created":"2019-03-04T09:00:00Z","changed":"2021-06-01T14:30:00Z","revision_id":"0192f0a0-0000-7000-8000-000000000103","log":"Fixed the dates","fields":{"body":{"value":"<p>The archive moves on Friday.</p>","format":"filtered_html"}}}
{"type":"revision","id":"0192f0a0-0000-7000-8000-000000000111","item_id":"0192f0a0-0000-7000-8000-000000000101","author_id":"0192f0a0-0000-7000-8000-00000000a001","title":"Moving the archive","created":"2019-03-04T09:00:00Z","log":"First draft","fields":{"body":{"value":"<p>The archive moves.</p>","format":"filtered_html"}}}
{"type":"revision","id":"0192f0a0-0000-7000-8000-000000000112","item_id":"0192f0a0-0000-7000-8000-000000000101","author_id":"0192f0a0-0000-7000-8000-00000000a001","title":"Moving the archive","created":"2020-01-10T08:15:00Z","log":"Added the day","fields":{"body":{"value":"<p>The archive moves on Thursday.</p>","format":"filtered_html"}}}
```

The `item` line is the article as it is now; its current revision gets the
`revision_id` and is dated `changed`. The two `revision` lines are its
history, each dated when it was saved. Check the file, then import it:

```
$ trovato import archive.ndjson --dry-run
Would import 3 records from archive.ndjson:
  item: 1 created, 0 updated, 0 unchanged
  revision: 2 created, 0 updated, 0 unchanged
3 change(s), none written.
$ trovato import archive.ndjson
Imported 3 records from archive.ndjson:
  item: 1 created, 0 updated, 0 unchanged
  revision: 2 created, 0 updated, 0 unchanged
3 change(s).
Running servers have dropped their cached copies; the next request reads the database.
```

The article is at `/item/0192f0a0-0000-7000-8000-000000000101`, dated
4 March 2019, with three revisions on its revisions page.

### Its comments, its address and its old address

The same file can carry the article's comments, the address it had on the old
site, and a redirect from the address before that. Each comes after what it
refers to:

```json
{"type":"comment","id":"0192f0a0-0000-7000-8000-000000000201","item_id":"0192f0a0-0000-7000-8000-000000000101","author_id":"0192f0a0-0000-7000-8000-00000000a001","subject":"Which Friday?","body":"<p>This Friday or next?</p>","created":"2021-06-02T08:00:00Z","changed":"2021-06-02T08:00:00Z"}
{"type":"comment","id":"0192f0a0-0000-7000-8000-000000000202","item_id":"0192f0a0-0000-7000-8000-000000000101","parent_id":"0192f0a0-0000-7000-8000-000000000201","author_id":"0192f0a0-0000-7000-8000-00000000a001","body":"<p>This one.</p>","created":"2021-06-02T09:30:00Z","changed":"2021-06-02T09:30:00Z"}
{"type":"alias","id":"0192f0a0-0000-7000-8000-000000000301","source":"/item/0192f0a0-0000-7000-8000-000000000101","alias":"/news/moving-the-archive","created":"2019-03-04T09:00:00Z","changed":"2019-03-04T09:00:00Z"}
{"type":"redirect","id":"0192f0a0-0000-7000-8000-000000000401","source":"/node/101","destination":"/news/moving-the-archive","created":"2019-03-04T09:00:00Z"}
```

The reply names its parent, so it is threaded under it. The alias serves the
article at `/news/moving-the-archive`, and the redirect sends the old site's
`/node/101` there with a 301. Redirects need the `trovato_redirects` plugin.

### A tile

Tiles carry their region, weight and body; a custom HTML tile's format must be
one the site has:

```json
{"type":"tile","id":"0192f0a0-0000-7000-8000-000000000501","machine_name":"archive_notice","label":"About the archive","region":"sidebar_first","config":{"body":"<p>Older articles moved here in 2019.</p>","format":"filtered_html"},"created":"2019-03-04T09:00:00Z","changed":"2021-06-01T14:30:00Z"}
```

### Running it again

Run the same file again and nothing is written:

```
$ trovato import archive.ndjson
Imported 3 records from archive.ndjson:
  item: 0 created, 0 updated, 1 unchanged
  revision: 0 created, 0 updated, 2 unchanged
0 change(s).
```

So a port can run its whole export on every pass while the old site is still
live. When the old site edits the article, the next export carries the new
title, fields and `changed`; the import adds one revision dated that
`changed`, and reports `1 updated`. Give that revision a new `revision_id`
(or none, and the kernel picks one): a stored revision is never rewritten.

### When a record is refused

Every record is checked before it is written: the content type exists, the
author exists, the title is there, each field is a shape its type reads, a
text format is one the site has, required fields are filled, dates are in order. One bad
record fails the whole file, with its line number, and nothing is kept:

```
$ trovato import archive.ndjson
Error: line 1: Body: you may not use the full_html_legacy text format. (nothing was imported)
```

Fix the port (or the old data), and run it again.
