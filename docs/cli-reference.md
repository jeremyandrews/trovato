# Command line reference

Every command reads the same settings as the server (environment, or `.env` in
the working directory); `DATABASE_URL` is always required. `trovato --help`
and `trovato <command> --help` print the same, from the binary.

| Command | What it does |
|---|---|
| `trovato` / `trovato serve` | Start the HTTP server. |
| `trovato migrate` | Apply pending kernel and plugin migrations. |
| `trovato plugin new <name>` | Scaffold a plugin under `plugins/`. |
| `trovato plugin list` | List discovered plugins and their status. |
| `trovato plugin install <name>` | Run a plugin's migrations and enable it. |
| `trovato plugin migrate [<name>]` | Run pending migrations for one plugin, or all. |
| `trovato plugin enable <name>` / `disable <name>` | Change a plugin's stored status (servers apply it on restart). |
| `trovato config export [<dir>] [--clean]` | Export configuration to YAML. |
| `trovato config import [<dir>] [--dry-run]` | Import configuration from YAML. |
| `trovato user create <user> --mail <address> [--role <role>]... [--blocked] [--no-password]` | Create an account, as the admin "Add user" form does; the password is read from stdin. |
| `trovato user reset-password <user> [<password>]` | Set a password (read from stdin if omitted) and end the user's sessions. |
| `trovato user role-add <user> <role>` / `role-remove <user> <role>` / `roles <user>` | Manage a user's roles. |
| `trovato cache clear [--kind <kind>]...` | Tell every running server to drop its in-memory caches (all kinds, or the ones named). |
| `trovato item revision-delete <item> <revision>` | Delete a past revision of an item. |
| `trovato import <file> [--dry-run]` | Import content from another site (items and their revisions), keeping each record's id and dates. |
| `trovato generate --items <N> --admin-password <PASSWORD>` | Write a deterministic test site of a given size (for performance tests and benchmarks). |

## `trovato migrate`

```
trovato migrate [--status | --dry-run] [--lock-timeout <SECONDS>]
```

Applies every pending kernel migration, then every pending migration of the
plugins the server would run: those enabled, plus new plugins it would
auto-enable. It prints `Applied <k> kernel and <p> plugin migration(s).` and
exits 0, or exits nonzero on any failure.

- **Safe from several places at once.** It takes one Postgres advisory lock for
  the whole run. A second run waits, then finds nothing to do. Plugin
  migrations run by `plugin install`, `plugin migrate`, `config import` and the
  server take the same lock.
- **Waiting holds nothing.** A second run polls for the lock rather than
  blocking on it, so it holds no snapshot that a `CREATE INDEX CONCURRENTLY`
  in the first run would wait for.
- **Idempotent.** Nothing pending: it does nothing and exits 0.
- `--status`: lists applied, pending and unknown migrations per kernel and
  plugin, and ends with `Pending: <n>`. Unknown ones were applied by a newer
  release. Takes no lock, writes nothing.
- `--dry-run`: lists what would be applied. Takes no lock, writes nothing.
- `--lock-timeout <SECONDS>` (default 10; 0 waits forever): how long one
  statement may wait for a table lock before the migration rolls back and the
  command fails. It does not apply to the wait for another run, which polls
  the lock. A kernel half that includes a
  `-- no-transaction` migration (a concurrent index build) runs without it,
  and it is put back for the plugin migrations.
- It refuses when a migration is marked failed (dirty), was edited after it
  was applied, or was applied by a newer release and named incompatible. These
  are checked before anything is applied, so a refusal changes nothing. (On a
  fresh database, a plugin that becomes eligible only once the kernel
  migrations have created `plugin_status` is checked just before its own
  migrations run.)

It needs Postgres only, not Redis.

## `MIGRATE_ON_START`

With `MIGRATE_ON_START=true` (the default), `serve` applies pending kernel and
plugin migrations when it starts, and the `plugin`, `config` and `user`
commands that use the database apply pending kernel migrations, as they always
have. With `false`, `serve` never changes the schema: it waits, answering
`/health/live` 200 and `/health/ready` 503, until `trovato migrate` has run. The
other commands refuse to run on a kernel schema that is behind, and say so.
Once it is current, `plugin install`, `plugin migrate` and `config import`
apply the plugin migrations they need, under the lock.

With `MIGRATE_ON_START=false`, enabling a plugin whose migrations never ran
(`plugin enable`, or the admin UI) leaves every server that restarts
afterwards not ready until `trovato migrate` runs. Run it right after the
enable.

See [docs/deployment/upgrades.md](deployment/upgrades.md).

## `trovato user create`

```
trovato user create <user> --mail <address> [--role <role>]... [--blocked] [--no-password]
```

Creates an account with the checks the admin "Add user" form makes, and writes
it the way that form does: through the site's user service, so the enabled
plugins see `tap_user_register` and the audit trail records the account, and
each `--role` grant is audited as the form's role checkboxes are. The command
builds the full server state to do that, the way `serve` starts, so it takes as
long as a server's start.

The password is read from one line of stdin, never from the command line:

```sh
printf '%s\n' "$NEW_PASSWORD" | trovato user create ada --mail ada@example.com --role editor
```

It must be 12 to 128 characters, as on the form. With `--no-password` nothing
is read and the account has no password: it cannot sign in until
`trovato user reset-password <user>` or a password reset sets one.
`--blocked` creates the account blocked. The command never creates a superuser.

At a terminal the password is not shown as it is typed.

`--mail` is required and may not be empty, as on the form. The command exits 1
and creates nothing when the name is empty, the address is missing or empty,
either already belongs to an account (compared case-insensitively), the
password is too short or too long, or a role does not exist; it prints every
problem at once, in the form's words. Leaving out the name altogether is a usage
error, which exits 2 as every command's usage errors do. An access rule blocking
the name or address is a warning, as on the form, not a refusal.

The account is written before its roles. If a role cannot be granted after all
(it was deleted in between, or the database refused), the command still grants
the others, then exits 1, saying the account exists and printing the
`trovato user role-add` command for each role it is missing. Running
`user create` again would only be refused, since the name is now taken.

## `trovato cache clear`

```
trovato cache clear [--kind <kind>]...
```

Tells every running server to drop its in-memory caches, so a change written
straight to the database (SQL, a restore, a script) is served on the next
request instead of after the cache's time limit or a restart. With no `--kind`
it clears every kind; `--kind` may be given more than once. The kinds are the
fields of the Redis hash `cache:gen`: `permissions`, `users`, `items`,
`field_access`, `content_types`, `categories`, `gather`, `redirects`,
`locale`, `menus`, `text_formats`, `themes` and `site_config`. Clearing
`site_config` also drops the search settings' copy in Redis.

It adds one to each kind's counter in `cache:gen`; every server reads the hash
every 250 ms and clears the caches whose counter changed
([multi-server](deployment/multi-server.md#how-a-change-on-one-server-reaches-the-others)).
The command then waits one second, four of those reads, before it returns, so
a request made after it reads the database. A server that cannot reach Redis
misses the bump and keeps its copies until their time limits.
It connects only to Redis (`DATABASE_URL` must still be set, as for every
command), and exits 1 when Redis cannot be reached. `config import`, `generate`
and the `plugin` and `user` commands that write end with the same clear, of the
kinds they may have written.

Which plugins run is not a cache: enabling or disabling one still takes a
restart.

## `trovato generate`

```
trovato generate --items <N> [--comments <N>] [--tags <N>] [--users <N>]
                 [--seed <N>] [--batch <ROWS>] [--parallel <N>]
                 --admin-password <PASSWORD>
```

Writes a deterministic site into `DATABASE_URL` for performance tests and
benchmarks. It only writes into an empty database (it runs the migrations itself): one with any
content it did not write (a real site, or a run that did not finish) is
refused. The same arguments always write the same rows: ids, titles, bodies,
dates, authors and tags all come from the seed and the row number. It writes:

- members `gen_user_1` to `gen_user_<users>` (default 1000) with addresses
  under `example.invalid`;
- an administrator, `gen_admin`;
- `--tags` terms in the `tags` vocabulary (default 500, an even number, slugs
  `topic-<n>`), two to a post;
- `--items` published blog posts with one revision each;
- `--comments` comments, spread over the newest posts first (default one per
  post).

It then sets the front page to `/blog` and analyses the tables. Every
generated account has the `--admin-password` password, at least 12 characters.
There is no default, so no generated site has a known administrator login. The
password hash is salted, so it is the one value that differs between two runs
with the same arguments.

The work is done in Postgres: one `INSERT ... SELECT ... FROM generate_series`
statement per `--batch` rows (default 20,000), `--parallel` statements at a
time (default 16). Running it again with the same site arguments does nothing,
whatever `--batch` and `--parallel` say. Running it with different ones on a
generated database is refused.

Comment text is not added to search. `--items 20000000`, with the default
20,000,000 comments, takes about half an hour on a Mac Studio.

The blog plugin must be on the plugin path: the posts are its `blog` type.

## `trovato item revision-delete`

```
trovato item revision-delete <item> <revision>
```

Deletes one past revision of an item — the same operation the revision
history page's Delete link reaches, through the same `ItemService::delete_revision`
(`docs/admin-permissions.md`, "Revisions"). Runs as an administrator, like
every other command on this page: whoever can run it already has the
database, so it needs no `delete revisions` permission and no confirmation.

The item's current revision is never deleted, here or anywhere else: it is
what the item row points at, and the foreign key from `item.current_revision_id`
to `item_revision.id` refuses the delete even if this check did not. Exits
nonzero, with no row removed, for the current revision, a revision id that is
not this item's, or one that does not exist.

Bumps the `Items` cache generation, the same as any other write to an item or
its revisions, so a running server's cached copy of the item drops.

## `trovato import`

```
trovato import <file> [--dry-run]
```

Imports content from another site, keeping each record's id and its created
and changed dates: what a site migration writes instead of SQL. The file is
newline delimited JSON, one record per line, each with a `type`, a caller
chosen `id` (a UUID) and that type's keys; `-` reads stdin. This release
imports `item` and `revision` records. The format, every key and every rule
are in [`docs/design/import.md`](design/import.md); a worked example is in
[`docs/migrating-a-site.md`](migrating-a-site.md).

- **One transaction.** Every record is checked as the forms check it (as an
  administrator: every text format is allowed) and written in turn; the file
  commits only after its last line passed. A record that fails is printed as
  `line <n>: <what is wrong> (nothing was imported)`, the command exits
  nonzero, and nothing from the file is kept.
- **Upsert by id.** A record identical to what is stored writes nothing, so
  running the same file again reports 0 changes. A changed `item` gains one
  revision, dated the record's `changed`. A stored revision is never
  rewritten: a `revision` record whose id is stored with other content fails
  the import. Records absent from the file are left alone.
- **No restart.** After a commit that changed anything, every cache kind is
  bumped once, as `trovato cache clear` does, so running servers serve the
  imported content on their next request. The search vectors and the static
  search index queue are kept current by the database's own triggers.
- **Not a form save.** No plugin taps fire (no mail to subscribers, no presave
  rewriting), no pathauto alias is made, and no moderation transition is
  asked: the file says what is stored.
- `--dry-run` checks every record and prints what would change, and writes
  nothing.

Runs as the operator, like every command on this page; no web route, API or
plugin host function can choose an id or a date.

```
$ trovato import site.ndjson
Imported 150 records from site.ndjson:
  item: 50 created, 0 updated, 0 unchanged
  revision: 100 created, 0 updated, 0 unchanged
150 change(s).
Running servers have dropped their cached copies; the next request reads the database.
```
