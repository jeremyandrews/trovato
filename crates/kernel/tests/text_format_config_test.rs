#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Text formats round trip through config export and import (B108), on
//! scratch databases of their own, as `config_import_test` does.
//!
//! Requires Postgres.

use std::path::PathBuf;

use trovato_kernel::config_storage::yaml::{export_config, import_config};
use trovato_kernel::config_storage::{ConfigEntity, ConfigStorage, DirectConfigStorage};
use trovato_kernel::content::text_format::{self, FilterKind, TextFormat};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "trovato_b108_{label}_{}",
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn formats_round_trip_through_config_export_and_import() {
    let db = trovato_test_utils::ScratchDb::create("b108_text_format").await;
    trovato_kernel::db::run_migrations(db.pool())
        .await
        .expect("migrate");
    let pool = db.pool();
    let storage = DirectConfigStorage::new(pool.clone());

    let format = TextFormat {
        format: "club_html".to_string(),
        name: "Club HTML".to_string(),
        weight: 7,
        filter: FilterKind::Html,
        allowed_tags: vec!["p".into(), "a".into()],
        allowed_attributes: [("a".to_string(), vec!["href".to_string()])]
            .into_iter()
            .collect(),
        convert_newlines: true,
        convert_urls: false,
    };
    text_format::save(pool, &format).await.unwrap();
    // A role granted the format, so the grant travels with it.
    let role = trovato_kernel::models::Role::create(pool, "club editors")
        .await
        .unwrap();
    trovato_kernel::models::Role::add_permission(pool, role.id, "use club_html")
        .await
        .unwrap();

    let out = TempDir::new("export");
    export_config(&storage, pool, &out.0, false)
        .await
        .expect("export");
    let yaml = std::fs::read_to_string(out.0.join("text_format.club_html.yml"))
        .expect("the format is exported");
    assert!(yaml.contains("Club HTML"), "{yaml}");
    for built_in in ["plain_text", "filtered_html", "full_html"] {
        assert!(
            out.0.join(format!("text_format.{built_in}.yml")).exists(),
            "{built_in} is exported"
        );
    }

    // Into a fresh database: the format and the role's grant both arrive.
    let fresh = trovato_test_utils::ScratchDb::create("b108_text_format_in").await;
    trovato_kernel::db::run_migrations(fresh.pool())
        .await
        .expect("migrate");
    let fresh_storage = DirectConfigStorage::new(fresh.pool().clone());
    import_config(&fresh_storage, fresh.pool(), &out.0, false)
        .await
        .expect("import");
    let back = text_format::find(fresh.pool(), "club_html")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(back, format, "round trip");
    let entity = fresh_storage
        .load("text_format", "club_html")
        .await
        .unwrap();
    assert!(matches!(entity, Some(ConfigEntity::TextFormat(_))));
    let role = trovato_kernel::models::Role::find_by_name(fresh.pool(), "club editors")
        .await
        .unwrap()
        .expect("the role is imported");
    let granted = trovato_kernel::models::Role::get_permissions(fresh.pool(), role.id)
        .await
        .unwrap();
    assert!(granted.iter().any(|p| p == "use club_html"), "{granted:?}");
    // The built-ins come through unchanged.
    assert_eq!(
        text_format::find(fresh.pool(), "filtered_html")
            .await
            .unwrap()
            .unwrap(),
        text_format::built_in()
            .into_iter()
            .find(|f| f.format == "filtered_html")
            .unwrap()
    );

    // An import allowing <script> is refused and changes nothing.
    let bad = TempDir::new("bad");
    std::fs::write(
        bad.0.join("text_format.club_html.yml"),
        yaml.replace("- p\n", "- script\n"),
    )
    .unwrap();
    assert!(
        import_config(&fresh_storage, fresh.pool(), &bad.0, false)
            .await
            .is_err(),
        "a format allowing <script> is not imported"
    );
    assert_eq!(
        text_format::find(fresh.pool(), "club_html")
            .await
            .unwrap()
            .unwrap(),
        format
    );
}
