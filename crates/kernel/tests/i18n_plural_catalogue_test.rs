#![allow(clippy::unwrap_used, clippy::expect_used)]
//! A plural interface string's translation goes into the locale catalogue
//! (`locale_string`, PostgreSQL) and comes back out with one form per plural
//! category of the language.
//!
//! The catalogue key and the stored forms are joined by a separator that
//! PostgreSQL `text` can hold; a NUL, as a compiled gettext catalogue uses,
//! would be refused by the database and every plural would stay English.

mod common;

use std::sync::Arc;

use common::{run_test, shared_app};
use trovato_kernel::i18n::{self, Message, PLURAL_SEPARATOR};
use trovato_kernel::services::locale::LocaleService;

const CONTEXT: &str = "i18n plural catalogue test";

#[test]
fn a_plural_translation_round_trips_through_the_catalogue() {
    run_test(async {
        let app = shared_app().await;
        common::ensure_plugin_tables(app, "trovato_locale");

        let singular = "{count} file was removed.";
        let plural = "{count} files were removed.";
        // Russian: one, few, many, other.
        let forms = [
            "Удалён {count} файл.",
            "Удалено {count} файла.",
            "Удалено {count} файлов.",
            "Удалено {count} файла (дробь).",
        ]
        .join(&PLURAL_SEPARATOR.to_string());

        let writer = LocaleService::new(
            app.db.clone(),
            Arc::new(trovato_kernel::metrics::Metrics::new()),
        );
        writer
            .import_translations(
                "ru",
                &[(
                    i18n::plural_source_key(singular, plural),
                    forms,
                    CONTEXT.to_string(),
                )],
            )
            .await
            .expect("the plural translation is stored");

        // A second service reads it back from the database, not the writer's cache.
        let reader = LocaleService::new(
            app.db.clone(),
            Arc::new(trovato_kernel::metrics::Metrics::new()),
        );
        reader.load_language("ru").await.unwrap();
        let render = |n: i64| {
            i18n::translate(
                Some(&reader),
                "ru",
                &Message {
                    source: singular,
                    plural: Some(plural),
                    count: Some(n),
                    context: CONTEXT,
                },
                &[],
            )
        };
        let (one, few, many) = (render(1), render(3), render(5));

        sqlx::query("DELETE FROM locale_string WHERE context = $1")
            .bind(CONTEXT)
            .execute(&app.db)
            .await
            .unwrap();

        assert_eq!(one, "Удалён 1 файл.");
        assert_eq!(few, "Удалено 3 файла.");
        assert_eq!(many, "Удалено 5 файлов.");
    });
}
