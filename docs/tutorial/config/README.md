# Ritrovo config set

The config set the tutorial imports:

```bash
cargo run --release --bin trovato -- config import docs/tutorial/config
```

Import validates the whole set before it writes anything: if any file fails to
parse or fails its schema check, the run names every offending file, exits
non-zero, and writes nothing. `--dry-run` runs the same validation without
writing, so it is a real preflight.

The Italian seed content in `seed-italian/` is a separate set, imported on its
own in Part 7. `locale/` holds a `.po` file, not config.

## Filenames

Every file is named `{entity_type}.{id}.yml`, where the ID is the entity's own
identifier — the same name `config export` writes, so this directory is what an
export of a configured Ritrovo produces, filename for filename. For the entity
types keyed by UUID that means the filename is a UUID; the first line of each file
says which entity it is, and the index below maps them.

Re-exporting after an import reproduces these files, with one exception: a stage's
`created` and `changed` are assigned by the database when the stage row is
created, so those two fields come back as the real timestamps rather than the
placeholder in the file.

The tutorial's hand-authored entities use UUIDs in the `0193a5a0-` family so they
are stable across installs and legible in a database dump.

## Index

### Stages (`0193a5a0-0000-…`)

| Machine name | UUID | Notes |
|---|---|---|
| `live` | `…-000000000001` | Public. Created by the installer; importing updates it. |
| `incoming` | `…-000000000002` | Internal. Where the importer lands new conferences. |
| `curated` | `…-000000000003` | Internal. Reviewed, awaiting publication. |
| `legal_review` | `…-000000000004` | Internal. Part 4's extensibility demo. |

### Roles (`0193a5a0-0002-…`)

| Name | UUID |
|---|---|
| `viewer` | `…-000000000001` |
| `editor` | `…-000000000002` |
| `publisher` | `…-000000000003` |

Import applies the roles themselves. It does not apply permissions: the `role`
config entity does not carry them. Each role file lists the permissions it is
meant to hold; assign them at `/admin/people/permissions` or with the SQL in
`../recipes/recipe-part-04.md`.

### Tiles (`0193a5a0-0003-…`)

| Machine name | UUID | Region |
|---|---|---|
| `conferences_this_month` | `…-000000000001` | sidebar |
| `open_cfps_sidebar` | `…-000000000002` | sidebar |
| `topic_cloud` | `…-000000000003` | sidebar |
| `search_box` | `…-000000000004` | header |
| `footer_info` | `…-000000000005` | footer |

### Menu links (`0193a5a0-0004-…`)

| Menu | Title | Path | UUID |
|---|---|---|---|
| main | Conferences | `/conferences` | `…-000000000001` |
| main | Speakers | `/speakers` | `…-000000000002` |
| main | Call for Papers | `/open-cfps` | `…-000000000003` |
| main | Topics | `/topics` | `…-000000000004` |
| footer | About | `/about` | `…-000000000005` |
| footer | Contact | `/contact` | `…-000000000006` |

### Everything else

Named by its own identifier, so it reads directly: `item_type.conference.yml`,
`category.topics.yml`, `language.it.yml`, `variable.pathauto_patterns.yml`,
`gather_query.ritrovo.open_cfps.yml`. Tags, URL aliases and search field configs
are UUID-keyed and generated, so their filenames are UUIDs with no index here.
