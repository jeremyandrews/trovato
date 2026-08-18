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

use sqlx::{Connection, Executor, PgConnection, PgPool};
use trovato_kernel::config_storage::yaml::{ConfigImportFailed, export_config, import_config};
use trovato_kernel::config_storage::{ConfigStorage, DirectConfigStorage, entity_types};

/// A database created for one test and dropped when the test ends.
///
/// "Imports clean against a fresh database" is the claim under test, so the test
/// gets an actually fresh database: created, migrated, used, dropped.
struct ScratchDb {
    /// Connection URL of the server, without the database name.
    server_url: String,
    name: String,
    pool: Option<PgPool>,
}

impl ScratchDb {
    async fn new(label: &str) -> Self {
        trovato_test_utils::env::load_dotenv();
        let database_url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL must be set to run these tests");

        // Split `postgres://user:pass@host:port/dbname` into server and name,
        // tolerating a query string after the database name.
        let without_query = database_url
            .split_once('?')
            .map_or(database_url.as_str(), |(base, _)| base);
        let cut = without_query
            .rfind('/')
            .expect("DATABASE_URL must include a database name");
        let server_url = without_query[..cut].to_string();

        let name = format!(
            "trovato_cfgimport_{label}_{}",
            uuid::Uuid::now_v7().simple()
        );

        let mut admin = PgConnection::connect(&format!("{server_url}/postgres"))
            .await
            .expect("failed to connect to the postgres maintenance database");
        admin
            .execute(format!(r#"CREATE DATABASE "{name}""#).as_str())
            .await
            .unwrap_or_else(|e| panic!("failed to create scratch database {name}: {e}"));
        drop(admin);

        let pool = PgPool::connect(&format!("{server_url}/{name}"))
            .await
            .expect("failed to connect to the scratch database");
        trovato_kernel::db::run_migrations(&pool)
            .await
            .expect("failed to migrate the scratch database");

        Self {
            server_url,
            name,
            pool: Some(pool),
        }
    }

    fn pool(&self) -> &PgPool {
        self.pool.as_ref().expect("scratch pool was already closed")
    }

    fn storage(&self) -> DirectConfigStorage {
        DirectConfigStorage::new(self.pool().clone())
    }

    /// Drop the database. Explicit rather than in `Drop` because dropping a
    /// database is async and needs the pool closed first.
    async fn cleanup(mut self) {
        if let Some(pool) = self.pool.take() {
            pool.close().await;
        }
        if let Ok(mut admin) = PgConnection::connect(&format!("{}/postgres", self.server_url)).await
        {
            let _ = admin
                .execute(
                    format!(r#"DROP DATABASE IF EXISTS "{}" WITH (FORCE)"#, self.name).as_str(),
                )
                .await;
        }
    }
}

/// Path to the tutorial's config set.
fn tutorial_config_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/tutorial/config")
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
async fn tutorial_config_set_imports_clean_on_a_fresh_database() {
    let db = ScratchDb::new("tutorial").await;
    let storage = db.storage();
    let dir = tutorial_config_dir();

    let expected_files = std::fs::read_dir(&dir)
        .expect("tutorial config directory must exist")
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
            panic!("the tutorial config set must import clean: {e}");
        }
    };

    assert!(
        result.warnings.is_empty(),
        "the tutorial config set should import without warnings: {:?}",
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
    for expected in ["editor", "publisher", "viewer"] {
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
        ("0193a5a0-0000-7000-8000-000000000001", "live"),
        ("0193a5a0-0000-7000-8000-000000000002", "incoming"),
        ("0193a5a0-0000-7000-8000-000000000003", "curated"),
        ("0193a5a0-0000-7000-8000-000000000004", "legal_review"),
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
    assert_eq!(tiles, 5, "all five tutorial tiles should have landed");

    let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM menu_link")
        .fetch_one(db.pool())
        .await
        .expect("failed to count menu links");
    assert_eq!(links, 6, "all six tutorial menu links should have landed");

    db.cleanup().await;
}

/// Importing the tutorial set twice has to converge, not fail the second time.
///
/// This is what `Stage::create` generating its own UUID used to break: the first
/// run created stages under UUIDs the files did not declare, so the second run
/// did not find them and tried to create them again, colliding on the unique
/// `machine_name`.
#[tokio::test]
async fn tutorial_config_set_is_idempotent() {
    let db = ScratchDb::new("idempotent").await;
    let storage = db.storage();
    let dir = tutorial_config_dir();

    for run in 1..=2 {
        if let Err(e) = import_config(&storage, db.pool(), &dir, false).await {
            db.cleanup().await;
            panic!("import run {run} of the tutorial config set failed: {e}");
        }
    }

    let stages: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM stage_config")
        .fetch_one(db.pool())
        .await
        .expect("failed to count stages");
    assert_eq!(stages, 4, "a second import must not duplicate stages");

    let roles: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM roles WHERE name IN ('viewer', 'editor', 'publisher')",
    )
    .fetch_one(db.pool())
    .await
    .expect("failed to count roles");
    assert_eq!(roles, 3, "a second import must not duplicate roles");

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
        .env("DATABASE_URL", format!("{}/{}", db.server_url, db.name))
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
#[tokio::test]
async fn the_menu_tree_survives_export_and_re_import() {
    let first = ScratchDb::new("rtfirst").await;
    let second = ScratchDb::new("rtsecond").await;

    let source = TempConfigDir::new("rtsource");
    let (root, child, grandchild) = write_menu_tree(&source);

    if let Err(e) = import_config(&first.storage(), first.pool(), source.path(), false).await {
        first.cleanup().await;
        second.cleanup().await;
        panic!("the menu tree must import clean: {e:#}");
    }

    // Export everything the first database holds.
    let export_one = TempConfigDir::new("rtexport1");
    if let Err(e) = export_config(&first.storage(), first.pool(), export_one.path(), false).await {
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
    if let Err(e) = import_config(&second.storage(), second.pool(), export_one.path(), false).await
    {
        first.cleanup().await;
        second.cleanup().await;
        panic!("re-importing an export must succeed: {e:#}");
    }

    let export_two = TempConfigDir::new("rtexport2");
    if let Err(e) = export_config(&second.storage(), second.pool(), export_two.path(), false).await
    {
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

    first.cleanup().await;
    second.cleanup().await;
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
