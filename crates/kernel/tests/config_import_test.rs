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
use trovato_kernel::config_storage::yaml::{ConfigImportFailed, import_config};
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
