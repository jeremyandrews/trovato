# A config set the kernel owns

Thirteen files that exist to be imported by `config_import_test.rs`, and by the
`include_str!` in `config_storage/yaml.rs`'s unit tests. Nothing ships them and no
tutorial reads them: they are test data.

## Why this exists

These tests used to import `docs/tutorial/config/`, which is the tutorial's
content and, until recently, Ritrovo's content model as well. That set has moved
to the Ritrovo repository, so the kernel's tests would have been asserting on
files another repository is free to change. A test that fails because a downstream
project renamed a field is a test about the wrong thing.

So the kernel keeps its own, here. `docs/tutorial/config/` stays where it is,
because the released image ships it and the tutorial still imports it.

## What it covers, and why each file is in it

It is small on purpose, but not arbitrary: every file is here because some
behaviour of the importer needs it.

| File(s) | What it pins |
| :- | :- |
| `item_type.workshop.yml` | Field definitions round-tripping through YAML: `Text`, `Date`, `Boolean`, `Blocks`, `required`, `title_label`. The `yaml.rs` unit test reads this file directly |
| `category.subjects.yml`, `tag.*.yml` | A tag resolving its category, and a tag resolving another tag as its parent — the two references import validates inside the set before it looks at the database |
| `search_field_config.*.yml` | A `bundle` resolving to an item type in the same set. This is the reference that makes the set indivisible: it cannot be imported without the type |
| `menu_link.*.yml` | A menu link resolving another as its parent, and the cycle check that walks up the chain |
| `role.*.yml` | Permissions being applied by import, which is the only management path roles have |
| `stage.*.yml` | Stages landing under the UUID their file declares, not one the kernel invents. This is what made a second import collide on `machine_name` |
| `tile.*.yml`, `url_alias.*.yml`, `variable.*.yml` | Entity types that were silently skipped once, so each is counted |

## Rules for changing it

The count test reads the directory, so adding a file needs no edit there. The
role, stage, tile and menu-link assertions name their contents, so changing those
means changing the test in the same commit.

Keep it minimal. It is not a demonstration of what Trovato can model: that is what
`docs/tutorial/` and the Ritrovo repository are for.
