#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The two per content type search switches: "Include this content type in
//! search" and "Include comments in search", at
//! `/admin/structure/types/{type}/search`.
//!
//! Both default to on, so a site that never opens the screen searches
//! everything. Saving goes through the form pipeline (CSRF and form build id),
//! recomputes the type's full text vectors, and asks for a Pagefind rebuild;
//! both backends then leave out what was switched off. The switches live in the
//! type's `settings`, so config export and import carry them.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app, user_holding};
use trovato_kernel::models::comment::CommentStatus;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateComment, CreateItem};
use trovato_kernel::search::SearchSwitches;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

/// A fresh type of this test's own, with a body field.
async fn fresh_type(app: &TestApp) -> String {
    use trovato_sdk::types::{FieldDefinition, FieldType};
    let name = format!("b106sw_{}", &Uuid::now_v7().simple().to_string()[20..]);
    let fields = vec![FieldDefinition::new("field_body", FieldType::TextLong).label("Body")];
    app.state
        .content_types()
        .create(
            &name,
            "Switch probe",
            Some("search switches fixture"),
            serde_json::json!({ "fields": serde_json::to_value(&fields).unwrap() }),
        )
        .await
        .expect("create the fixture type");

    // The type, and the items on it (whose comments cascade), go when the test
    // ends: every run names a new type, so leaving it would leave one more
    // behind on every run.
    let pool = app.db.clone();
    let registry = std::sync::Arc::clone(app.state.content_types());
    let machine_name = name.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE type = $1")
            .bind(&machine_name)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM item_type WHERE type = $1")
            .bind(&machine_name)
            .execute(&pool)
            .await;
        registry.invalidate(&machine_name);
    });
    name
}

/// An item of `item_type` whose body says `body_word`, with one published
/// comment saying `comment_word`.
async fn item_with_comment(
    app: &TestApp,
    item_type: &str,
    body_word: &str,
    comment_word: &str,
) -> Uuid {
    let id = app
        .state
        .items()
        .create(
            CreateItem {
                item_type: item_type.to_string(),
                title: "Switch probe item".to_string(),
                author_id: Uuid::nil(),
                status: Some(1),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(serde_json::json!({ "field_body": { "value": body_word } })),
                stage_id: Some(LIVE_STAGE_ID),
                language: Some("en".to_string()),
                log: Some("search switches fixture".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create")
        .id;
    app.state
        .comments()
        .create(
            CreateComment {
                item_id: id,
                parent_id: None,
                author_id: Uuid::nil(),
                subject: None,
                body: format!("A comment saying {comment_word}"),
                body_format: None,
                status: Some(CommentStatus::Published.as_i16()),
            },
            &admin(),
        )
        .await
        .expect("comment");
    id
}

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).replace("&#x2F;", "/")
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(Request::get(uri).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

fn input_value(html: &str, name: &str) -> Option<String> {
    let needle = format!(r#"name="{name}""#);
    let at = html.find(&needle)?;
    let start = html[..at].rfind('<')?;
    let end = at + html[at..].find('>')?;
    let tag = &html[start..end];
    let vat = tag.find(r#"value=""#)? + 7;
    let vend = tag[vat..].find('"')? + vat;
    Some(tag[vat..vend].to_string())
}

/// The switches form, as the page renders it.
fn switches_form(html: &str, action: &str) -> String {
    let at = html
        .find(&format!(r#"action="{action}""#))
        .unwrap_or_else(|| panic!("a form posting to {action}: {html}"));
    let scoped = &html[html[..at].rfind("<form").unwrap()..];
    scoped[..scoped.find("</form>").unwrap()].to_string()
}

/// Whether the form's checkbox `name` is checked.
fn checked(form: &str, name: &str) -> bool {
    let at = form
        .find(&format!(r#"name="{name}""#))
        .unwrap_or_else(|| panic!("a {name} checkbox: {form}"));
    let start = form[..at].rfind('<').unwrap();
    let end = at + form[at..].find('>').unwrap();
    form[start..end].contains("checked")
}

/// Load the page, post the form with the two boxes as given, follow nothing.
async fn save_switches(
    app: &TestApp,
    cookies: &str,
    item_type: &str,
    include: bool,
    comments: bool,
) -> StatusCode {
    let uri = format!("/admin/structure/types/{item_type}/search");
    let (status, html) = get(app, cookies, &uri).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    let form = switches_form(&html, &uri);
    let mut body = format!(
        "_token={}&_form_build_id={}&op=Save",
        urlencoding::encode(&input_value(&form, "_token").unwrap()),
        urlencoding::encode(&input_value(&form, "_form_build_id").unwrap()),
    );
    if include {
        body.push_str("&include=1");
    }
    if comments {
        body.push_str("&comments=1");
    }
    app.request_with_cookies(
        Request::builder()
            .method("POST")
            .uri(&uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
    .status()
}

/// What each backend finds for `word`, as the anonymous visitor.
async fn found(app: &TestApp, word: &str) -> (Vec<Uuid>, Vec<Uuid>) {
    let docs = trovato_kernel::cron::indexable_documents(&app.db, app.state.items())
        .await
        .expect("export");
    let pagefind = docs
        .iter()
        .filter(|d| trovato_kernel::cron::render_fragment(d).contains(word))
        .map(|d| d.id)
        .collect();
    let postgres = app
        .state
        .search()
        .search_unrecorded(word, &[LIVE_STAGE_ID], None, 50, 0)
        .await
        .expect("search")
        .results
        .into_iter()
        .map(|r| r.id)
        .collect();
    (pagefind, postgres)
}

#[test]
fn both_switches_default_to_on() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_plugin_enabled("trovato_comments").await;
        let item_type = fresh_type(app).await;

        assert_eq!(
            SearchSwitches::from_settings(&serde_json::json!({})),
            SearchSwitches::default()
        );
        assert!(SearchSwitches::default().include && SearchSwitches::default().comments);
        assert_eq!(
            SearchSwitches::from_settings(&serde_json::json!({ "search": { "include": false } })),
            SearchSwitches {
                include: false,
                comments: true
            },
            "only an explicit false turns one off"
        );

        let (_, cookies) = user_holding(app, "b106swd", &["administer site"]).await;
        let uri = format!("/admin/structure/types/{item_type}/search");
        let (status, html) = get(app, &cookies, &uri).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains("Include this content type in search"),
            "{html}"
        );
        assert!(html.contains("Include comments in search"), "{html}");
        let form = switches_form(&html, &uri);
        assert!(
            checked(&form, "include"),
            "include is on by default: {form}"
        );
        assert!(
            checked(&form, "comments"),
            "comments are on by default: {form}"
        );

        // And a fresh type's item and comment are found by both backends.
        let tag = Uuid::now_v7().simple().to_string();
        let (body, comment) = (format!("zqswbody{tag}"), format!("zqswcomment{tag}"));
        let id = item_with_comment(app, &item_type, &body, &comment).await;
        assert_eq!(found(app, &body).await, (vec![id], vec![id]));
        assert_eq!(found(app, &comment).await, (vec![id], vec![id]));
    });
}

#[test]
fn the_switches_take_a_type_or_its_comments_out_of_both_backends() {
    run_test(async {
        let app = shared_app().await;
        app.ensure_plugin_enabled("trovato_comments").await;
        common::ensure_plugin_tables(app, "trovato_search");
        let item_type = fresh_type(app).await;
        let tag = Uuid::now_v7().simple().to_string();
        let (body, comment) = (format!("zqswbody{tag}"), format!("zqswcomment{tag}"));
        let id = item_with_comment(app, &item_type, &body, &comment).await;
        let (_, cookies) = user_holding(app, "b106sws", &["administer site"]).await;

        // Without the form token nothing changes.
        let uri = format!("/admin/structure/types/{item_type}/search");
        let response = app
            .request_with_cookies(
                Request::builder()
                    .method("POST")
                    .uri(&uri)
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from("comments=1"))
                    .unwrap(),
                &cookies,
            )
            .await;
        assert!(
            !response.status().is_redirection(),
            "a post without a token is refused"
        );
        assert_eq!(found(app, &body).await, (vec![id], vec![id]));

        // Comments off: the comment only match goes, the item stays.
        sqlx::query("UPDATE pagefind_index_status SET rebuild_requested = false WHERE id = 1")
            .execute(&app.db)
            .await
            .unwrap();
        assert_eq!(
            save_switches(app, &cookies, &item_type, true, false).await,
            StatusCode::SEE_OTHER
        );
        let rebuild: bool =
            sqlx::query_scalar("SELECT rebuild_requested FROM pagefind_index_status WHERE id = 1")
                .fetch_one(&app.db)
                .await
                .unwrap();
        assert!(rebuild, "saving the switches requests a rebuild");
        assert_eq!(
            found(app, &comment).await,
            (vec![], vec![]),
            "comments are out"
        );
        assert_eq!(
            found(app, &body).await,
            (vec![id], vec![id]),
            "the item is still in"
        );
        let (_, html) = get(app, &cookies, &uri).await;
        let form = switches_form(&html, &uri);
        assert!(
            checked(&form, "include") && !checked(&form, "comments"),
            "{form}"
        );

        // The type off: its items go from both backends.
        assert_eq!(
            save_switches(app, &cookies, &item_type, false, false).await,
            StatusCode::SEE_OTHER
        );
        assert_eq!(found(app, &body).await, (vec![], vec![]), "the type is out");

        // Both back on: everything returns.
        assert_eq!(
            save_switches(app, &cookies, &item_type, true, true).await,
            StatusCode::SEE_OTHER
        );
        assert_eq!(found(app, &body).await, (vec![id], vec![id]));
        assert_eq!(found(app, &comment).await, (vec![id], vec![id]));

        // A user without the permission may not change them.
        let (_, plain) = user_holding(app, "b106swp", &[]).await;
        let (status, _) = get(app, &plain, &uri).await;
        assert_ne!(status, StatusCode::OK);
    });
}

#[test]
fn config_export_and_import_keep_the_switches() {
    run_test(async {
        use trovato_kernel::config_storage::DirectConfigStorage;
        use trovato_kernel::config_storage::yaml::{export_config, import_config};

        let source = trovato_test_utils::ScratchDb::create("b106sw_export").await;
        trovato_kernel::db::run_migrations(source.pool())
            .await
            .expect("migrate");
        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ('b106sw_roundtrip', 'Round trip', 'switches', true, 'Title', 'core', \
                     '{\"fields\": [], \"search\": {\"include\": true, \"comments\": false}}'::jsonb)",
        )
        .execute(source.pool())
        .await
        .expect("seed");

        let dir = std::env::temp_dir().join(format!("b106sw_{}", Uuid::now_v7().simple()));
        export_config(
            &DirectConfigStorage::new(source.pool().clone()),
            source.pool(),
            &dir,
            true,
        )
        .await
        .expect("export");

        let target = trovato_test_utils::ScratchDb::create("b106sw_import").await;
        trovato_kernel::db::run_migrations(target.pool())
            .await
            .expect("migrate");
        import_config(
            &DirectConfigStorage::new(target.pool().clone()),
            target.pool(),
            &dir,
            false,
        )
        .await
        .expect("import");

        let settings: serde_json::Value =
            sqlx::query_scalar("SELECT settings FROM item_type WHERE type = 'b106sw_roundtrip'")
                .fetch_one(target.pool())
                .await
                .expect("the type was imported");
        assert_eq!(
            SearchSwitches::from_settings(&settings),
            SearchSwitches {
                include: true,
                comments: false
            },
            "the switches survive export and import: {settings}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    });
}
