#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Config import runs the same field value check the item service runs: an
//! imported item with more values than a field's cardinality allows, or a tag
//! from the wrong category in a `CategoryReference` field, is refused.
//!
//! Config import wrote items with its own SQL, so `cardinality` and the field
//! kind meant nothing to it (Ritrovo `G-CARDINALITY-IS-INERT`).
//!
//! Its own scratch database, as `file_reference_import_test.rs` uses, because
//! it imports config sets.

use std::path::PathBuf;

use trovato_kernel::config_storage::DirectConfigStorage;
use trovato_kernel::config_storage::yaml::import_config;
use uuid::Uuid;

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("trovato_b72_{label}_{}", Uuid::now_v7().simple()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.0.join(name), body).unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const RUST: &str = "0193b72a-0000-7000-8000-000000000001";
const PYTHON: &str = "0193b72a-0000-7000-8000-000000000002";
const GO: &str = "0193b72a-0000-7000-8000-000000000003";
const HALL: &str = "0193b72a-0000-7000-8000-000000000004";
const WEB: &str = "0193b72a-0000-7000-8000-000000000005";

fn tag(id: &str, category: &str, label: &str) -> String {
    format!(
        "id: '{id}'\ncategory_id: {category}\nlabel: {label}\ndescription: null\n\
         slug: {slug}\nweight: 0\ncreated: 1767225600\nchanged: 1767225600\n",
        slug = label.to_lowercase()
    )
}

/// A config set with two categories, their tags, and a type whose
/// `field_topics` holds at most three topics.
fn base_set(label: &str) -> TempDir {
    let dir = TempDir::new(label);
    dir.write(
        "category.b72_topics.yml",
        "id: b72_topics\nlabel: Topics\ndescription: null\nhierarchy: 1\nweight: 0\n",
    );
    dir.write(
        "category.b72_venues.yml",
        "id: b72_venues\nlabel: Venues\ndescription: null\nhierarchy: 0\nweight: 0\n",
    );
    dir.write(&format!("tag.{RUST}.yml"), &tag(RUST, "b72_topics", "Rust"));
    dir.write(
        &format!("tag.{PYTHON}.yml"),
        &tag(PYTHON, "b72_topics", "Python"),
    );
    dir.write(&format!("tag.{GO}.yml"), &tag(GO, "b72_topics", "Go"));
    dir.write(&format!("tag.{WEB}.yml"), &tag(WEB, "b72_topics", "Web"));
    dir.write(&format!("tag.{HALL}.yml"), &tag(HALL, "b72_venues", "Hall"));
    dir.write(
        "item_type.b72_talk.yml",
        "type: b72_talk\nlabel: Talk\ndescription: null\nhas_title: true\n\
         title_label: Title\nplugin: core\nsettings:\n  fields:\n\
         \x20 - field_name: field_topics\n    field_type:\n      CategoryReference: b72_topics\n\
         \x20   label: Topics\n    cardinality: 3\n",
    );
    dir
}

fn item(dir: &TempDir, id: Uuid, topics: &[&str]) {
    let list: String = topics.iter().map(|t| format!("  - '{t}'\n")).collect();
    dir.write(
        &format!("item.{id}.yml"),
        &format!("id: {id}\ntype: b72_talk\ntitle: Imported Talk\nstatus: 1\nfields:\n  field_topics:\n{list}"),
    );
}

async fn stored(pool: &sqlx::PgPool, id: Uuid) -> Option<serde_json::Value> {
    sqlx::query_scalar("SELECT fields FROM item WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn config_import_runs_the_field_value_check() {
    let db = trovato_test_utils::ScratchDb::create("b72_tagref_import").await;
    let pool = db.pool();
    trovato_kernel::db::run_migrations(pool).await.unwrap();
    let storage = DirectConfigStorage::new(pool.clone());

    // Too many topics.
    let dir = base_set("too_many");
    let too_many = Uuid::now_v7();
    item(&dir, too_many, &[RUST, PYTHON, GO, WEB]);
    let err = import_config(&storage, pool, &dir.0, false)
        .await
        .expect_err("four topics in a field that allows three");
    assert!(
        format!("{err:#}").contains("Topics allows at most 3 values"),
        "{err:#}"
    );
    assert!(stored(pool, too_many).await.is_none(), "nothing was stored");

    // A venue in the topics field.
    let dir = base_set("wrong_category");
    let wrong = Uuid::now_v7();
    item(&dir, wrong, &[HALL]);
    let err = import_config(&storage, pool, &dir.0, false)
        .await
        .expect_err("a venue is not a topic");
    assert!(format!("{err:#}").contains(HALL), "{err:#}");
    assert!(stored(pool, wrong).await.is_none(), "nothing was stored");

    // Within the limits it imports as written.
    let dir = base_set("valid");
    let valid = Uuid::now_v7();
    item(&dir, valid, &[RUST, PYTHON, GO]);
    import_config(&storage, pool, &dir.0, false)
        .await
        .expect("three topics import");
    assert_eq!(
        stored(pool, valid).await.unwrap()["field_topics"],
        serde_json::json!([RUST, PYTHON, GO])
    );
}
