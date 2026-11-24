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
| `trovato user reset-password <user> [<password>]` | Set a password (read from stdin if omitted) and end the user's sessions. |
| `trovato user role-add <user> <role>` / `role-remove <user> <role>` / `roles <user>` | Manage a user's roles. |
| `trovato generate --items <N>` | Write a deterministic test site of a given size (for performance tests and benchmarks). |

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

## `trovato generate`

```
trovato generate --items <N> [--comments <N>] [--tags <N>] [--users <N>]
                 [--seed <N>] [--batch <ROWS>] [--parallel <N>]
                 [--admin-password <PASSWORD>]
```

Writes a deterministic site into `DATABASE_URL` for performance tests and
benchmarks. Never run it against a site with real content. The same
arguments always write the same rows: ids, titles, bodies, dates, authors and
tags all come from the seed and the row number. It writes:

- members `gen_user_1` to `gen_user_<users>` (default 1000) with addresses
  under `example.invalid`;
- an administrator, `gen_admin`;
- `--tags` terms in the `tags` vocabulary (default 500, slugs `topic-<n>`);
- `--items` published blog posts with one revision each;
- `--comments` comments, spread over the newest posts first (default one per
  post).

It then sets the front page to `/blog` and analyses the tables. Every
generated account has the `--admin-password` password (default
`generated-admin-password`).

The work is done in Postgres: one `INSERT ... SELECT ... FROM generate_series`
statement per `--batch` rows (default 20,000), `--parallel` statements at a
time (default 16). Running it again with the same arguments does nothing.
Running it with different arguments on a generated database is refused.

Comment text is not added to search. With `--items 20000000`, the default
20,000,000 comments, 500 tags and 1,000 members took 55 minutes on a Mac
Studio, Postgres 16 in Docker.

The blog plugin must be on the plugin path: the posts are its `blog` type.
