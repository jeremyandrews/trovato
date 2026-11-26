#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Integration tests for `config import`.
//!
//! Two things are under test here, and they are the two halves of the same
//! defect: import used to apply whatever parsed and report success, so a config
//! set containing one bad file produced a partial import with exit code 0 — and
//! 18 of the 76 files in the tutorial's own config set were bad files.
//!
//! Every test runs against its own scratch database (see [`ScratchDb`]) rather
//! than the shared fixture, because these tests import a whole config set and
//! assert on row counts; sharing a database with the rest of the suite would
//! make both flaky.

use std::path::{Path, PathBuf};

use sqlx::PgPool;
use trovato_kernel::config_storage::yaml::{
    ConfigImportFailed, export_config, import_config, import_config_with_env,
};
use trovato_kernel::config_storage::{
    ConfigPluginStatus, ConfigStorage, DirectConfigStorage, PluginConfigSet, entity_types,
    plugin_config,
};

/// A database created for one test and dropped when the test ends.
///
/// "Imports clean against a fresh database" is the claim under test, so the test
/// gets an actually fresh database: created, migrated, used, dropped. The
/// create-and-drop half is `trovato_test_utils::ScratchDb`, which drops the
/// database in `Drop` rather than on the last line of the test body — this file
/// used to leave one behind on the server for every test that failed part way.
struct ScratchDb(trovato_test_utils::ScratchDb);

impl ScratchDb {
    async fn new(label: &str) -> Self {
        let db = trovato_test_utils::ScratchDb::create(&format!("cfgimport_{label}")).await;
        trovato_kernel::db::run_migrations(db.pool())
            .await
            .expect("failed to migrate the scratch database");
        Self(db)
    }

    fn pool(&self) -> &PgPool {
        self.0.pool()
    }

    fn storage(&self) -> DirectConfigStorage {
        DirectConfigStorage::new(self.pool().clone())
    }

    /// Storage that also knows the repository's plugins, so their config
    /// tables are entity types, as they are for `trovato config`.
    async fn plugin_storage(&self) -> DirectConfigStorage {
        let plugins_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        self.storage()
            .with_plugins(PluginConfigSet::discover(&[plugins_dir]).await)
    }

    /// This database's own connection URL, for the one test that drives the
    /// `trovato` binary as a subprocess.
    fn url(&self) -> String {
        self.0.url()
    }

    /// Drop the database at a chosen point. `Drop` does the same work for a
    /// test that fails before reaching it, so this is a convenience rather than
    /// the guarantee it used to be.
    async fn cleanup(self) {
        drop(self);
    }
}

/// Path to the config set the kernel owns for its own tests.
///
/// These tests used to import `docs/tutorial/config/`. That set carried the
/// tutorial's content and, with it, Ritrovo's content model; the model moved to
/// the Ritrovo repository (`demo/config/`, jeremyandrews/ritrovo#11), which would
/// have left the kernel asserting on files another project is free to change.
/// `docs/tutorial/config/` stays where it is, because the image ships it and the
/// tutorial imports it. See the fixture's README for what each file pins.
fn fixture_config_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config-set")
}

/// Directory for a test's hand-written config files, removed on drop.
struct TempConfigDir(PathBuf);

impl TempConfigDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "trovato_cfgimport_{label}_{}",
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn write(&self, filename: &str, contents: &str) {
        std::fs::write(self.0.join(filename), contents).unwrap();
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempConfigDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The reporting half of the defect: one unparseable file used to be a warning
/// on an otherwise successful run. Import must now fail, name the file, and
/// leave the database untouched — including the valid file sitting next to it.
#[tokio::test]
async fn import_fails_and_writes_nothing_when_one_file_does_not_parse() {
    let db = ScratchDb::new("badfile").await;
    let storage = db.storage();

    let dir = TempConfigDir::new("badfile");
    // Valid, and would import fine on its own.
    dir.write(
        "category.badfile_topics.yml",
        "id: badfile_topics\nlabel: Topics\ndescription: null\nhierarchy: 0\nweight: 0\n",
    );
    // Not valid YAML at all.
    dir.write("variable.badfile_broken.yml", "not: [valid: yaml: {}\n");

    let err = import_config(&storage, db.pool(), dir.path(), false)
        .await
        .expect_err("import must fail when a file in the set does not parse");

    let failed = err
        .downcast_ref::<ConfigImportFailed>()
        .expect("the error must be a ConfigImportFailed so callers can inspect it");

    assert_eq!(
        failed.imported_total(),
        0,
        "validation failed, so nothing should have been written: {failed}"
    );
    assert_eq!(failed.failures.len(), 1, "expected one failure: {failed}");
    assert_eq!(failed.failures[0].filename, "variable.badfile_broken.yml");
    assert!(
        err.to_string().contains("variable.badfile_broken.yml"),
        "the report must name the offending file, got: {err}"
    );

    // The valid sibling must not have landed: a config set is atomic on validation.
    let landed = storage
        .load(entity_types::CATEGORY, "badfile_topics")
        .await
        .expect("category lookup failed");
    assert!(
        landed.is_none(),
        "a valid file must not be applied when another file in the same set failed"
    );

    db.cleanup().await;
}

/// A schema mismatch is the same class of failure as malformed YAML, and it is
/// the one the tutorial config set actually hit: valid YAML that no longer
/// matches the entity's schema.
#[tokio::test]
async fn import_fails_on_schema_mismatch_not_just_malformed_yaml() {
    let db = ScratchDb::new("schema").await;
    let storage = db.storage();

    let dir = TempConfigDir::new("schema");
    // Valid YAML. Stage requires `id`, `machine_name`, `created` and `changed`.
    dir.write(
        "stage.schema_incoming.yml",
        "category_id: stages\nlabel: Incoming\nvisibility: internal\nis_default: true\nweight: 0\n",
    );

    let err = import_config(&storage, db.pool(), dir.path(), false)
        .await
        .expect_err("import must fail when a file does not match its entity schema");

    let failed = err.downcast_ref::<ConfigImportFailed>().unwrap();
    assert_eq!(failed.failures.len(), 1, "expected one failure: {failed}");
    assert_eq!(failed.failures[0].filename, "stage.schema_incoming.yml");
    assert!(
        failed.failures[0].error.contains("missing field"),
        "the report must say what is wrong with the file, got: {}",
        failed.failures[0].error
    );

    db.cleanup().await;
}

/// `--dry-run` is a preflight, so it has to fail on the same input the real run
/// fails on rather than reporting what it "would" import.
#[tokio::test]
async fn dry_run_fails_on_the_same_input_a_real_run_fails_on() {
    let db = ScratchDb::new("dryrun").await;
    let storage = db.storage();

    let dir = TempConfigDir::new("dryrun");
    dir.write("variable.dryrun_broken.yml", "not: [valid: yaml: {}\n");

    let err = import_config(&storage, db.pool(), dir.path(), true)
        .await
        .expect_err("a dry run must fail on a set that would fail");
    assert!(
        err.to_string().contains("variable.dryrun_broken.yml"),
        "got: {err}"
    );

    db.cleanup().await;
}

/// An unresolvable reference used to skip one entity with a warning and still
/// report success. It is now a validation failure, so nothing is written.
#[tokio::test]
async fn import_fails_when_a_reference_cannot_be_resolved() {
    let db = ScratchDb::new("refs").await;
    let storage = db.storage();

    let dir = TempConfigDir::new("refs");
    let tag_id = uuid::Uuid::now_v7();
    dir.write(
        &format!("tag.{tag_id}.yml"),
        &format!(
            "id: {tag_id}\ncategory_id: refs_nonexistent\nlabel: Orphan\ndescription: null\n\
             slug: orphan\nweight: 0\ncreated: 1767225600\nchanged: 1767225600\n"
        ),
    );

    let err = import_config(&storage, db.pool(), dir.path(), false)
        .await
        .expect_err("import must fail when a tag's category does not exist");

    let failed = err.downcast_ref::<ConfigImportFailed>().unwrap();
    assert_eq!(failed.imported_total(), 0, "nothing should be written");
    assert!(
        failed.failures[0].error.contains("refs_nonexistent"),
        "got: {}",
        failed.failures[0].error
    );

    db.cleanup().await;
}

/// The repair half: the tutorial's own config set must import clean against a
/// fresh database.
///
/// Roles and stages are asserted as rows on purpose. `KNOWN-ISSUES.md` records
/// that neither has an admin form, so `config import` is their only management
/// path — a silently skipped file there means an entity that never arrives and
/// nothing that says why.
#[tokio::test]
async fn the_fixture_config_set_imports_clean_on_a_fresh_database() {
    let db = ScratchDb::new("fixtureset").await;
    let storage = db.storage();
    let dir = fixture_config_dir();

    let expected_files = std::fs::read_dir(&dir)
        .expect("the fixture config directory must exist")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext == "yml" || ext == "yaml")
        })
        .count();

    let result = match import_config(&storage, db.pool(), &dir, false).await {
        Ok(result) => result,
        Err(e) => {
            db.cleanup().await;
            panic!("the fixture config set must import clean: {e}");
        }
    };

    assert!(
        result.warnings.is_empty(),
        "the fixture config set should import without warnings: {:?}",
        result.warnings
    );
    assert_eq!(
        result.total(),
        expected_files,
        "every file in the set should have been imported; counts: {:?}",
        result.counts
    );

    // Roles: import is their only management path.
    let roles: Vec<String> = sqlx::query_scalar("SELECT name FROM roles ORDER BY name")
        .fetch_all(db.pool())
        .await
        .expect("failed to list roles");
    for expected in ["fixture_reader", "fixture_writer"] {
        assert!(
            roles.iter().any(|r| r == expected),
            "role '{expected}' should have landed as a row, got: {roles:?}"
        );
    }

    // Stages: same, and under the UUIDs their files declare.
    let stages: Vec<(uuid::Uuid, String)> =
        sqlx::query_as("SELECT tag_id, machine_name FROM stage_config ORDER BY machine_name")
            .fetch_all(db.pool())
            .await
            .expect("failed to list stages");
    for (expected_id, expected_name) in [
        ("1f0d9b6a-0003-4000-8000-000000000001", "fixture_draft"),
        ("1f0d9b6a-0003-4000-8000-000000000002", "fixture_review"),
    ] {
        let expected_id: uuid::Uuid = expected_id.parse().unwrap();
        assert!(
            stages
                .iter()
                .any(|(id, name)| *id == expected_id && name == expected_name),
            "stage '{expected_name}' should have landed under {expected_id}, got: {stages:?}"
        );
    }

    // Tiles and menu links were in the failing set too.
    let tiles: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tile")
        .fetch_one(db.pool())
        .await
        .expect("failed to count tiles");
    assert_eq!(tiles, 1, "the fixture's tile should have landed");

    let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM menu_link")
        .fetch_one(db.pool())
        .await
        .expect("failed to count menu links");
    assert_eq!(links, 2, "both fixture menu links should have landed");

    db.cleanup().await;
}

/// Importing the set twice has to converge, not fail the second time.
///
/// This is what `Stage::create` generating its own UUID used to break: the first
/// run created stages under UUIDs the files did not declare, so the second run
/// did not find them and tried to create them again, colliding on the unique
/// `machine_name`.
#[tokio::test]
async fn the_fixture_config_set_is_idempotent() {
    let db = ScratchDb::new("idempotent").await;
    let storage = db.storage();
    let dir = fixture_config_dir();

    for run in 1..=2 {
        if let Err(e) = import_config(&storage, db.pool(), &dir, false).await {
            db.cleanup().await;
            panic!("import run {run} of the fixture config set failed: {e}");
        }
    }

    // The fixture's own stages only. A fresh database already carries the
    // kernel's seeded public stage, and counting that in would make this
    // assertion about the migrations rather than about importing twice.
    let stages: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM stage_config \
         WHERE machine_name IN ('fixture_draft', 'fixture_review')",
    )
    .fetch_one(db.pool())
    .await
    .expect("failed to count stages");
    assert_eq!(stages, 2, "a second import must not duplicate stages");

    let roles: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM roles WHERE name IN ('fixture_reader', 'fixture_writer')",
    )
    .fetch_one(db.pool())
    .await
    .expect("failed to count roles");
    assert_eq!(roles, 2, "a second import must not duplicate roles");

    db.cleanup().await;
}

/// The operator-facing half of the reporting fix: the CLI itself must exit
/// non-zero and print the offending filename. Asserted through the real binary
/// because the exit code is what an operator and a deploy script react to.
#[tokio::test]
async fn config_import_cli_exits_non_zero_and_names_the_bad_file() {
    let db = ScratchDb::new("cli").await;

    let dir = TempConfigDir::new("cli");
    dir.write(
        "category.cli_topics.yml",
        "id: cli_topics\nlabel: Topics\ndescription: null\nhierarchy: 0\nweight: 0\n",
    );
    dir.write("variable.cli_broken.yml", "not: [valid: yaml: {}\n");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_trovato"))
        .args(["config", "import"])
        .arg(dir.path())
        .env("DATABASE_URL", db.url())
        .output()
        .expect("failed to run the trovato binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        !output.status.success(),
        "config import must exit non-zero when a file does not parse.\n{combined}"
    );
    assert!(
        combined.contains("variable.cli_broken.yml"),
        "the output must name the offending file.\n{combined}"
    );

    db.cleanup().await;
}

// =============================================================================
// Menu link and tile round trip (hierarchy, visibility, ownership, stage)
// =============================================================================
//
// `menu_link` and `tile` both carry columns their config files must declare to
// parse, and the storage layer's insert used to bind only some of them: a menu
// link's `parent_id`, `hidden` and `plugin` and both types' `stage_id` were
// dropped, so the row took the column default. Composed navigation was therefore
// unbuildable by the only supported path, and a tile or link could not be
// imported onto a non-Live stage at all.

/// The non-Live stage these tests import onto. Declared as a config file rather
/// than inserted by hand so the fixture exercises the same path a site would.
const OTHER_STAGE_ID: &str = "0193a5a0-0000-7000-8000-0000000000aa";

/// A stage config file for [`OTHER_STAGE_ID`].
fn other_stage_yaml() -> String {
    format!(
        "id: {OTHER_STAGE_ID}\nmachine_name: roundtrip_review\nlabel: Roundtrip Review\n\
         visibility: internal\nis_default: false\nweight: 0\n\
         created: 1767225600\nchanged: 1767225600\n"
    )
}

/// One menu link config file.
#[allow(clippy::too_many_arguments)]
fn menu_link_yaml(
    id: &str,
    menu_name: &str,
    path: &str,
    title: &str,
    parent_id: Option<&str>,
    weight: i32,
    hidden: bool,
    plugin: &str,
    stage_id: &str,
) -> String {
    let parent = match parent_id {
        Some(p) => format!("parent_id: {p}\n"),
        None => "parent_id: null\n".to_string(),
    };
    format!(
        "id: {id}\nmenu_name: {menu_name}\npath: {path}\ntitle: {title}\n{parent}\
         weight: {weight}\nhidden: {hidden}\nplugin: {plugin}\nstage_id: {stage_id}\n\
         created: 1767225600\nchanged: 1767225600\n"
    )
}

/// Write the three-level fixture tree into `dir` and return the ids, root first.
///
/// Deliberately written so that filename order does *not* match tree order: the
/// grandchild sorts before the root, so an import that saves in filename order
/// hits the `parent_id` foreign key before the parent row exists.
fn write_menu_tree(dir: &TempConfigDir) -> (String, String, String) {
    let root = "0193a5a0-0004-7000-8000-0000000000c1".to_string();
    let child = "0193a5a0-0004-7000-8000-0000000000b1".to_string();
    let grandchild = "0193a5a0-0004-7000-8000-0000000000a1".to_string();

    dir.write(&format!("stage.{OTHER_STAGE_ID}.yml"), &other_stage_yaml());
    dir.write(
        &format!("menu_link.{root}.yml"),
        &menu_link_yaml(
            &root,
            "roundtrip",
            "/rt/docs",
            "Docs",
            None,
            0,
            false,
            "core",
            LIVE_STAGE_ID_STR,
        ),
    );
    dir.write(
        &format!("menu_link.{child}.yml"),
        &menu_link_yaml(
            &child,
            "roundtrip",
            "/rt/docs/guide",
            "Guide",
            Some(&root),
            5,
            true,
            "trovato_blog",
            LIVE_STAGE_ID_STR,
        ),
    );
    dir.write(
        &format!("menu_link.{grandchild}.yml"),
        &menu_link_yaml(
            &grandchild,
            "roundtrip",
            "/rt/docs/guide/install",
            "Install",
            Some(&child),
            10,
            false,
            "core",
            OTHER_STAGE_ID,
        ),
    );

    (root, child, grandchild)
}

/// The Live stage UUID, as the seeded row carries it.
const LIVE_STAGE_ID_STR: &str = "0193a5a0-0000-7000-8000-000000000001";

/// A menu link's hierarchy, visibility, ownership and stage survive an import.
///
/// Before the fix the insert bound only `id`, `menu_name`, `path`, `title`,
/// `weight`, `created` and `changed`, so all four of the assertions below read
/// the column default instead of the file's value.
#[tokio::test]
async fn menu_link_import_binds_parent_hidden_plugin_and_stage() {
    let db = ScratchDb::new("menutree").await;
    let storage = db.storage();

    let dir = TempConfigDir::new("menutree");
    let (root, child, grandchild) = write_menu_tree(&dir);

    if let Err(e) = import_config(&storage, db.pool(), dir.path(), false).await {
        db.cleanup().await;
        panic!("the menu tree must import clean: {e:#}");
    }

    let rows: Vec<(uuid::Uuid, Option<uuid::Uuid>, bool, String, uuid::Uuid)> = sqlx::query_as(
        "SELECT id, parent_id, hidden, plugin, stage_id FROM menu_link \
         WHERE menu_name = 'roundtrip' ORDER BY weight",
    )
    .fetch_all(db.pool())
    .await
    .expect("failed to read imported menu links");

    let root_id: uuid::Uuid = root.parse().unwrap();
    let child_id: uuid::Uuid = child.parse().unwrap();
    let grandchild_id: uuid::Uuid = grandchild.parse().unwrap();
    let live: uuid::Uuid = LIVE_STAGE_ID_STR.parse().unwrap();
    let other: uuid::Uuid = OTHER_STAGE_ID.parse().unwrap();

    let expected = vec![
        (root_id, None, false, "core".to_string(), live),
        (
            child_id,
            Some(root_id),
            true,
            "trovato_blog".to_string(),
            live,
        ),
        (
            grandchild_id,
            Some(child_id),
            false,
            "core".to_string(),
            other,
        ),
    ];

    assert_eq!(
        rows, expected,
        "parent_id, hidden, plugin and stage_id must all come from the config file"
    );

    db.cleanup().await;
}

/// A tile lands on the stage its file declares, not on Live.
#[tokio::test]
async fn tile_import_lands_on_the_declared_stage() {
    let db = ScratchDb::new("tilestage").await;
    let storage = db.storage();

    let dir = TempConfigDir::new("tilestage");
    dir.write(&format!("stage.{OTHER_STAGE_ID}.yml"), &other_stage_yaml());
    let tile_id = "0193a5a0-0003-7000-8000-0000000000a1";
    dir.write(
        &format!("tile.{tile_id}.yml"),
        &format!(
            "id: {tile_id}\nmachine_name: roundtrip_tile\nlabel: Roundtrip Tile\nregion: sidebar\n\
             tile_type: custom\nconfig: {{}}\nvisibility: {{}}\nweight: 0\nstatus: 1\n\
             plugin: core\nstage_id: {OTHER_STAGE_ID}\ncreated: 1767225600\nchanged: 1767225600\n"
        ),
    );

    if let Err(e) = import_config(&storage, db.pool(), dir.path(), false).await {
        db.cleanup().await;
        panic!("the tile must import clean: {e:#}");
    }

    let stage_id: uuid::Uuid =
        sqlx::query_scalar("SELECT stage_id FROM tile WHERE machine_name = 'roundtrip_tile'")
            .fetch_one(db.pool())
            .await
            .expect("failed to read the imported tile");

    assert_eq!(
        stage_id,
        OTHER_STAGE_ID.parse::<uuid::Uuid>().unwrap(),
        "a tile must land on the stage its config file declares"
    );

    db.cleanup().await;
}

/// Whether an exported YAML document declares `field` with `value`.
///
/// serde_yml quotes a scalar that would otherwise be ambiguous, so a UUID comes
/// back as `parent_id: '0193...'` and a bare `contains` on the unquoted form
/// misses it. Both forms mean the same thing to the parser, so both count.
fn yaml_declares(document: &str, field: &str, value: &str) -> bool {
    document.lines().any(|line| {
        let line = line.trim();
        line == format!("{field}: {value}")
            || line == format!("{field}: '{value}'")
            || line == format!("{field}: \"{value}\"")
    })
}

/// Export reproduces what was imported, and re-importing the export reproduces
/// the same database: the full round trip, asserted on the exported bytes.
///
/// Comparing exports rather than rows is deliberate. A row comparison would pass
/// if export dropped a field that import also dropped; comparing the two
/// exported documents catches a field that survives neither direction only if it
/// is also absent from the file, which the assertion on the first export's
/// content covers.
///
/// The set also carries plugin configuration: the six plugins that own config
/// tables are enabled by `plugin_status` files, and each table has a row. The
/// whole second export must equal the first, byte for byte, which also proves
/// that a fresh install's seeded rows (the image styles) give way to the
/// exported ones, and that runtime state never enters the set.
#[tokio::test]
async fn the_menu_tree_survives_export_and_re_import() {
    let first = ScratchDb::new("rtfirst").await;
    let second = ScratchDb::new("rtsecond").await;
    let first_storage = first.plugin_storage().await;
    let second_storage = second.plugin_storage().await;
    // The second database stands for a fresh install, which has booted with
    // these plugins enabled: their migrations have seeded image styles and
    // roles under ids of their own, which the imported ones must replace.
    for plugin in PLUGINS_WITH_CONFIG {
        plugin_config::save_status(
            second.pool(),
            &ConfigPluginStatus {
                name: plugin.to_string(),
                enabled: true,
            },
            "0",
        )
        .await
        .unwrap();
    }
    second_storage
        .plugins()
        .unwrap()
        .migrate_enabled(second.pool())
        .await
        .unwrap();

    let source = TempConfigDir::new("rtsource");
    let (root, child, grandchild) = write_menu_tree(&source);
    let plugin_files = write_plugin_config(&source);
    // Runtime state, which import must ignore.
    source.write(
        "variable.update_status.yml",
        "key: update_status\nvalue: {latest: 9.9.9}\n",
    );

    let env = round_trip_secrets;
    if let Err(e) =
        import_config_with_env(&first_storage, first.pool(), source.path(), false, &env).await
    {
        first.cleanup().await;
        second.cleanup().await;
        panic!("the menu tree must import clean: {e:#}");
    }

    // Export everything the first database holds.
    let export_one = TempConfigDir::new("rtexport1");
    if let Err(e) = export_config(&first_storage, first.pool(), export_one.path(), false).await {
        first.cleanup().await;
        second.cleanup().await;
        panic!("export must succeed: {e:#}");
    }

    // The export must carry the hierarchy, not just the flat fields.
    let exported_child =
        std::fs::read_to_string(export_one.path().join(format!("menu_link.{child}.yml")))
            .expect("the child link must be exported");
    assert!(
        yaml_declares(&exported_child, "parent_id", &root),
        "the export must name the parent link, got:\n{exported_child}"
    );
    assert!(
        yaml_declares(&exported_child, "hidden", "true"),
        "the export must carry the hidden flag, got:\n{exported_child}"
    );
    assert!(
        yaml_declares(&exported_child, "plugin", "trovato_blog"),
        "the export must carry plugin ownership, got:\n{exported_child}"
    );
    let exported_grandchild = std::fs::read_to_string(
        export_one
            .path()
            .join(format!("menu_link.{grandchild}.yml")),
    )
    .expect("the grandchild link must be exported");
    assert!(
        yaml_declares(&exported_grandchild, "stage_id", OTHER_STAGE_ID),
        "the export must carry the non-Live stage, got:\n{exported_grandchild}"
    );

    // Re-import that export into a second, independent database.
    if let Err(e) = import_config_with_env(
        &second_storage,
        second.pool(),
        export_one.path(),
        false,
        &env,
    )
    .await
    {
        first.cleanup().await;
        second.cleanup().await;
        panic!("re-importing an export must succeed: {e:#}");
    }

    let export_two = TempConfigDir::new("rtexport2");
    if let Err(e) = export_config(&second_storage, second.pool(), export_two.path(), false).await {
        first.cleanup().await;
        second.cleanup().await;
        panic!("the second export must succeed: {e:#}");
    }

    // Byte-for-byte on every menu link and tile file in the set.
    let mut compared = 0usize;
    for id in [&root, &child, &grandchild] {
        let name = format!("menu_link.{id}.yml");
        let a = std::fs::read(export_one.path().join(&name)).expect("first export missing a link");
        let b = std::fs::read(export_two.path().join(&name)).expect("second export missing a link");
        assert_eq!(
            String::from_utf8_lossy(&a),
            String::from_utf8_lossy(&b),
            "{name} did not survive the round trip"
        );
        compared += 1;
    }
    assert_eq!(compared, 3, "all three links must have been compared");

    // Every plugin entity the source set carried is in the export, with what
    // the source said.
    for (name, field, value) in &plugin_files {
        let exported = std::fs::read_to_string(export_one.path().join(name))
            .unwrap_or_else(|_| panic!("{name} must be exported"));
        assert!(
            yaml_declares(&exported, field, value),
            "{name} must carry {field}: {value}, got:\n{exported}"
        );
    }
    // Runtime state is neither imported nor exported.
    for key in trovato_kernel::config_storage::RUNTIME_STATE_KEYS {
        assert!(
            !export_one
                .path()
                .join(format!("variable.{key}.yml"))
                .exists(),
            "runtime state '{key}' must not be exported"
        );
    }
    let update_status: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT value FROM site_config WHERE key = 'update_status'")
            .fetch_optional(first.pool())
            .await
            .unwrap();
    assert!(
        update_status.is_none(),
        "importing runtime state must not write it, got {update_status:?}"
    );

    // The whole second export equals the first.
    let listing = |dir: &Path| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let names = listing(export_one.path());
    assert_eq!(
        names,
        listing(export_two.path()),
        "both exports must hold the same files"
    );
    for name in &names {
        let a = std::fs::read(export_one.path().join(name)).unwrap();
        let b = std::fs::read(export_two.path().join(name)).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&a),
            String::from_utf8_lossy(&b),
            "{name} did not survive the round trip"
        );
    }

    first.cleanup().await;
    second.cleanup().await;
}

/// The secrets the round trip's set refers to, as the environment of the
/// import would hold them.
fn round_trip_secrets(name: &str) -> Option<String> {
    match name {
        "TROVATO_SECRET_OAUTH_CLIENT_0193A5A0_0092_7000_8000_000000000003_CLIENT_SECRET_HASH" => {
            Some("rt-client-secret".to_string())
        }
        "TROVATO_SECRET_WEBHOOK_0193A5A0_0092_7000_8000_000000000004_SECRET" => {
            Some("rt-webhook-secret".to_string())
        }
        _ => None,
    }
}

/// Every in-tree plugin whose manifest declares `[config] tables`.
const PLUGINS_WITH_CONFIG: [&str; 6] = [
    "trovato_config_translation",
    "trovato_image_styles",
    "trovato_locale",
    "trovato_oauth2",
    "trovato_redirects",
    "trovato_webhooks",
];

/// The six plugins that own config tables, enabled, and one row in each of
/// their tables. Returns `(file, field, value)` for one field of each file,
/// for asserting the export carries it.
fn write_plugin_config(dir: &TempConfigDir) -> Vec<(String, &'static str, String)> {
    let mut expect = Vec::new();
    for plugin in PLUGINS_WITH_CONFIG {
        let name = format!("plugin_status.{plugin}.yml");
        dir.write(&name, &format!("name: {plugin}\nenabled: true\n"));
        expect.push((name, "enabled", "true".to_string()));
    }

    let style = "0193a5a0-0092-7000-8000-000000000001";
    dir.write(
        &format!("image_style.{style}.yml"),
        &format!(
            "id: {style}\nname: hero\nlabel: Hero (1600w)\n\
             effects:\n- type: scale\n  width: 1600\ncreated: 1700000000\nchanged: 1700000001\n"
        ),
    );
    expect.push((
        format!("image_style.{style}.yml"),
        "label",
        "Hero (1600w)".to_string(),
    ));

    let redirect = "0193a5a0-0092-7000-8000-000000000002";
    dir.write(
        &format!("redirect.{redirect}.yml"),
        &format!(
            "id: {redirect}\nsource: /old-page\ndestination: /new-page\nstatus_code: 301\n\
             language: en\ncreated: 1700000000\n"
        ),
    );
    expect.push((
        format!("redirect.{redirect}.yml"),
        "source",
        "/old-page".to_string(),
    ));

    let client = "0193a5a0-0092-7000-8000-000000000003";
    dir.write(
        &format!("oauth_client.{client}.yml"),
        &format!(
            "id: {client}\nclient_id: rt-client\n\
             client_secret_hash: env:TROVATO_SECRET_OAUTH_CLIENT_0193A5A0_0092_7000_8000_000000000003_CLIENT_SECRET_HASH\n\
             name: Round trip\nredirect_uris:\n- https://example.com/cb\n\
             grant_types:\n- authorization_code\nscopes: []\ncreated: 1700000000\n"
        ),
    );
    expect.push((
        format!("oauth_client.{client}.yml"),
        "client_id",
        "rt-client".to_string(),
    ));

    let hook = "0193a5a0-0092-7000-8000-000000000004";
    dir.write(
        &format!("webhook.{hook}.yml"),
        &format!(
            "id: {hook}\nname: Deploy\nurl: https://example.com/hook\nevents:\n- item.insert\n\
             secret: env:TROVATO_SECRET_WEBHOOK_0193A5A0_0092_7000_8000_000000000004_SECRET\n\
             active: true\ncreated: 1700000000\nchanged: 1700000000\n"
        ),
    );
    expect.push((format!("webhook.{hook}.yml"), "name", "Deploy".to_string()));

    let string = "0193a5a0-0092-7000-8000-000000000005";
    dir.write(
        &format!("locale_string.{string}.yml"),
        &format!(
            "id: {string}\nsource: Read more\ntranslation: Leggi di più\nlanguage: it\ncontext: ''\n"
        ),
    );
    expect.push((
        format!("locale_string.{string}.yml"),
        "translation",
        "Leggi di più".to_string(),
    ));

    dir.write(
        "config_translation.variable.site_name.it.yml",
        "entity_type: variable\nentity_id: site_name\nlanguage: it\ndata:\n  value: Il sito\n",
    );
    expect.push((
        "config_translation.variable.site_name.it.yml".to_string(),
        "language",
        "it".to_string(),
    ));

    expect
}

/// A menu link whose declared parent is in neither the import set nor the
/// database is a validation failure, named by file, with nothing written.
///
/// Without this the foreign key rejects the row mid-save-pass, which reports as
/// a storage failure on whichever file happened to be saved first.
#[tokio::test]
async fn menu_link_import_fails_when_the_parent_does_not_exist() {
    let db = ScratchDb::new("menuorphan").await;
    let storage = db.storage();

    let dir = TempConfigDir::new("menuorphan");
    let orphan = "0193a5a0-0004-7000-8000-0000000000d1";
    let missing = "0193a5a0-0004-7000-8000-0000000000d2";
    dir.write(
        &format!("menu_link.{orphan}.yml"),
        &menu_link_yaml(
            orphan,
            "roundtrip",
            "/rt/orphan",
            "Orphan",
            Some(missing),
            0,
            false,
            "core",
            LIVE_STAGE_ID_STR,
        ),
    );

    let Err(err) = import_config(&storage, db.pool(), dir.path(), false).await else {
        db.cleanup().await;
        panic!("import must fail when a menu link's parent does not exist");
    };

    let failed = err
        .downcast_ref::<ConfigImportFailed>()
        .expect("the error must be a ConfigImportFailed");
    assert_eq!(failed.imported_total(), 0, "nothing should be written");
    assert_eq!(failed.failures.len(), 1, "expected one failure: {failed}");
    assert_eq!(
        failed.failures[0].filename,
        format!("menu_link.{orphan}.yml")
    );
    assert!(
        failed.failures[0].error.contains(missing),
        "the report must name the missing parent, got: {}",
        failed.failures[0].error
    );

    let landed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM menu_link")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(landed, 0, "a failed validation must write nothing");

    db.cleanup().await;
}

/// A parent cycle is a validation failure rather than an import that hangs or
/// half-applies. Two links naming each other is the smallest case.
#[tokio::test]
async fn menu_link_import_fails_on_a_parent_cycle() {
    let db = ScratchDb::new("menucycle").await;
    let storage = db.storage();

    let dir = TempConfigDir::new("menucycle");
    let a = "0193a5a0-0004-7000-8000-0000000000e1";
    let b = "0193a5a0-0004-7000-8000-0000000000e2";
    dir.write(
        &format!("menu_link.{a}.yml"),
        &menu_link_yaml(
            a,
            "roundtrip",
            "/rt/a",
            "A",
            Some(b),
            0,
            false,
            "core",
            LIVE_STAGE_ID_STR,
        ),
    );
    dir.write(
        &format!("menu_link.{b}.yml"),
        &menu_link_yaml(
            b,
            "roundtrip",
            "/rt/b",
            "B",
            Some(a),
            0,
            false,
            "core",
            LIVE_STAGE_ID_STR,
        ),
    );

    let Err(err) = import_config(&storage, db.pool(), dir.path(), false).await else {
        db.cleanup().await;
        panic!("import must fail on a menu link parent cycle");
    };

    let failed = err.downcast_ref::<ConfigImportFailed>().unwrap();
    assert_eq!(failed.imported_total(), 0, "nothing should be written");
    assert!(
        failed
            .failures
            .iter()
            .any(|f| f.error.contains("cycle") || f.error.contains("ancestor")),
        "the report must say the tree contains a cycle, got: {failed}"
    );

    db.cleanup().await;
}

/// A link that names itself as parent is the degenerate cycle.
#[tokio::test]
async fn menu_link_import_fails_when_a_link_is_its_own_parent() {
    let db = ScratchDb::new("menuself").await;
    let storage = db.storage();

    let dir = TempConfigDir::new("menuself");
    let id = "0193a5a0-0004-7000-8000-0000000000f1";
    dir.write(
        &format!("menu_link.{id}.yml"),
        &menu_link_yaml(
            id,
            "roundtrip",
            "/rt/self",
            "Self",
            Some(id),
            0,
            false,
            "core",
            LIVE_STAGE_ID_STR,
        ),
    );

    let Err(err) = import_config(&storage, db.pool(), dir.path(), false).await else {
        db.cleanup().await;
        panic!("import must fail when a menu link is its own parent");
    };

    let failed = err.downcast_ref::<ConfigImportFailed>().unwrap();
    assert_eq!(failed.imported_total(), 0, "nothing should be written");
    assert!(
        failed.failures[0].error.contains("itself"),
        "the report must say the link references itself, got: {}",
        failed.failures[0].error
    );

    db.cleanup().await;
}

/// A stage whose `category_tag` row is already present gains its `stage_config`
/// row rather than colliding on the primary key.
///
/// This is the state an export/import round trip produces: a stage's tag is
/// exported as a `tag` entity as well as inside the `stage` entity, and tags
/// import first. `save_stage` used to branch on "does the stage exist", read the
/// half-present state as absent, and take a create path whose `category_tag`
/// insert then failed. The two-step import below is that state in isolation.
#[tokio::test]
async fn a_stage_lands_when_its_tag_row_already_exists() {
    let db = ScratchDb::new("stagehalf").await;
    let storage = db.storage();

    // Step one: the stage's tag row arrives on its own, as a `tag` entity.
    let tags_only = TempConfigDir::new("stagehalf_tag");
    tags_only.write(
        &format!("tag.{OTHER_STAGE_ID}.yml"),
        &format!(
            "id: '{OTHER_STAGE_ID}'\ncategory_id: stages\nlabel: Roundtrip Review\n\
             description: null\nslug: roundtrip-review\nweight: 0\n\
             created: 1767225600\nchanged: 1767225600\n"
        ),
    );
    if let Err(e) = import_config(&storage, db.pool(), tags_only.path(), false).await {
        db.cleanup().await;
        panic!("the stage's tag must import on its own: {e:#}");
    }

    // Step two: the stage entity for that same id.
    let stage_only = TempConfigDir::new("stagehalf_stage");
    stage_only.write(&format!("stage.{OTHER_STAGE_ID}.yml"), &other_stage_yaml());
    if let Err(e) = import_config(&storage, db.pool(), stage_only.path(), false).await {
        db.cleanup().await;
        panic!("a stage whose tag row already exists must still land: {e:#}");
    }

    let landed: Option<String> =
        sqlx::query_scalar("SELECT machine_name FROM stage_config WHERE tag_id = $1")
            .bind(OTHER_STAGE_ID.parse::<uuid::Uuid>().unwrap())
            .fetch_optional(db.pool())
            .await
            .expect("failed to read stage_config");

    assert_eq!(
        landed.as_deref(),
        Some("roundtrip_review"),
        "the stage config row must have been written for the existing tag"
    );

    db.cleanup().await;
}

/// A stage cannot adopt a tag that belongs to some other category.
///
/// The fix above makes an existing tag row acceptable, which is only safe while
/// "existing" means "a stage tag". Without this guard the same code would attach
/// a `stage_config` row to somebody's topic term and call it a stage.
#[tokio::test]
async fn a_stage_refuses_an_id_that_belongs_to_another_category() {
    let db = ScratchDb::new("stageclash").await;
    let storage = db.storage();

    let dir = TempConfigDir::new("stageclash");
    dir.write(
        "category.stageclash_topics.yml",
        "id: stageclash_topics\nlabel: Topics\ndescription: null\nhierarchy: 0\nweight: 0\n",
    );
    dir.write(
        &format!("tag.{OTHER_STAGE_ID}.yml"),
        &format!(
            "id: '{OTHER_STAGE_ID}'\ncategory_id: stageclash_topics\nlabel: Not A Stage\n\
             description: null\nslug: not-a-stage\nweight: 0\n\
             created: 1767225600\nchanged: 1767225600\n"
        ),
    );
    dir.write(&format!("stage.{OTHER_STAGE_ID}.yml"), &other_stage_yaml());

    let Err(err) = import_config(&storage, db.pool(), dir.path(), false).await else {
        db.cleanup().await;
        panic!("a stage must not adopt a tag from another category");
    };

    assert!(
        err.to_string().contains("stageclash_topics"),
        "the failure must name the category the id already belongs to, got: {err:#}"
    );

    let stages: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM stage_config WHERE tag_id = $1")
        .bind(OTHER_STAGE_ID.parse::<uuid::Uuid>().unwrap())
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(stages, 0, "no stage config row may have been written");

    db.cleanup().await;
}

// =============================================================================
// Role permissions
// =============================================================================
//
// The `role` config entity carried a role's UUID and name and nothing else, so
// `config import` created roles that could do nothing and the tutorial's role
// files listed their intended permissions in comments. A role file now declares
// its permissions and the import grants exactly that set.

/// Write a role config file.
fn role_yaml(id: &str, name: &str, permissions: Option<&[&str]>) -> String {
    let mut yaml = format!("id: {id}\nname: {name}\ncreated: \"2026-01-01T00:00:00Z\"\n");
    match permissions {
        Some([]) => yaml.push_str("permissions: []\n"),
        Some(list) => {
            yaml.push_str("permissions:\n");
            for permission in list {
                yaml.push_str(&format!("- {permission}\n"));
            }
        }
        None => {}
    }
    yaml
}

async fn permissions_of(db: &ScratchDb, role_id: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT permission FROM role_permissions WHERE role_id = $1 ORDER BY permission",
    )
    .bind(role_id.parse::<uuid::Uuid>().unwrap())
    .fetch_all(db.pool())
    .await
    .expect("failed to read role permissions")
}

/// A role file's permissions are granted, and a re-import with one removed
/// revokes it: the file is authoritative.
#[tokio::test]
async fn a_role_file_grants_exactly_the_permissions_it_declares() {
    let db = ScratchDb::new("roleperms").await;
    let storage = db.storage();
    let role_id = "0193a5a0-0002-7000-8000-0000000000a1";

    let dir = TempConfigDir::new("roleperms");
    dir.write(
        &format!("role.{role_id}.yml"),
        &role_yaml(
            role_id,
            "roundtrip_editor",
            Some(&["access content", "create content", "edit any content"]),
        ),
    );

    if let Err(e) = import_config(&storage, db.pool(), dir.path(), false).await {
        db.cleanup().await;
        panic!("a role file with permissions must import clean: {e:#}");
    }

    assert_eq!(
        permissions_of(&db, role_id).await,
        vec![
            "access content".to_string(),
            "create content".to_string(),
            "edit any content".to_string()
        ],
        "import must grant exactly the declared set"
    );

    // Re-import with one permission removed: it must be revoked, not merged.
    let dir = TempConfigDir::new("roleperms2");
    dir.write(
        &format!("role.{role_id}.yml"),
        &role_yaml(
            role_id,
            "roundtrip_editor",
            Some(&["access content", "create content"]),
        ),
    );
    if let Err(e) = import_config(&storage, db.pool(), dir.path(), false).await {
        db.cleanup().await;
        panic!("the second import must succeed: {e:#}");
    }

    assert_eq!(
        permissions_of(&db, role_id).await,
        vec!["access content".to_string(), "create content".to_string()],
        "a permission the file no longer names must be revoked"
    );

    // And an explicitly empty list revokes everything.
    let dir = TempConfigDir::new("roleperms3");
    dir.write(
        &format!("role.{role_id}.yml"),
        &role_yaml(role_id, "roundtrip_editor", Some(&[])),
    );
    if let Err(e) = import_config(&storage, db.pool(), dir.path(), false).await {
        db.cleanup().await;
        panic!("the third import must succeed: {e:#}");
    }
    assert!(
        permissions_of(&db, role_id).await.is_empty(),
        "an empty list must revoke every permission"
    );

    db.cleanup().await;
}

/// A role file with no `permissions` key leaves the role's grants alone.
///
/// This is the compatibility case, and it is not cosmetic: every role file
/// written before this existed omits the key, and reading that as "revoke
/// everything" would mean an import silently stripping a site's permissions.
#[tokio::test]
async fn a_role_file_without_the_key_leaves_existing_grants_alone() {
    let db = ScratchDb::new("rolekeep").await;
    let storage = db.storage();
    let role_id = "0193a5a0-0002-7000-8000-0000000000b1";

    let with_permissions = TempConfigDir::new("rolekeep1");
    with_permissions.write(
        &format!("role.{role_id}.yml"),
        &role_yaml(role_id, "keeper", Some(&["access content", "access files"])),
    );
    import_config(&storage, db.pool(), with_permissions.path(), false)
        .await
        .expect("first import");

    let without = TempConfigDir::new("rolekeep2");
    without.write(
        &format!("role.{role_id}.yml"),
        &role_yaml(role_id, "keeper", None),
    );
    if let Err(e) = import_config(&storage, db.pool(), without.path(), false).await {
        db.cleanup().await;
        panic!("a role file without the key must still import: {e:#}");
    }

    assert_eq!(
        permissions_of(&db, role_id).await,
        vec!["access content".to_string(), "access files".to_string()],
        "an omitted key must not revoke anything"
    );

    db.cleanup().await;
}

/// An export carries the permissions, and re-importing it reproduces them.
#[tokio::test]
async fn a_role_export_round_trips_its_permissions() {
    let first = ScratchDb::new("rolexp1").await;
    let second = ScratchDb::new("rolexp2").await;
    let role_id = "0193a5a0-0002-7000-8000-0000000000c1";

    let source = TempConfigDir::new("rolexpsrc");
    source.write(
        &format!("role.{role_id}.yml"),
        &role_yaml(
            role_id,
            "exported_role",
            Some(&["access content", "use filtered_html"]),
        ),
    );
    if let Err(e) = import_config(&first.storage(), first.pool(), source.path(), false).await {
        first.cleanup().await;
        second.cleanup().await;
        panic!("import must succeed: {e:#}");
    }

    let exported = TempConfigDir::new("rolexpout");
    if let Err(e) = export_config(&first.storage(), first.pool(), exported.path(), false).await {
        first.cleanup().await;
        second.cleanup().await;
        panic!("export must succeed: {e:#}");
    }

    let document = std::fs::read_to_string(exported.path().join(format!("role.{role_id}.yml")))
        .expect("the role must be exported");
    assert!(
        document.contains("permissions:"),
        "the export must carry the permissions key, got:\n{document}"
    );
    assert!(
        document.contains("access content") && document.contains("use filtered_html"),
        "the export must carry both permissions, got:\n{document}"
    );

    if let Err(e) = import_config(&second.storage(), second.pool(), exported.path(), false).await {
        first.cleanup().await;
        second.cleanup().await;
        panic!("re-importing the export must succeed: {e:#}");
    }

    assert_eq!(
        permissions_of(&second, role_id).await,
        vec![
            "access content".to_string(),
            "use filtered_html".to_string()
        ],
        "the permissions must survive the round trip"
    );

    first.cleanup().await;
    second.cleanup().await;
}

/// An unknown permission fails validation, names the file and the string, says
/// what the likely cause is, and writes nothing.
#[tokio::test]
async fn an_unknown_permission_fails_validation_loudly() {
    let db = ScratchDb::new("rolebogus").await;
    let storage = db.storage();
    let role_id = "0193a5a0-0002-7000-8000-0000000000d1";

    let dir = TempConfigDir::new("rolebogus");
    dir.write(
        &format!("role.{role_id}.yml"),
        &role_yaml(
            role_id,
            "bogus_role",
            Some(&["access content", "administer widgets"]),
        ),
    );

    let Err(err) = import_config(&storage, db.pool(), dir.path(), false).await else {
        db.cleanup().await;
        panic!("import must fail when a role names an unknown permission");
    };

    let failed = err
        .downcast_ref::<ConfigImportFailed>()
        .expect("the error must be a ConfigImportFailed");
    assert_eq!(failed.imported_total(), 0, "nothing should be written");
    assert_eq!(failed.failures.len(), 1, "expected one failure: {failed}");
    assert_eq!(failed.failures[0].filename, format!("role.{role_id}.yml"));
    assert!(
        failed.failures[0].error.contains("administer widgets"),
        "the report must name the offending permission, got: {}",
        failed.failures[0].error
    );
    assert!(
        failed.failures[0].error.contains("not being enabled"),
        "the report must name a disabled plugin as the likely cause, got: {}",
        failed.failures[0].error
    );

    let landed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM roles WHERE name = 'bogus_role'")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(landed, 0, "a failed validation must write nothing");

    db.cleanup().await;
}

/// A config file writes a content translation.
///
/// The second of the two write paths. `item_translation` had no config entity at
/// all, so a config set could ship an item and never its translations, and a
/// site's translated content could not be exported and re-imported the way every
/// other entity can.
///
/// The file is `item_translation.<uuid>.<language>.yml`. The filename parser
/// splits an entity file on its *first* dot, so `<uuid>.<language>` arrives as
/// one id string and no parser change was needed to carry two keys in one name.
#[tokio::test]
async fn a_config_file_writes_a_content_translation() {
    let db = ScratchDb::new("itemtrans").await;
    let storage = db.storage();
    let item_id = "0193a5a0-0002-7000-8000-0000000000f1";

    // The table belongs to `trovato_content_translation`, so a database with
    // only the kernel's migrations does not have it.
    create_item_translation_table(&db).await;
    seed_language(&db, "it", "Italian").await;

    let dir = TempConfigDir::new("itemtrans");
    dir.write(
        &format!("item.{item_id}.yml"),
        &format!(
            "id: {item_id}\ntype: page\ntitle: The Original\nlanguage: en\nstatus: 1\nfields:\n  body: the original body\n"
        ),
    );
    dir.write(
        &format!("item_translation.{item_id}.it.yml"),
        &format!(
            "item_id: {item_id}\nlanguage: it\ntitle: L'originale\nfields:\n  body: il corpo originale\n"
        ),
    );

    if let Err(e) = import_config(&storage, db.pool(), dir.path(), false).await {
        db.cleanup().await;
        panic!("an item and its translation must import together: {e:#}");
    }

    let row: Option<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT title, fields FROM item_translation WHERE item_id = $1 AND language = 'it'",
    )
    .bind(item_id.parse::<uuid::Uuid>().unwrap())
    .fetch_optional(db.pool())
    .await
    .unwrap();

    let ok = row.as_ref().is_some_and(|(title, fields)| {
        title == "L'originale"
            && fields.get("body").and_then(|v| v.as_str()) == Some("il corpo originale")
    });
    db.cleanup().await;
    assert!(ok, "the translation must be stored as written, got {row:?}");
}

/// A translation of an item that does not exist is refused.
///
/// `item_translation` has no foreign key to `item`, so without this check the
/// file would import cleanly and produce a row nothing ever reads.
#[tokio::test]
async fn a_translation_of_a_missing_item_is_refused() {
    let db = ScratchDb::new("itemtransorphan").await;
    let storage = db.storage();
    let missing = "0193a5a0-0002-7000-8000-0000000000f2";

    create_item_translation_table(&db).await;
    seed_language(&db, "it", "Italian").await;

    let dir = TempConfigDir::new("itemtransorphan");
    dir.write(
        &format!("item_translation.{missing}.it.yml"),
        &format!("item_id: {missing}\nlanguage: it\ntitle: Orfano\nfields: {{}}\n"),
    );

    let result = import_config(&storage, db.pool(), dir.path(), false).await;
    let refused = result.is_err();
    db.cleanup().await;
    assert!(refused, "a translation of a missing item must be refused");
}

/// Create the plugin-owned `item_translation` table on a scratch database.
async fn create_item_translation_table(db: &ScratchDb) {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS item_translation (
            item_id UUID NOT NULL,
            language VARCHAR(12) NOT NULL,
            title TEXT NOT NULL DEFAULT '',
            fields JSONB NOT NULL DEFAULT '{}',
            created BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::bigint,
            changed BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::bigint,
            PRIMARY KEY (item_id, language)
        )",
    )
    .execute(db.pool())
    .await
    .expect("create item_translation");
}

async fn seed_language(db: &ScratchDb, id: &str, label: &str) {
    sqlx::query(
        "INSERT INTO language (id, label, weight, is_default, direction) \
         VALUES ($1, $2, 1, false, 'ltr') ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(label)
    .execute(db.pool())
    .await
    .expect("seed language");
}

/// A permission a plugin declared can be granted by a config file.
///
/// The point of dispatching `tap_perm`. Before it, a plugin's permissions were
/// in no list the kernel could consult, so `config import` refused to grant one:
/// a role file naming `administer netgrasp` failed validation, and the only way
/// to hold a plugin permission was SQL, which the next save of the permission
/// grid then deleted.
///
/// The declaration reaches this process through the `plugin_permission` table
/// rather than through memory, which is the whole reason that table exists:
/// `config import` is a CLI with a pool and no `AppState`, so an in-memory
/// registry would be invisible to it. This test writes the declaration the way
/// the boot dispatch does and then imports a role file naming it.
#[tokio::test]
async fn a_permission_a_plugin_declared_can_be_granted_by_a_config_file() {
    let db = ScratchDb::new("rolepluginperm").await;
    let storage = db.storage();
    let role_id = "0193a5a0-0002-7000-8000-0000000000e3";

    // A name no kernel permission and no seeded grant uses, so passing can only
    // mean the declaration was consulted.
    let declared = "administer widget farms";
    sqlx::query("INSERT INTO plugin_permission (name, plugin, description) VALUES ($1, $2, $3)")
        .bind(declared)
        .bind("trovato_widgets")
        .bind("Administer widget farms")
        .execute(db.pool())
        .await
        .unwrap();

    let held: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM role_permissions WHERE permission = $1")
            .bind(declared)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        held, 0,
        "no role may already hold it, or the older already-granted rule would \
         accept this file and the declaration would prove nothing"
    );

    let dir = TempConfigDir::new("rolepluginperm");
    dir.write(
        &format!("role.{role_id}.yml"),
        &role_yaml(role_id, "widget_admin", Some(&[declared])),
    );

    if let Err(e) = import_config(&storage, db.pool(), dir.path(), false).await {
        db.cleanup().await;
        panic!("a permission a plugin declared must be grantable by config: {e:#}");
    }

    assert_eq!(
        permissions_of(&db, role_id).await,
        vec![declared.to_string()],
        "the plugin permission must have been granted"
    );

    db.cleanup().await;
}

/// An undeclared, ungranted permission is still refused.
///
/// The other half of the rule above: accepting declarations must not turn the
/// validation off. A typo in a role file is still a grant that matches nothing
/// any check will ever ask for, and it still has to be caught here.
#[tokio::test]
async fn a_permission_nothing_declares_or_holds_is_still_refused() {
    let db = ScratchDb::new("rolebogusperm").await;
    let storage = db.storage();
    let role_id = "0193a5a0-0002-7000-8000-0000000000e4";

    let dir = TempConfigDir::new("rolebogusperm");
    dir.write(
        &format!("role.{role_id}.yml"),
        &role_yaml(role_id, "typo_role", Some(&["administer widgt farms"])),
    );

    let result = import_config(&storage, db.pool(), dir.path(), false).await;
    let refused = result.is_err();
    db.cleanup().await;
    assert!(
        refused,
        "a permission neither declared nor granted must still be refused"
    );
}

/// A permission the kernel does not define but a role already holds is valid.
///
/// The disabled-plugin case. A plugin that is off declares nothing, because only
/// enabled plugins are loaded and dispatched, while the grants it left behind
/// remain. Without this, exporting such a site would produce a set that cannot
/// be re-imported. The seeded `authenticated user` role proves the sets differ:
/// it holds `view own profile`, which the kernel's own list does not contain.
#[tokio::test]
async fn a_permission_some_role_already_holds_is_accepted() {
    let db = ScratchDb::new("roleplugin").await;
    let storage = db.storage();
    let role_id = "0193a5a0-0002-7000-8000-0000000000e1";

    // Not in KERNEL_PERMISSIONS, but seeded onto the authenticated role by a
    // migration — so the kernel has evidence it exists.
    let seeded: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM role_permissions WHERE permission = 'view own profile'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert!(
        seeded > 0,
        "this test depends on 'view own profile' being seeded"
    );

    let dir = TempConfigDir::new("roleplugin");
    dir.write(
        &format!("role.{role_id}.yml"),
        &role_yaml(role_id, "profile_role", Some(&["view own profile"])),
    );

    if let Err(e) = import_config(&storage, db.pool(), dir.path(), false).await {
        db.cleanup().await;
        panic!("a permission an existing role holds must be accepted: {e:#}");
    }

    assert_eq!(
        permissions_of(&db, role_id).await,
        vec!["view own profile".to_string()],
        "the grant must have been applied"
    );

    db.cleanup().await;
}

/// Role files declare real permissions, and importing the set grants them.
///
/// The point of the change this pins: a reader used to be told to run SQL, or to
/// click through the permission grid, for something a config set should apply.
#[tokio::test]
async fn roles_arrive_with_their_permissions() {
    let db = ScratchDb::new("fixtureperms").await;
    let storage = db.storage();

    if let Err(e) = import_config(&storage, db.pool(), &fixture_config_dir(), false).await {
        db.cleanup().await;
        panic!("the fixture config set must import clean: {e:#}");
    }

    for (role, expected) in [
        ("fixture_reader", vec!["access content"]),
        (
            "fixture_writer",
            vec!["access content", "create content", "edit own content"],
        ),
    ] {
        let granted: Vec<String> = sqlx::query_scalar(
            "SELECT permission FROM role_permissions rp \
             JOIN roles r ON r.id = rp.role_id WHERE r.name = $1 ORDER BY permission",
        )
        .bind(role)
        .fetch_all(db.pool())
        .await
        .expect("failed to read role permissions");

        let expected: Vec<String> = expected.into_iter().map(String::from).collect();
        assert_eq!(
            granted, expected,
            "the {role} role should arrive with its permissions"
        );
    }

    db.cleanup().await;
}

// The `item` config entity bound `promote` and `sticky` as literal zeros in its
// INSERT and left them, and `created`, out of `ON CONFLICT DO UPDATE`. A file
// could not promote an item to the front page or pin it, and a corrected
// `created` never reached an item that already existed.

/// Read back an imported item's promote, sticky and created.
async fn item_flags(db: &ScratchDb, id: &str) -> (i16, i16, i64) {
    sqlx::query_as("SELECT promote, sticky, created FROM item WHERE id = $1")
        .bind(id.parse::<uuid::Uuid>().unwrap())
        .fetch_one(db.pool())
        .await
        .expect("the item landed")
}

/// `promote`, `sticky` and `created` come from the file on insert and on re-import.
#[tokio::test]
async fn item_import_sets_promote_sticky_and_created_and_updates_them() {
    let db = ScratchDb::new("itemflags").await;
    let storage = db.storage();
    let id = "0193a5a0-0006-7000-8000-0000000000a1";
    let file = format!("item.{id}.yml");
    let dir = TempConfigDir::new("itemflags");

    // Insert: both flags on, an explicit creation time.
    dir.write(
        &file,
        &format!(
            "id: {id}\ntype: page\ntitle: Flagged\npromote: true\nsticky: true\n\
             created: 1767225600\n"
        ),
    );
    if let Err(e) = import_config(&storage, db.pool(), dir.path(), false).await {
        db.cleanup().await;
        panic!("the item must import clean: {e:#}");
    }
    assert_eq!(
        item_flags(&db, id).await,
        (1, 1, 1_767_225_600),
        "insert must take promote, sticky and created from the file"
    );

    // Re-import: promote turned off, sticky omitted (defaults to false), a
    // corrected creation time.
    dir.write(
        &file,
        &format!("id: {id}\ntype: page\ntitle: Flagged\npromote: false\ncreated: 1735689600\n"),
    );
    if let Err(e) = import_config(&storage, db.pool(), dir.path(), false).await {
        db.cleanup().await;
        panic!("the re-import must succeed: {e:#}");
    }
    assert_eq!(
        item_flags(&db, id).await,
        (0, 0, 1_735_689_600),
        "re-import must update promote, sticky and created"
    );

    // Re-import without `created`: the stored creation time must survive, since
    // an absent timestamp is not a claim that the item was created just now.
    dir.write(&file, &format!("id: {id}\ntype: page\ntitle: Flagged\n"));
    if let Err(e) = import_config(&storage, db.pool(), dir.path(), false).await {
        db.cleanup().await;
        panic!("the third import must succeed: {e:#}");
    }
    assert_eq!(
        item_flags(&db, id).await.2,
        1_735_689_600,
        "a file without created must not overwrite the stored creation time"
    );

    // Export carries the flags, so an export/import round trip keeps them.
    dir.write(
        &file,
        &format!("id: {id}\ntype: page\ntitle: Flagged\npromote: true\nsticky: true\n"),
    );
    if let Err(e) = import_config(&storage, db.pool(), dir.path(), false).await {
        db.cleanup().await;
        panic!("the fourth import must succeed: {e:#}");
    }
    let out = TempConfigDir::new("itemflags_export");
    export_config(&storage, db.pool(), out.path(), false)
        .await
        .expect("export");
    let exported = std::fs::read_to_string(out.path().join(&file)).expect("item exported");
    assert!(
        yaml_declares(&exported, "promote", "true") && yaml_declares(&exported, "sticky", "true"),
        "export must carry promote and sticky: {exported}"
    );

    db.cleanup().await;
}

// A hand-written config file that left out `created` failed to parse, and because
// import validates the whole set before writing, one such file took every other
// file in the set down with it. Items defaulted their timestamps; roles, tags,
// URL aliases, stages, tiles and menu links did not, and the last four also
// required `changed`.

/// Every config entity that carries timestamps imports from a file that omits
/// them, and lands with a real creation time rather than the epoch.
#[tokio::test]
async fn config_files_without_timestamps_import_and_are_stamped_now() {
    let db = ScratchDb::new("notimestamps").await;
    let storage = db.storage();
    let before = chrono::Utc::now().timestamp() - 5;

    let role_id = "0193a5a0-0005-7000-8000-0000000000a1";
    let tag_id = "0193a5a0-0005-7000-8000-0000000000a2";
    let alias_id = "0193a5a0-0005-7000-8000-0000000000a3";
    let tile_id = "0193a5a0-0005-7000-8000-0000000000a4";
    let link_id = "0193a5a0-0005-7000-8000-0000000000a5";

    let dir = TempConfigDir::new("notimestamps");
    dir.write(
        &format!("role.{role_id}.yml"),
        &format!("id: {role_id}\nname: notimestamps_role\npermissions:\n- access content\n"),
    );
    dir.write(
        "category.notimestamps_topics.yml",
        "id: notimestamps_topics\nlabel: Topics\ndescription: null\nhierarchy: 0\nweight: 0\n",
    );
    dir.write(
        &format!("tag.{tag_id}.yml"),
        &format!(
            "id: {tag_id}\ncategory_id: notimestamps_topics\nlabel: Rust\n\
             description: null\nslug: null\nweight: 0\n"
        ),
    );
    dir.write(
        &format!("stage.{OTHER_STAGE_ID}.yml"),
        &format!(
            "id: {OTHER_STAGE_ID}\nmachine_name: roundtrip_review\nlabel: Roundtrip Review\n\
             visibility: internal\nis_default: false\nweight: 0\n"
        ),
    );
    dir.write(
        &format!("url_alias.{alias_id}.yml"),
        &format!(
            "id: {alias_id}\nsource: /item/{tag_id}\nalias: /notimestamps\nlanguage: en\n\
             stage_id: {OTHER_STAGE_ID}\n"
        ),
    );
    dir.write(
        &format!("tile.{tile_id}.yml"),
        &format!(
            "id: {tile_id}\nmachine_name: notimestamps_tile\nlabel: Tile\nregion: sidebar\n\
             tile_type: custom\nconfig: {{}}\nvisibility: {{}}\nweight: 0\nstatus: 1\n\
             plugin: core\nstage_id: {OTHER_STAGE_ID}\n"
        ),
    );
    dir.write(
        &format!("menu_link.{link_id}.yml"),
        &format!(
            "id: {link_id}\nmenu_name: main\npath: /notimestamps\ntitle: Link\nparent_id: null\n\
             weight: 0\nhidden: false\nplugin: core\nstage_id: {OTHER_STAGE_ID}\n"
        ),
    );

    if let Err(e) = import_config(&storage, db.pool(), dir.path(), false).await {
        db.cleanup().await;
        panic!("config files without timestamps must import clean: {e:#}");
    }

    let role_created: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT created FROM roles WHERE name = 'notimestamps_role'")
            .fetch_one(db.pool())
            .await
            .expect("the role landed");
    assert!(
        role_created.timestamp() >= before,
        "role created {role_created}"
    );

    for (what, sql) in [
        (
            "tag",
            "SELECT created FROM category_tag WHERE label = 'Rust' AND category_id = 'notimestamps_topics'",
        ),
        (
            "url alias",
            "SELECT created FROM url_alias WHERE alias = '/notimestamps'",
        ),
        (
            "stage",
            "SELECT t.created FROM category_tag t JOIN stage_config s ON s.tag_id = t.id \
             WHERE s.machine_name = 'roundtrip_review'",
        ),
        (
            "tile",
            "SELECT created FROM tile WHERE machine_name = 'notimestamps_tile'",
        ),
        (
            "menu link",
            "SELECT created FROM menu_link WHERE path = '/notimestamps'",
        ),
    ] {
        let created: i64 = sqlx::query_scalar(sql)
            .fetch_one(db.pool())
            .await
            .unwrap_or_else(|e| panic!("the {what} landed: {e}"));
        assert!(
            created >= before,
            "the {what} must be stamped with the import time, not {created}"
        );
    }

    db.cleanup().await;
}

/// A role file that is genuinely wrong still reports itself by name, and the set
/// stays atomic: the valid file beside it is not written.
#[tokio::test]
async fn a_bad_role_file_is_named_and_nothing_is_written() {
    let db = ScratchDb::new("badrole").await;
    let storage = db.storage();
    let role_id = "0193a5a0-0005-7000-8000-0000000000b1";

    let dir = TempConfigDir::new("badrole");
    dir.write(
        "category.badrole_topics.yml",
        "id: badrole_topics\nlabel: Topics\ndescription: null\nhierarchy: 0\nweight: 0\n",
    );
    // No `name`, which a role cannot do without.
    dir.write(&format!("role.{role_id}.yml"), &format!("id: {role_id}\n"));

    let err = import_config(&storage, db.pool(), dir.path(), false)
        .await
        .expect_err("a role file without a name must fail the import");
    let failed = err
        .downcast_ref::<ConfigImportFailed>()
        .expect("the error is a ConfigImportFailed");
    assert_eq!(failed.failures.len(), 1, "one failure: {failed}");
    assert_eq!(failed.failures[0].filename, format!("role.{role_id}.yml"));
    assert!(
        storage
            .load(entity_types::CATEGORY, "badrole_topics")
            .await
            .expect("category lookup")
            .is_none(),
        "the valid sibling must not be written"
    );

    db.cleanup().await;
}

/// A `gather_query` tile file whose `config` names the gather and `items`.
fn gather_tile_yaml(tile_id: &str, machine_name: &str, items: &str) -> String {
    format!(
        "id: {tile_id}\nmachine_name: {machine_name}\nlabel: Gather Tile\nregion: sidebar\n\
         tile_type: gather_query\nconfig:\n  query_id: upcoming\n  items: {items}\n\
         visibility: {{}}\nweight: 0\nstatus: 1\nplugin: core\n\
         stage_id: 0193a5a0-0000-7000-8000-000000000001\ncreated: 1767225600\nchanged: 1767225600\n"
    )
}

/// A gather tile's item count arrives with its import and leaves with its
/// export (BL-99).
#[tokio::test]
async fn a_gather_tile_item_count_survives_import_and_export() {
    let db = ScratchDb::new("gathertile").await;
    let storage = db.storage();

    let dir = TempConfigDir::new("gathertile");
    let tile_id = "0193a5a0-0003-7000-8000-0000000000b1";
    dir.write(
        &format!("tile.{tile_id}.yml"),
        &gather_tile_yaml(tile_id, "gather_tile_rt", "12"),
    );
    if let Err(e) = import_config(&storage, db.pool(), dir.path(), false).await {
        db.cleanup().await;
        panic!("the gather tile must import clean: {e:#}");
    }
    let config: serde_json::Value =
        sqlx::query_scalar("SELECT config FROM tile WHERE machine_name = 'gather_tile_rt'")
            .fetch_one(db.pool())
            .await
            .expect("the imported tile");
    assert_eq!(config["items"], serde_json::json!(12), "{config}");

    let export = TempConfigDir::new("gathertile_export");
    if let Err(e) = export_config(&storage, db.pool(), export.path(), false).await {
        db.cleanup().await;
        panic!("export must succeed: {e:#}");
    }
    let exported = std::fs::read_to_string(export.path().join(format!("tile.{tile_id}.yml")))
        .expect("the tile must be exported");
    assert!(yaml_declares(&exported, "items", "12"), "{exported}");
    assert!(
        yaml_declares(&exported, "query_id", "upcoming"),
        "{exported}"
    );

    db.cleanup().await;
}

/// A tile's `audience` visibility rule (anonymous-only or signed-in-only)
/// arrives with its import and leaves with its export, same as any other
/// `visibility` key (B174).
#[tokio::test]
async fn a_tile_audience_rule_survives_import_and_export() {
    let db = ScratchDb::new("tileaudience").await;
    let storage = db.storage();

    let dir = TempConfigDir::new("tileaudience");
    let tile_id = "0193a5a0-0003-7000-8000-0000000000d1";
    dir.write(
        &format!("tile.{tile_id}.yml"),
        &format!(
            "id: {tile_id}\nmachine_name: login_tile_rt\nlabel: Login\nregion: sidebar\n\
             tile_type: custom_html\nconfig: {{}}\nvisibility:\n  audience: anonymous\n\
             weight: 0\nstatus: 1\nplugin: core\n\
             stage_id: 0193a5a0-0000-7000-8000-000000000001\ncreated: 1767225600\nchanged: 1767225600\n"
        ),
    );
    if let Err(e) = import_config(&storage, db.pool(), dir.path(), false).await {
        db.cleanup().await;
        panic!("the tile must import clean: {e:#}");
    }
    let visibility: serde_json::Value =
        sqlx::query_scalar("SELECT visibility FROM tile WHERE machine_name = 'login_tile_rt'")
            .fetch_one(db.pool())
            .await
            .expect("the imported tile");
    assert_eq!(visibility["audience"], serde_json::json!("anonymous"));

    let export = TempConfigDir::new("tileaudience_export");
    if let Err(e) = export_config(&storage, db.pool(), export.path(), false).await {
        db.cleanup().await;
        panic!("export must succeed: {e:#}");
    }
    let exported = std::fs::read_to_string(export.path().join(format!("tile.{tile_id}.yml")))
        .expect("the tile must be exported");
    assert!(
        yaml_declares(&exported, "audience", "anonymous"),
        "{exported}"
    );

    db.cleanup().await;
}

/// An item count outside 1 to 50 is refused with the file named, and nothing
/// is written: the same rule the tile form applies.
#[tokio::test]
async fn import_refuses_a_gather_tile_item_count_outside_one_to_fifty() {
    let db = ScratchDb::new("gathertilebad").await;
    let storage = db.storage();

    for (n, items) in ["0", "51", "\"five\""].iter().enumerate() {
        let dir = TempConfigDir::new("gathertilebad");
        let tile_id = format!("0193a5a0-0003-7000-8000-0000000000c{n}");
        let filename = format!("tile.{tile_id}.yml");
        dir.write(
            &filename,
            &gather_tile_yaml(&tile_id, "gather_tile_bad", items),
        );

        let err = import_config(&storage, db.pool(), dir.path(), false)
            .await
            .expect_err("an out-of-range item count must fail the import");
        let failed = err.downcast_ref::<ConfigImportFailed>().unwrap();
        assert_eq!(failed.imported_total(), 0, "items: {items} wrote nothing");
        assert!(
            failed
                .failures
                .iter()
                .any(|f| f.filename == filename && f.error.contains("1 and 50")),
            "items: {items}: {:#?}",
            failed.failures
        );
    }

    db.cleanup().await;
}

/// A plugin config row is refused before anything is written when its plugin
/// would not be enabled, when it names a column its table lacks, and when a
/// plugin the set enables is not on disk.
#[tokio::test]
async fn a_plugin_row_is_refused_by_name_when_it_cannot_be_written() {
    let db = ScratchDb::new("pluginrow").await;
    let storage = db.plugin_storage().await;
    let redirect = "0193a5a0-0092-7000-8000-0000000000aa";
    let row = format!(
        "id: {redirect}\nsource: /a\ndestination: /b\nstatus_code: 301\nlanguage: en\ncreated: 1\n"
    );

    // The redirects plugin is not enabled, and the set does not enable it.
    let dir = TempConfigDir::new("pluginrow_disabled");
    dir.write(&format!("redirect.{redirect}.yml"), &row);
    dir.write(
        "plugin_status.no_such_plugin.yml",
        "name: no_such_plugin\nenabled: true\n",
    );
    let err = import_config(&storage, db.pool(), dir.path(), false)
        .await
        .expect_err("a row for a plugin that is not enabled must be refused");
    let failed = err.downcast_ref::<ConfigImportFailed>().unwrap();
    assert_eq!(
        failed.imported_total(),
        0,
        "nothing may be written: {failed}"
    );
    let text = failed.to_string();
    assert!(
        text.contains(&format!("redirect.{redirect}.yml")) && text.contains("trovato_redirects"),
        "the row's file and its plugin must be named: {text}"
    );
    assert!(
        text.contains("plugin_status.no_such_plugin.yml") && text.contains("not on disk"),
        "enabling a plugin that is not on disk must be refused: {text}"
    );

    // Enabled, with its table in place: a column the table lacks is refused.
    let dir = TempConfigDir::new("pluginrow_column");
    dir.write(
        "plugin_status.trovato_redirects.yml",
        "name: trovato_redirects\nenabled: true\n",
    );
    import_config(&storage, db.pool(), dir.path(), false)
        .await
        .expect("enabling the plugin must import");
    dir.write(
        &format!("redirect.{redirect}.yml"),
        &format!("{row}no_such_column: 1\n"),
    );
    let err = import_config(&storage, db.pool(), dir.path(), false)
        .await
        .expect_err("an unknown column must be refused");
    let text = err.to_string();
    assert!(
        text.contains("no_such_column"),
        "the unknown column must be named: {text}"
    );

    db.cleanup().await;
}

// ---------------------------------------------------------------------------
// B130: a config set never carries a secret.
//
// PR 201 exported each plugin config row with `to_jsonb`, so `webhook.secret`
// and `oauth_client.client_secret_hash` were written in the clear. These tests
// pin the fix at the export and import layer.
// ---------------------------------------------------------------------------

/// The two plugins whose config tables hold a secret.
const SECRET_PLUGINS: [&str; 2] = ["trovato_oauth2", "trovato_webhooks"];

/// One site's secrets, unique per test so that a match in an export can only
/// have come from this site.
struct SiteSecrets {
    hook_id: String,
    client_id: String,
    webhook_secret: String,
    client_secret: String,
    client_secret_hash: String,
    turnstile: String,
}

impl SiteSecrets {
    fn new() -> Self {
        let tag = uuid::Uuid::now_v7().simple().to_string();
        let client_secret = format!("client-secret-{tag}");
        let client_secret_hash = argon2::password_hash::PasswordHasher::hash_password(
            &argon2::Argon2::default(),
            client_secret.as_bytes(),
            &argon2::password_hash::SaltString::generate(
                &mut argon2::password_hash::rand_core::OsRng,
            ),
        )
        .unwrap()
        .to_string();
        Self {
            hook_id: uuid::Uuid::now_v7().to_string(),
            client_id: uuid::Uuid::now_v7().to_string(),
            webhook_secret: format!("whsec-{tag}"),
            client_secret,
            client_secret_hash,
            turnstile: format!("turnstile-{tag}"),
        }
    }

    fn webhook_var(&self) -> String {
        plugin_config::secret_env_name("webhook", &self.hook_id, "secret")
    }

    fn client_var(&self) -> String {
        plugin_config::secret_env_name("oauth_client", &self.client_id, "client_secret_hash")
    }

    fn turnstile_var(&self) -> String {
        plugin_config::secret_env_name(
            "variable",
            "plugin.trovato_captcha.turnstile_secret_key",
            "value",
        )
    }

    /// The import environment that supplies every secret, in plaintext.
    fn env(&self) -> impl Fn(&str) -> Option<String> + Send + Sync + use<> {
        let vars = [
            (self.webhook_var(), self.webhook_secret.clone()),
            (self.client_var(), self.client_secret.clone()),
            (self.turnstile_var(), self.turnstile.clone()),
        ];
        move |name: &str| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
    }
}

/// Enable the secret-holding plugins and run their migrations, as a booted
/// install would have.
async fn enable_secret_plugins(db: &ScratchDb, storage: &DirectConfigStorage) {
    for plugin in SECRET_PLUGINS {
        plugin_config::save_status(
            db.pool(),
            &ConfigPluginStatus {
                name: plugin.to_string(),
                enabled: true,
            },
            "0",
        )
        .await
        .unwrap();
    }
    storage
        .plugins()
        .unwrap()
        .migrate_enabled(db.pool())
        .await
        .unwrap();
}

/// A site with one webhook (secret set), one OAuth client and the captcha
/// secret.
async fn seed_secret_site(db: &ScratchDb, storage: &DirectConfigStorage, s: &SiteSecrets) {
    enable_secret_plugins(db, storage).await;
    sqlx::query(
        "INSERT INTO webhook (id, name, url, events, secret) \
         VALUES ($1::uuid, 'Deploy', 'https://example.com/hook', '[\"item.insert\"]', $2)",
    )
    .bind(&s.hook_id)
    .bind(&s.webhook_secret)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO oauth_client (id, client_id, client_secret_hash, name, grant_types) \
         VALUES ($1::uuid, 'b130-client', $2, 'B130', '[\"client_credentials\"]')",
    )
    .bind(&s.client_id)
    .bind(&s.client_secret_hash)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO site_config (key, value) \
         VALUES ('plugin.trovato_captcha.turnstile_secret_key', to_jsonb($1::text))",
    )
    .bind(&s.turnstile)
    .execute(db.pool())
    .await
    .unwrap();
}

/// Every file under `dir`, recursively, as `(relative path, bytes)`.
fn every_file(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(dir).unwrap().display().to_string();
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    out
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

async fn webhook_secret_of(db: &ScratchDb, id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT secret FROM webhook WHERE id = $1::uuid")
        .bind(id)
        .fetch_optional(db.pool())
        .await
        .unwrap()
}

async fn client_hash_of(db: &ScratchDb, id: &str) -> Option<String> {
    sqlx::query_scalar("SELECT client_secret_hash FROM oauth_client WHERE id = $1::uuid")
        .bind(id)
        .fetch_optional(db.pool())
        .await
        .unwrap()
}

/// (a) The guard: no file an export writes holds a stored secret, the OAuth
/// client's hash or its plaintext. Each secret is a reference instead.
#[tokio::test]
async fn an_export_never_contains_a_secret() {
    let db = ScratchDb::new("b130export").await;
    let storage = db.plugin_storage().await;
    let s = SiteSecrets::new();
    seed_secret_site(&db, &storage, &s).await;

    let out = TempConfigDir::new("b130export");
    export_config(&storage, db.pool(), out.path(), false)
        .await
        .unwrap();

    let files = every_file(out.path());
    assert!(
        files
            .iter()
            .any(|(name, _)| name == &format!("webhook.{}.yml", s.hook_id)),
        "the webhook itself must still be exported"
    );
    for (name, bytes) in &files {
        for secret in [
            &s.webhook_secret,
            &s.client_secret,
            &s.client_secret_hash,
            &s.turnstile,
        ] {
            assert!(
                !contains(bytes, secret),
                "{name} holds a secret:\n{}",
                String::from_utf8_lossy(bytes)
            );
        }
        // No Argon2 hash of anything, either.
        assert!(
            !contains(bytes, "$argon2"),
            "{name} holds a password hash:\n{}",
            String::from_utf8_lossy(bytes)
        );
    }
    let hook =
        std::fs::read_to_string(out.path().join(format!("webhook.{}.yml", s.hook_id))).unwrap();
    assert!(
        yaml_declares(&hook, "secret", &format!("env:{}", s.webhook_var())),
        "the webhook's secret must be a reference, got:\n{hook}"
    );
    let client =
        std::fs::read_to_string(out.path().join(format!("oauth_client.{}.yml", s.client_id)))
            .unwrap();
    assert!(
        yaml_declares(
            &client,
            "client_secret_hash",
            &format!("env:{}", s.client_var())
        ),
        "the client's hash must be a reference, got:\n{client}"
    );
    db.cleanup().await;
}

/// An empty secret is left out of the file, not written as a reference
/// nobody could supply.
#[tokio::test]
async fn an_empty_secret_is_left_out_of_the_export() {
    let db = ScratchDb::new("b130empty").await;
    let storage = db.plugin_storage().await;
    enable_secret_plugins(&db, &storage).await;
    let hook = uuid::Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO webhook (id, name, url) VALUES ($1::uuid, 'Open', 'https://example.com/h')",
    )
    .bind(&hook)
    .execute(db.pool())
    .await
    .unwrap();
    let out = TempConfigDir::new("b130empty");
    export_config(&storage, db.pool(), out.path(), false)
        .await
        .unwrap();
    let file = std::fs::read_to_string(out.path().join(format!("webhook.{hook}.yml"))).unwrap();
    assert!(
        !file.lines().any(|l| l.starts_with("secret:")),
        "an empty secret must be left out, got:\n{file}"
    );

    // And it imports into a fresh install with no variable set.
    let fresh = ScratchDb::new("b130emptyfresh").await;
    let fresh_storage = fresh.plugin_storage().await;
    enable_secret_plugins(&fresh, &fresh_storage).await;
    let none = |_: &str| None;
    import_config_with_env(&fresh_storage, fresh.pool(), out.path(), false, &none)
        .await
        .unwrap();
    assert_eq!(webhook_secret_of(&fresh, &hook).await.as_deref(), Some(""));
    db.cleanup().await;
    fresh.cleanup().await;
}

/// (b) An export imports into a fresh install with the variables set: the
/// webhook holds the supplied secret, the OAuth client authenticates with the
/// supplied secret, and neither plaintext of the client secret nor the old
/// hash is stored.
#[tokio::test]
async fn an_export_imports_into_a_fresh_install_with_its_secrets_from_the_environment() {
    let source = ScratchDb::new("b130src").await;
    let source_storage = source.plugin_storage().await;
    let s = SiteSecrets::new();
    seed_secret_site(&source, &source_storage, &s).await;
    let out = TempConfigDir::new("b130fresh");
    export_config(&source_storage, source.pool(), out.path(), false)
        .await
        .unwrap();

    // A fresh install that has booted with the plugins enabled, as in the
    // round trip above: their migrations have run and seeded their roles.
    let fresh = ScratchDb::new("b130dst").await;
    let fresh_storage = fresh.plugin_storage().await;
    enable_secret_plugins(&fresh, &fresh_storage).await;
    let env = s.env();
    if let Err(e) =
        import_config_with_env(&fresh_storage, fresh.pool(), out.path(), false, &env).await
    {
        source.cleanup().await;
        fresh.cleanup().await;
        panic!("an export must import with its secrets supplied: {e:#}");
    }

    assert_eq!(
        webhook_secret_of(&fresh, &s.hook_id).await.as_deref(),
        Some(s.webhook_secret.as_str()),
        "the webhook must sign with the supplied secret"
    );
    let hash = client_hash_of(&fresh, &s.client_id).await.unwrap();
    assert_ne!(hash, s.client_secret, "the plaintext must never be stored");
    assert!(hash.starts_with("$argon2id$"), "stored {hash}");
    let oauth = trovato_kernel::services::oauth::OAuthService::new(
        fresh.pool().clone(),
        b"b130-test-jwt-secret-at-least-32-bytes!!",
    );
    assert!(
        oauth
            .validate_client_credentials("b130-client", &s.client_secret)
            .await
            .unwrap()
            .is_some(),
        "the client must authenticate with the supplied secret"
    );
    assert!(
        oauth
            .validate_client_credentials("b130-client", "wrong")
            .await
            .unwrap()
            .is_none(),
        "and not with another"
    );
    let turnstile: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT value FROM site_config WHERE key = 'plugin.trovato_captcha.turnstile_secret_key'",
    )
    .fetch_optional(fresh.pool())
    .await
    .unwrap();
    assert_eq!(turnstile, Some(serde_json::json!(s.turnstile)));

    source.cleanup().await;
    fresh.cleanup().await;
}

/// (c) With the variables unset, import refuses, names each variable, and
/// writes nothing: not the rows, not the plugin status, not the variables.
#[tokio::test]
async fn an_import_without_the_variables_refuses_and_writes_nothing() {
    let source = ScratchDb::new("b130unsetsrc").await;
    let source_storage = source.plugin_storage().await;
    let s = SiteSecrets::new();
    seed_secret_site(&source, &source_storage, &s).await;
    let out = TempConfigDir::new("b130unset");
    export_config(&source_storage, source.pool(), out.path(), false)
        .await
        .unwrap();
    out.write(
        "variable.site_name.yml",
        "key: site_name\nvalue: Must not land\n",
    );

    let fresh = ScratchDb::new("b130unsetdst").await;
    let fresh_storage = fresh.plugin_storage().await;
    enable_secret_plugins(&fresh, &fresh_storage).await;
    let none = |_: &str| None;
    let err = import_config_with_env(&fresh_storage, fresh.pool(), out.path(), false, &none)
        .await
        .expect_err("an import missing its secrets must fail");
    let failed = err
        .downcast_ref::<ConfigImportFailed>()
        .expect("a validation failure");
    let all = failed
        .failures
        .iter()
        .map(|f| format!("{}: {}", f.filename, f.error))
        .collect::<Vec<_>>()
        .join("\n");
    for var in [s.webhook_var(), s.client_var(), s.turnstile_var()] {
        assert!(
            all.contains(&var),
            "the failure must name {var}, got:\n{all}"
        );
    }

    // Only the refused files name a variable: the rest of the set is fine.
    assert_eq!(failed.failures.len(), 3, "{all}");
    for table in ["webhook", "oauth_client"] {
        let rows: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                .fetch_one(fresh.pool())
                .await
                .unwrap();
        assert_eq!(rows, 0, "a refused import must not write a {table} row");
    }
    let name: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT value FROM site_config WHERE key = 'site_name'")
            .fetch_optional(fresh.pool())
            .await
            .unwrap();
    assert_ne!(
        name,
        Some(serde_json::json!("Must not land")),
        "a refused import must not write the files that did validate"
    );
    let captcha: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT value FROM site_config WHERE key = 'plugin.trovato_captcha.turnstile_secret_key'",
    )
    .fetch_optional(fresh.pool())
    .await
    .unwrap();
    assert!(captcha.is_none());

    source.cleanup().await;
    fresh.cleanup().await;
}

/// (d) Re-importing an export onto the install it came from, with no
/// variable set, keeps every secret that install holds.
#[tokio::test]
async fn a_re_import_keeps_the_secrets_the_install_holds() {
    let db = ScratchDb::new("b130keep").await;
    let storage = db.plugin_storage().await;
    let s = SiteSecrets::new();
    seed_secret_site(&db, &storage, &s).await;
    let out = TempConfigDir::new("b130keep");
    export_config(&storage, db.pool(), out.path(), false)
        .await
        .unwrap();

    let none = |_: &str| None;
    if let Err(e) = import_config_with_env(&storage, db.pool(), out.path(), false, &none).await {
        db.cleanup().await;
        panic!("a re-import onto the same install must not need the variables: {e:#}");
    }
    assert_eq!(
        webhook_secret_of(&db, &s.hook_id).await.as_deref(),
        Some(s.webhook_secret.as_str())
    );
    assert_eq!(
        client_hash_of(&db, &s.client_id).await.as_deref(),
        Some(s.client_secret_hash.as_str())
    );
    let turnstile: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT value FROM site_config WHERE key = 'plugin.trovato_captcha.turnstile_secret_key'",
    )
    .fetch_optional(db.pool())
    .await
    .unwrap();
    assert_eq!(turnstile, Some(serde_json::json!(s.turnstile)));

    // A variable set to something else does not overwrite what the install
    // holds either: first match wins.
    let other = |_: &str| Some("other".to_string());
    import_config_with_env(&storage, db.pool(), out.path(), false, &other)
        .await
        .unwrap();
    assert_eq!(
        webhook_secret_of(&db, &s.hook_id).await.as_deref(),
        Some(s.webhook_secret.as_str())
    );
    db.cleanup().await;
}

/// A file holding a secret in the clear, as an export from before B130 does,
/// is refused even where the install already holds a secret, and nothing is
/// written.
#[tokio::test]
async fn a_secret_in_the_clear_is_refused() {
    let db = ScratchDb::new("b130literal").await;
    let storage = db.plugin_storage().await;
    let s = SiteSecrets::new();
    seed_secret_site(&db, &storage, &s).await;

    let dir = TempConfigDir::new("b130literal");
    dir.write(
        &format!("webhook.{}.yml", s.hook_id),
        &format!(
            "id: {}\nname: Renamed\nurl: https://example.com/hook\nevents: []\n\
             secret: smuggled\nactive: true\ncreated: 1700000000\nchanged: 1700000000\n",
            s.hook_id
        ),
    );
    dir.write(
        "variable.plugin.trovato_captcha.turnstile_secret_key.yml",
        "key: plugin.trovato_captcha.turnstile_secret_key\nvalue: smuggled\n",
    );
    // A reference to a variable outside TROVATO_SECRET_ is a literal too.
    let client = uuid::Uuid::now_v7();
    dir.write(
        &format!("oauth_client.{client}.yml"),
        &format!("id: {client}\nclient_id: other\nclient_secret_hash: env:DATABASE_URL\nname: x\n"),
    );
    let env = |_: &str| Some("supplied".to_string());
    let err = import_config_with_env(&storage, db.pool(), dir.path(), false, &env)
        .await
        .expect_err("a secret in the clear must be refused");
    let failed = err.downcast_ref::<ConfigImportFailed>().unwrap();
    assert_eq!(failed.failures.len(), 3, "{:?}", failed.failures);
    assert!(
        failed
            .failures
            .iter()
            .all(|f| f.error.contains("in the clear")),
        "{:?}",
        failed.failures
    );
    let name: String = sqlx::query_scalar("SELECT name FROM webhook WHERE id = $1::uuid")
        .bind(&s.hook_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(name, "Deploy", "nothing may be written");
    assert_eq!(
        webhook_secret_of(&db, &s.hook_id).await.as_deref(),
        Some(s.webhook_secret.as_str())
    );
    db.cleanup().await;
}

/// Every in-tree config table's credential-looking column is declared under
/// `[config] secret_columns`, so the export warning never fires for an
/// in-tree plugin and the declaration, not the name heuristic, is the
/// contract.
#[tokio::test]
async fn every_in_tree_credential_column_is_declared_secret() {
    let db = ScratchDb::new("b130decl").await;
    let storage = db.plugin_storage().await;
    let plugins = storage.plugins().unwrap();
    for plugin in PLUGINS_WITH_CONFIG {
        plugin_config::save_status(
            db.pool(),
            &ConfigPluginStatus {
                name: plugin.to_string(),
                enabled: true,
            },
            "0",
        )
        .await
        .unwrap();
    }
    plugins.migrate_enabled(db.pool()).await.unwrap();
    for (_, table) in plugins.tables() {
        let shape = plugin_config::table_shape(db.pool(), &table)
            .await
            .unwrap()
            .unwrap();
        let declared = plugins.secret_columns(&table);
        for column in &shape.columns {
            if plugin_config::looks_like_credential(column) {
                assert!(
                    declared.contains(column),
                    "{table}.{column} looks like a credential and is not declared secret"
                );
            }
        }
        for column in &declared {
            assert!(
                shape.columns.contains(column),
                "{table} declares secret column {column}, which it does not have"
            );
        }
    }
    assert_eq!(plugins.secret_columns("webhook"), vec!["secret"]);
    assert_eq!(
        plugins.secret_columns("oauth_client"),
        vec!["client_secret_hash"]
    );
    db.cleanup().await;
}
