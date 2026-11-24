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
- **Idempotent.** Nothing pending: it does nothing and exits 0.
- `--status`: lists applied, pending and unknown migrations per kernel and
  plugin, and ends with `Pending: <n>`. Unknown ones were applied by a newer
  release. Takes no lock, writes nothing.
- `--dry-run`: lists what would be applied. Takes no lock, writes nothing.
- `--lock-timeout <SECONDS>` (default 10; 0 waits forever): how long one
  statement may wait for a table lock before the migration rolls back and the
  command fails. It is set after the advisory lock is taken, so it never makes a
  second run fail instead of wait.
- It refuses when a migration is marked failed (dirty), was edited after it
  was applied, or was applied by a newer release and named incompatible. The
  kernel half is checked before any kernel migration runs, the plugin half
  before any plugin migration runs, so a plugin problem is found after the
  kernel migrations have been applied.

It needs Postgres only, not Redis.

## `MIGRATE_ON_START`

With `MIGRATE_ON_START=true` (the default), `serve` and the `plugin`, `config`
and `user` commands apply pending migrations when they start, as they always
have. With `false`, `serve` never changes the schema: it waits, answering
`/health/live` 200 and `/health/ready` 503, until `trovato migrate` has run. The
other commands refuse to run on a kernel schema that is behind, and say so;
`plugin install`, `plugin migrate` and `config import` still apply the plugin
migrations they need, under the lock.

See [docs/deployment/upgrades.md](deployment/upgrades.md).
