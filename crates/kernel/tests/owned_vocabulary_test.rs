#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Owned vocabularies (B108).
//!
//! Drives the real `plugins/test_owned_vocabulary` wasm through the real
//! router: its manifest declares the `club` owner type and the route
//! `/club/{owner}/categories`; it creates a club's vocabulary through the
//! `vocabulary` host interface; and its `tap_vocabulary_owner_access` grants
//! a club's managers. The tests prove the migration leaves existing
//! vocabularies site-wide, the host interface creates and refuses as
//! documented, the owner route admits a manager and the declared permission
//! and refuses anonymous, plain and non-manager users, and an item reference
//! field limited to the item's owner refuses any other vocabulary's term.
//!
//! `test_owned_vocabulary` is `default_enabled = false`, so this file
//! installs it and builds its own `TestApp`, as `plugin_api_test` does.
//!
//! Requires Postgres + Redis and the fixture `.wasm` built into
//! `plugins/test_owned_vocabulary/`.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::TestApp;
use trovato_kernel::content::owned_vocabulary::TermsNotOwned;
use trovato_kernel::models::{Category, CreateItem, CreateTag};
use trovato_kernel::tap::UserContext;
use trovato_sdk::types::{FieldDefinition, FieldType};
use uuid::Uuid;

const PLUGIN: &str = "test_owned_vocabulary";
const PERM_CREATE: &str = "create test clubs";
const PERM_ALL_CLUBS: &str = "administer every club's categories";
const PASSWORD: &str = "correct-horse-battery-staple";

static APP: std::sync::OnceLock<TestApp> = std::sync::OnceLock::new();

fn app() -> &'static TestApp {
    APP.get_or_init(|| {
        let handle = common::shared_runtime_handle();
        std::thread::spawn(move || handle.block_on(build_app()))
            .join()
            .expect("owned vocabulary fixture app init thread panicked")
    })
}

async fn build_app() -> TestApp {
    trovato_test_utils::env::load_dotenv();
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    common::ensure_database_migrated(&database_url).await;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await
        .expect("failed to connect for fixture setup");
    trovato_kernel::plugin::status::install_plugin(&pool, PLUGIN, "1.0.0")
        .await
        .unwrap_or_else(|e| panic!("failed to install '{PLUGIN}': {e:#}"));
    pool.close().await;

    TestApp::with_config(|config| {
        if std::env::var_os("PLUGINS_DIR").is_none() {
            config.plugins_dirs = vec![common::project_root().join("plugins")];
        }
    })
    .await
}

fn unique(prefix: &str) -> String {
    format!("{prefix}_{}", &Uuid::now_v7().simple().to_string()[20..])
}

async fn user_id_of(app: &TestApp, username: &str) -> Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(username)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

/// Grant one permission to one user, through a role of one.
async fn grant(app: &TestApp, user: Uuid, permission: &str) {
    let role = format!("b108_ov_{}", Uuid::now_v7().simple());
    let role_id = common::create_test_role(app, &role).await;
    sqlx::query(
        "INSERT INTO role_permissions (role_id, permission) VALUES ($1, $2) \
         ON CONFLICT DO NOTHING",
    )
    .bind(role_id)
    .bind(permission)
    .execute(&app.db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(user)
        .bind(role_id)
        .execute(&app.db)
        .await
        .unwrap();
    app.state.permissions().invalidate_user(user);
}

/// A signed-in user, and their id.
async fn user(app: &TestApp, prefix: &str) -> (String, Uuid) {
    let name = unique(prefix);
    let cookies = app
        .create_and_login_user(&name, PASSWORD, &format!("{name}@test.local"))
        .await;
    let id = user_id_of(app, &name).await;
    (cookies, id)
}

async fn body_of(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 4_000_000)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(Request::get(uri).body(Body::empty()).unwrap(), cookies)
        .await;
    let status = response.status();
    (status, body_of(response).await)
}

async fn post_form(app: &TestApp, cookies: &str, uri: &str, body: String) -> StatusCode {
    app.request_with_cookies(
        Request::post(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
    .status()
}

/// A hidden input's value in `html`.
fn hidden(html: &str, name: &str) -> String {
    let needle = format!("name=\"{name}\"");
    let pos = html
        .find(&needle)
        .unwrap_or_else(|| panic!("no {name} in {}", &html[..html.len().min(300)]));
    let rel = html[pos..].find("value=\"").unwrap();
    let start = pos + rel + 7;
    let end = start + html[start..].find('"').unwrap();
    html[start..end].to_string()
}

/// A CSRF token for this session, from the login page.
async fn csrf(app: &TestApp, cookies: &str) -> String {
    let (_, html) = get(app, cookies, "/user/login").await;
    for name in ["_token", "csrf_token"] {
        if html.contains(&format!("name=\"{name}\"")) {
            return hidden(&html, name);
        }
    }
    panic!(
        "no CSRF token on the login page: {}",
        &html[..html.len().min(600)]
    );
}

/// Create a club and its vocabulary through the fixture's API, as a user
/// holding `create test clubs`. The plugin answers with what the host gave.
async fn create_club(
    app: &TestApp,
    cookies: &str,
    slug: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let token = csrf(app, cookies).await;
    let response = app
        .request_with_cookies(
            Request::post(format!("/tov/clubs/{slug}"))
                .header(header::CONTENT_TYPE, "application/json")
                .header("X-CSRF-Token", token)
                .body(Body::from(body.to_string()))
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let text = body_of(response).await;
    (
        status,
        serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text)),
    )
}

/// A club with one vocabulary, made through the plugin; its slug, id and
/// vocabulary id.
async fn club(app: &TestApp, creator: &str) -> (String, String, String) {
    let slug = unique("rowers").replace('_', "-");
    let id = unique("club");
    let vocabulary = unique("boats");
    let (status, body) = create_club(
        app,
        creator,
        &slug,
        serde_json::json!({"id": id, "vocabulary": vocabulary, "label": "Boat classes"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (slug, id, vocabulary)
}

async fn manage(app: &TestApp, club_id: &str, user: Uuid) {
    sqlx::query("INSERT INTO tov_manager (club_id, user_id) VALUES ($1, $2)")
        .bind(club_id)
        .bind(user)
        .execute(&app.db)
        .await
        .unwrap();
}

/// Leave the fixture disabled, so it does not load in other test binaries.
/// This app read its enabled set at construction and keeps it.
async fn disable_plugin(app: &TestApp) {
    sqlx::query("UPDATE plugin_status SET status = 0 WHERE name = $1")
        .bind(PLUGIN)
        .execute(&app.db)
        .await
        .ok();
}

/// A creator: a user holding `create test clubs`.
async fn creator(app: &TestApp) -> String {
    let (cookies, id) = user(app, "ov_creator").await;
    grant(app, id, PERM_CREATE).await;
    cookies
}

/// The migration adds the owner columns as NULL: the vocabularies a site
/// already has stay site-wide, listed and administered as before.
#[test]
fn existing_vocabularies_stay_site_wide() {
    common::run_test(async {
        let app = app();
        let id = unique("sitewide");
        sqlx::query("INSERT INTO category (id, label) VALUES ($1, 'Site wide')")
            .bind(&id)
            .execute(&app.db)
            .await
            .unwrap();
        let category = Category::find_by_id(&app.db, &id).await.unwrap().unwrap();
        assert_eq!(category.owner(), None);
        assert!(
            Category::list(&app.db)
                .await
                .unwrap()
                .iter()
                .any(|c| c.id == id)
        );
        let yaml = serde_yml::to_string(&category).unwrap();
        assert!(
            !yaml.contains("owner"),
            "a site-wide vocabulary exports as before: {yaml}"
        );
        // Half an owner is refused by the table itself.
        let half = sqlx::query("UPDATE category SET owner_type = 'club' WHERE id = $1")
            .bind(&id)
            .execute(&app.db)
            .await;
        assert!(half.is_err(), "owner_type without owner_id is refused");
        Category::delete(&app.db, &id).await.unwrap();
        disable_plugin(app).await;
    });
}

/// `vocabulary::create-owned` creates a vocabulary for a declared owner type,
/// and refuses an undeclared owner type and a taken id with their codes;
/// `list-owned` lists the owner's vocabularies.
#[test]
fn a_plugin_creates_and_lists_owned_vocabularies() {
    common::run_test(async {
        let app = app();
        let creator = creator(app).await;
        let slug = unique("eights").replace('_', "-");
        let club_id = unique("club");
        let vocabulary = unique("crews");

        let (status, created) = create_club(
            app,
            &creator,
            &slug,
            serde_json::json!({"id": club_id, "vocabulary": vocabulary, "label": "Crews"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        assert_eq!(created["owner_type"], "club");
        assert_eq!(created["owner_id"], club_id.as_str());
        assert_eq!(created["hierarchy"], 1);
        let stored = Category::find_by_id(&app.db, &vocabulary)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.owner(), Some(("club", club_id.as_str())));

        // The same id again: taken.
        let (status, refused) = create_club(
            app,
            &creator,
            &slug,
            serde_json::json!({"id": club_id, "vocabulary": vocabulary, "label": "Again"}),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            refused["error_code"],
            trovato_sdk::host_errors::ERR_VOCABULARY_EXISTS
        );

        // An owner type the plugin did not declare.
        let (status, refused) = create_club(
            app,
            &creator,
            &slug,
            serde_json::json!({
                "id": club_id, "vocabulary": unique("x"), "label": "X", "owner_type": "team"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            refused["error_code"],
            trovato_sdk::host_errors::ERR_VOCABULARY_OWNER_UNDECLARED
        );

        // A bad id.
        let (_, refused) = create_club(
            app,
            &creator,
            &slug,
            serde_json::json!({"id": club_id, "vocabulary": "Not A Name", "label": "X"}),
        )
        .await;
        assert_eq!(
            refused["error_code"],
            trovato_sdk::host_errors::ERR_VOCABULARY_INVALID
        );

        let (status, listed) = get(app, &creator, &format!("/tov/clubs/{slug}/vocabularies")).await;
        assert_eq!(status, StatusCode::OK, "{listed}");
        let listed: Vec<serde_json::Value> = serde_json::from_str(&listed).unwrap();
        let ids: Vec<&str> = listed.iter().filter_map(|v| v["id"].as_str()).collect();
        assert_eq!(ids, vec![vocabulary.as_str()]);
        disable_plugin(app).await;
    });
}

/// The owner route: anonymous users are sent to sign in, plain users and
/// non-managers get 403 on every route and write nothing, a manager and a
/// holder of the declared permission administer the terms with the kernel's
/// term list and term form, and another owner's vocabulary or an unknown
/// owner is 404.
#[test]
fn the_owner_route_admits_managers_and_refuses_everyone_else() {
    common::run_test(async {
        let app = app();
        let creator = creator(app).await;
        let (slug, club_id, vocabulary) = club(app, &creator).await;
        let (other_slug, _, other_vocabulary) = club(app, &creator).await;
        let base = format!("/club/{slug}/categories");
        let list = format!("{base}/{vocabulary}");
        let add = format!("{list}/add");

        // Anonymous: every route refuses (401 to a script; a browser is shown
        // the access-denied page with the login form), none answers.
        for uri in [&base, &list, &add] {
            let (status, _) = get(app, "", uri).await;
            assert!(
                status == StatusCode::UNAUTHORIZED,
                "anonymous GET {uri} answered {status}"
            );
        }

        // A plain user, and a manager of another club: 403.
        let (plain, _) = user(app, "ov_plain").await;
        let (other_manager, other_manager_id) = user(app, "ov_other").await;
        let other_club_id: String = sqlx::query_scalar("SELECT id FROM tov_club WHERE slug = $1")
            .bind(&other_slug)
            .fetch_one(&app.db)
            .await
            .unwrap();
        manage(app, &other_club_id, other_manager_id).await;
        for cookies in [&plain, &other_manager] {
            for uri in [&base, &list, &add] {
                let (status, _) = get(app, cookies, uri).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "GET {uri}");
            }
            let token = csrf(app, cookies).await;
            let status = post_form(
                app,
                cookies,
                &add,
                format!("_token={token}&_form_build_id=x&label=Intruder&weight=0"),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "POST {add}");
        }
        assert_eq!(
            app.state
                .categories()
                .count_tags(&vocabulary)
                .await
                .unwrap(),
            0,
            "a refused write wrote nothing"
        );

        // The manager.
        let (manager, manager_id) = user(app, "ov_manager").await;
        manage(app, &club_id, manager_id).await;
        let (status, html) = get(app, &manager, &base).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            html.contains("Boat classes"),
            "the owner's vocabulary is listed"
        );
        assert!(
            !html.contains(&other_vocabulary),
            "another owner's vocabulary is not"
        );

        let (status, form) = get(app, &manager, &add).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            form.replace("&#x2F;", "/")
                .contains(&format!("action=\"{add}\"")),
            "the kernel's term form"
        );
        let token = hidden(&form, "_token");
        let build = hidden(&form, "_form_build_id");
        let status = post_form(
            app,
            &manager,
            &add,
            format!(
                "_token={token}&_form_build_id={build}&label=Coxed+four&slug=coxed-four&weight=0"
            ),
        )
        .await;
        assert!(status.is_redirection(), "added: {status}");
        let (status, html) = get(app, &manager, &list).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Coxed four"), "the kernel's term list");
        let tag = app
            .state
            .categories()
            .list_tags(&vocabulary)
            .await
            .unwrap()
            .into_iter()
            .find(|t| t.label == "Coxed four")
            .expect("the term was created in the owner's vocabulary");
        // The add form again, now that the hierarchical vocabulary has a term
        // to offer as a parent; and a child term under it.
        let (status, form) = get(app, &manager, &add).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the add form with a parent to offer"
        );
        let token = hidden(&form, "_token");
        let build = hidden(&form, "_form_build_id");
        let status = post_form(
            app,
            &manager,
            &add,
            format!(
                "_token={token}&_form_build_id={build}&label=Coxed+four+B&weight=2&parent_id={}",
                tag.id
            ),
        )
        .await;
        assert!(status.is_redirection(), "child added: {status}");
        let child = app
            .state
            .categories()
            .list_tags(&vocabulary)
            .await
            .unwrap()
            .into_iter()
            .find(|t| t.label == "Coxed four B")
            .expect("the child term");
        let parents = app.state.categories().get_parents(child.id).await.unwrap();
        assert_eq!(parents.first().map(|p| p.id), Some(tag.id));

        // A parent from another owner's vocabulary is refused.
        let foreign_parent = app
            .state
            .categories()
            .create_tag(CreateTag {
                category_id: other_vocabulary.clone(),
                label: "Elsewhere".to_string(),
                description: None,
                slug: None,
                weight: None,
                parent_ids: None,
            })
            .await
            .unwrap();
        let (_, form) = get(app, &manager, &add).await;
        let token = hidden(&form, "_token");
        let build = hidden(&form, "_form_build_id");
        let status = post_form(
            app,
            &manager,
            &add,
            format!(
                "_token={token}&_form_build_id={build}&label=Smuggled&weight=3&parent_id={}",
                foreign_parent.id
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "the form comes back with the error");
        assert!(
            !app.state
                .categories()
                .list_tags(&vocabulary)
                .await
                .unwrap()
                .iter()
                .any(|t| t.label == "Smuggled"),
            "no term hangs off another owner's tree"
        );

        let edit = format!("{list}/terms/{}/edit", tag.id);
        assert!(
            html.replace("&#x2F;", "/").contains(&edit),
            "edit links stay under the owner route"
        );

        let (status, form) = get(app, &manager, &edit).await;
        assert_eq!(status, StatusCode::OK);
        let token = hidden(&form, "_token");
        let build = hidden(&form, "_form_build_id");
        let status = post_form(
            app,
            &manager,
            &edit,
            format!("_token={token}&_form_build_id={build}&label=Coxed+four+(4%2B)&slug=coxed-four&weight=1"),
        )
        .await;
        assert!(status.is_redirection(), "edited: {status}");
        let edited = app
            .state
            .categories()
            .get_tag(tag.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(edited.label, "Coxed four (4+)");

        // A non-manager cannot edit or delete the term either.
        let token = csrf(app, &plain).await;
        let status = post_form(
            app,
            &plain,
            &format!("{list}/terms/{}/delete", tag.id),
            format!("_token={token}"),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            app.state
                .categories()
                .get_tag(tag.id)
                .await
                .unwrap()
                .is_some()
        );

        // Another owner's vocabulary, under this owner's route: 404.
        let (status, _) = get(app, &manager, &format!("{base}/{other_vocabulary}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // A site-wide vocabulary under the owner route: 404.
        let (status, _) = get(app, &manager, &format!("{base}/tags")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // An owner the plugin does not know: 404.
        let (status, _) = get(app, &manager, "/club/no-such-club/categories").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // The declared permission admits a user who manages nothing.
        let (holder, holder_id) = user(app, "ov_holder").await;
        grant(app, holder_id, PERM_ALL_CLUBS).await;
        let (status, _) = get(app, &holder, &list).await;
        assert_eq!(status, StatusCode::OK);

        // The list's Delete opens a confirmation page; a post without its
        // form deletes nothing.
        let delete = format!("{list}/terms/{}/delete", tag.id);
        let (_, html) = get(app, &manager, &list).await;
        assert!(
            html.replace("&#x2F;", "/")
                .contains(&format!(r#"href="{delete}""#)),
            "the row's Delete links to its confirmation page"
        );
        let token = csrf(app, &manager).await;
        let status = post_form(app, &manager, &delete, format!("_token={token}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "a token alone is refused");
        assert!(
            app.state
                .categories()
                .get_tag(tag.id)
                .await
                .unwrap()
                .is_some()
        );

        // The manager deletes the term through the confirmation form.
        let (status, html) = get(app, &manager, &delete).await;
        assert_eq!(status, StatusCode::OK);
        let status = post_form(
            app,
            &manager,
            &delete,
            common::form_fields(&html, "category_tag_delete_form"),
        )
        .await;
        assert!(status.is_redirection(), "deleted: {status}");
        assert!(
            app.state
                .categories()
                .get_tag(tag.id)
                .await
                .unwrap()
                .is_none()
        );
        disable_plugin(app).await;
    });
}

/// A reference field limited to the item's owner (`settings.vocabulary_owner`)
/// takes only terms from vocabularies that owner owns: another club's term,
/// a site-wide term, or any term on an item with no owner is refused, on
/// create and on update.
#[test]
fn a_limited_reference_field_takes_only_the_owners_terms() {
    common::run_test(async {
        let app = app();
        let creator = creator(app).await;
        let (_, club_id, vocabulary) = club(app, &creator).await;
        let (_, _, other_vocabulary) = club(app, &creator).await;

        let term = |category: String, label: &str| CreateTag {
            category_id: category,
            label: label.to_string(),
            description: None,
            slug: None,
            weight: None,
            parent_ids: None,
        };
        let own = app
            .state
            .categories()
            .create_tag(term(vocabulary.clone(), "Eight"))
            .await
            .unwrap();
        let foreign = app
            .state
            .categories()
            .create_tag(term(other_vocabulary.clone(), "Pair"))
            .await
            .unwrap();
        let site_wide = app
            .state
            .categories()
            .create_tag(term("tags".to_string(), &unique("sitewide")))
            .await
            .unwrap();

        let item_type = unique("club_post");
        let fields = vec![
            FieldDefinition::new("field_club", FieldType::Text { max_length: None }).label("Club"),
            {
                let mut f = FieldDefinition::new(
                    "field_boats",
                    FieldType::RecordReference("category_term".into()),
                )
                .cardinality(-1)
                .label("Boats");
                f.settings = serde_json::json!({
                    "vocabulary_owner": {"owner_type": "club", "owner_field": "field_club"}
                });
                f
            },
        ];
        let settings = serde_json::json!({ "fields": fields });
        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ($1, 'Club post', 'B108 fixture', true, 'Title', 'core', $2)",
        )
        .bind(&item_type)
        .bind(&settings)
        .execute(&app.db)
        .await
        .unwrap();
        app.state
            .content_types()
            .replace(
                &item_type,
                "Club post",
                Some("B108 fixture"),
                settings.clone(),
            )
            .await
            .ok();

        let admin = UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()]);
        let create = |fields: serde_json::Value| CreateItem {
            item_type: item_type.clone(),
            title: "A club post".to_string(),
            author_id: Uuid::nil(),
            status: Some(1),
            promote: None,
            sticky: None,
            fields: Some(fields),
            stage_id: None,
            language: None,
            log: None,
        };
        let refused = |e: anyhow::Error| {
            e.downcast_ref::<TermsNotOwned>()
                .map(|r| r.0.iter().map(|(f, _)| f.clone()).collect::<Vec<_>>())
        };

        // The owner's own term: saved.
        let item = app
            .state
            .items()
            .create(
                create(serde_json::json!({
                    "field_club": club_id, "field_boats": [own.id.to_string()]
                })),
                &admin,
            )
            .await
            .expect("the owner's term is accepted");

        for (what, fields) in [
            (
                "another owner's term",
                serde_json::json!({"field_club": club_id, "field_boats": [foreign.id.to_string()]}),
            ),
            (
                "a site-wide term",
                serde_json::json!({"field_club": club_id, "field_boats": [{"target_id": site_wide.id.to_string()}]}),
            ),
            (
                "an item with no owner",
                serde_json::json!({"field_boats": [own.id.to_string()]}),
            ),
        ] {
            let e = app
                .state
                .items()
                .create(create(fields), &admin)
                .await
                .expect_err(what);
            assert_eq!(refused(e), Some(vec!["field_boats".to_string()]), "{what}");
        }

        // On update too.
        let e = app
            .state
            .items()
            .update(
                item.id,
                trovato_kernel::models::UpdateItem {
                    title: None,
                    status: None,
                    promote: None,
                    sticky: None,
                    fields: Some(serde_json::json!({
                        "field_club": club_id,
                        "field_boats": [own.id.to_string(), foreign.id.to_string()]
                    })),
                    log: None,
                    stage_id: None,
                    author_id: None,
                    created: None,
                },
                &admin,
            )
            .await
            .expect_err("a foreign term on update");
        assert_eq!(refused(e), Some(vec!["field_boats".to_string()]));

        sqlx::query("DELETE FROM item WHERE type = $1")
            .bind(&item_type)
            .execute(&app.db)
            .await
            .ok();
        sqlx::query("DELETE FROM item_type WHERE type = $1")
            .bind(&item_type)
            .execute(&app.db)
            .await
            .ok();
        disable_plugin(app).await;
    });
}

/// An owned vocabulary round-trips through config export and import with its
/// owner, on scratch databases of its own; half an owner is refused on import.
#[tokio::test]
async fn an_owned_vocabulary_round_trips_through_config() {
    use trovato_kernel::config_storage::DirectConfigStorage;
    use trovato_kernel::config_storage::yaml::{export_config, import_config};

    let temp = |label: &str| {
        let path =
            std::env::temp_dir().join(format!("trovato_b108_{label}_{}", Uuid::now_v7().simple()));
        std::fs::create_dir_all(&path).unwrap();
        path
    };

    let db = trovato_test_utils::ScratchDb::create("b108_owned_out").await;
    trovato_kernel::db::run_migrations(db.pool())
        .await
        .expect("migrate");
    let id = "club_boats";
    sqlx::query(
        "INSERT INTO category (id, label, owner_type, owner_id) VALUES ($1, 'Boats', 'club', 'club-7')",
    )
    .bind(id)
    .execute(db.pool())
    .await
    .unwrap();
    let out = temp("owned_export");
    export_config(
        &DirectConfigStorage::new(db.pool().clone()),
        db.pool(),
        &out,
        false,
    )
    .await
    .expect("export");
    let file = out.join(format!("category.{id}.yml"));
    let yaml = std::fs::read_to_string(&file).expect("the vocabulary is exported");
    assert!(yaml.contains("owner_type: club"), "{yaml}");
    let site_wide = std::fs::read_to_string(out.join("category.tags.yml")).expect("tags exported");
    assert!(
        !site_wide.contains("owner"),
        "a site-wide vocabulary exports as before"
    );

    let fresh = trovato_test_utils::ScratchDb::create("b108_owned_in").await;
    trovato_kernel::db::run_migrations(fresh.pool())
        .await
        .expect("migrate");
    import_config(
        &DirectConfigStorage::new(fresh.pool().clone()),
        fresh.pool(),
        &out,
        false,
    )
    .await
    .expect("import");
    let stored = Category::find_by_id(fresh.pool(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.owner(), Some(("club", "club-7")));
    let tags = Category::find_by_id(fresh.pool(), "tags")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tags.owner(), None);

    // Half an owner is refused, and nothing changes.
    let bad = temp("owned_bad");
    std::fs::write(
        bad.join(format!("category.{id}.yml")),
        yaml.replace("owner_id: club-7\n", "")
            .replace("Boats", "Changed"),
    )
    .unwrap();
    assert!(
        import_config(
            &DirectConfigStorage::new(fresh.pool().clone()),
            fresh.pool(),
            &bad,
            false
        )
        .await
        .is_err(),
        "owner_type without owner_id is not imported"
    );
    let unchanged = Category::find_by_id(fresh.pool(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.label, "Boats");
    let _ = std::fs::remove_dir_all(&out);
    let _ = std::fs::remove_dir_all(&bad);
}

// =============================================================================
// B189: an owned vocabulary is read only by someone who may see its owner
// =============================================================================

/// Every vocabulary id `GET /api/v1/categories` lists to `cookies`, every page.
async fn listed_vocabularies(app: &TestApp, cookies: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for page in 1.. {
        let (status, body) = get(
            app,
            cookies,
            &format!("/api/v1/categories?per_page=500&page={page}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        let data = json["data"].as_array().unwrap();
        if data.is_empty() {
            break;
        }
        ids.extend(data.iter().map(|c| c["id"].as_str().unwrap().to_string()));
        let total = json["total"].as_u64().unwrap() as usize;
        if ids.len() >= total {
            break;
        }
    }
    ids
}

/// Every route that reads one vocabulary or one of its terms.
fn read_routes(vocabulary: &str, term: Uuid) -> Vec<String> {
    vec![
        format!("/api/v1/category/{vocabulary}"),
        format!("/api/v1/category/{vocabulary}/tags"),
        format!("/api/v1/category/{vocabulary}/roots"),
        format!("/api/v1/tag/{term}"),
        format!("/api/v1/tag/{term}/parents"),
        format!("/api/v1/tag/{term}/children"),
        format!("/api/v1/tag/{term}/ancestors"),
        format!("/api/v1/tag/{term}/descendants"),
        format!("/api/v1/tag/{term}/breadcrumb"),
        // The unversioned alias serves the same handlers.
        format!("/api/category/{vocabulary}/tags"),
        format!("/api/tag/{term}"),
    ]
}

/// A root term and a child term in `vocabulary`.
async fn two_terms(app: &TestApp, vocabulary: &str, label: &str) -> (Uuid, Uuid) {
    let root = app
        .state
        .categories()
        .create_tag(CreateTag {
            category_id: vocabulary.to_string(),
            label: format!("{label} root"),
            description: None,
            slug: None,
            weight: Some(0),
            parent_ids: None,
        })
        .await
        .unwrap();
    let child = app
        .state
        .categories()
        .create_tag(CreateTag {
            category_id: vocabulary.to_string(),
            label: format!("{label} child"),
            description: None,
            slug: None,
            weight: Some(0),
            parent_ids: Some(vec![root.id]),
        })
        .await
        .unwrap();
    (root.id, child.id)
}

/// The visibility rule over the category API (B189). Anonymous callers, plain
/// users and another club's manager see the site-wide vocabularies only, and
/// get 404 (never 403) for an owned vocabulary and its terms on every read
/// route; a club's manager, whom the plugin grants `view`, sees that club's
/// vocabulary and not another club's; the declared permission, `administer
/// categories` and a site administrator see every club's; a site-wide
/// vocabulary reads byte for byte the same to every one of them.
#[test]
fn owned_vocabularies_are_hidden_from_public_category_reads() {
    common::run_test(async {
        let app = app();
        app.ensure_plugin_enabled("trovato_categories").await;
        let creator = creator(app).await;
        let (_, club_a, vocab_a) = club(app, &creator).await;
        let (_, _club_b, vocab_b) = club(app, &creator).await;
        let (root_a, child_a) = two_terms(app, &vocab_a, "Sculls").await;
        let (root_b, _) = two_terms(app, &vocab_b, "Sweeps").await;

        let site = unique("b189_site");
        sqlx::query("INSERT INTO category (id, label) VALUES ($1, 'Site wide')")
            .bind(&site)
            .execute(&app.db)
            .await
            .unwrap();
        let site_term = app
            .state
            .categories()
            .create_tag(CreateTag {
                category_id: site.clone(),
                label: "Oars".to_string(),
                description: None,
                slug: None,
                weight: Some(0),
                parent_ids: None,
            })
            .await
            .unwrap();

        let (plain, _) = user(app, "ov_b189_plain").await;
        let (manager_a, manager_a_id) = user(app, "ov_b189_mgr").await;
        manage(app, &club_a, manager_a_id).await;
        let (every_club, every_club_id) = user(app, "ov_b189_all").await;
        grant(app, every_club_id, PERM_ALL_CLUBS).await;
        let (categories_admin, categories_admin_id) = user(app, "ov_b189_cat").await;
        grant(app, categories_admin_id, "administer categories").await;
        let admin_name = unique("ov_b189_admin");
        let admin = app
            .create_and_login_admin(&admin_name, PASSWORD, &format!("{admin_name}@test.local"))
            .await;

        // Who sees which club's vocabulary.
        let callers: [(&str, &str, bool, bool); 6] = [
            ("anonymous", "", false, false),
            ("a plain user", &plain, false, false),
            ("club A's manager", &manager_a, true, false),
            (
                "a holder of the declared permission",
                &every_club,
                true,
                true,
            ),
            (
                "a holder of administer categories",
                &categories_admin,
                true,
                true,
            ),
            ("the site administrator", &admin, true, true),
        ];

        let site_category = format!(
            "{{\"id\":\"{site}\",\"label\":\"Site wide\",\"description\":null,\
             \"hierarchy\":0,\"weight\":0}}"
        );
        let site_tag = format!(
            "{{\"id\":\"{}\",\"category_id\":\"{site}\",\"label\":\"Oars\",\
             \"description\":null,\"weight\":0,\"created\":{},\"changed\":{}}}",
            site_term.id, site_term.created, site_term.changed
        );
        let site_tags =
            format!("{{\"data\":[{site_tag}],\"total\":1,\"page\":1,\"per_page\":100}}");

        for (who, cookies, sees_a, sees_b) in callers {
            let listed = listed_vocabularies(app, cookies).await;
            assert!(
                listed.contains(&site),
                "{who} lists the site-wide vocabulary"
            );
            assert!(listed.contains(&"tags".to_string()), "{who} lists tags");
            assert_eq!(listed.contains(&vocab_a), sees_a, "{who} lists club A's");
            assert_eq!(listed.contains(&vocab_b), sees_b, "{who} lists club B's");

            for (vocabulary, root, child, sees) in [
                (&vocab_a, root_a, Some(child_a), sees_a),
                (&vocab_b, root_b, None, sees_b),
            ] {
                let mut routes = read_routes(vocabulary, root);
                if let Some(child) = child {
                    routes.extend(read_routes(vocabulary, child));
                }
                for uri in routes {
                    let (status, body) = get(app, cookies, &uri).await;
                    if sees {
                        assert_eq!(status, StatusCode::OK, "{who}: GET {uri}: {body}");
                    } else {
                        assert_eq!(status, StatusCode::NOT_FOUND, "{who}: GET {uri}: {body}");
                        assert!(
                            !body.contains("Sculls") && !body.contains("Sweeps"),
                            "{who}: GET {uri} names no term: {body}"
                        );
                    }
                }
            }
            if sees_a {
                let (_, body) =
                    get(app, cookies, &format!("/api/v1/category/{vocab_a}/tags")).await;
                assert!(
                    body.contains("Sculls root") && body.contains("Sculls child"),
                    "{body}"
                );
                let (_, body) =
                    get(app, cookies, &format!("/api/v1/tag/{child_a}/breadcrumb")).await;
                assert!(body.contains("Sculls root"), "{who}: {body}");
            }

            // The site-wide vocabulary, byte for byte the same for everyone.
            let (status, body) = get(app, cookies, &format!("/api/v1/category/{site}")).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body, site_category, "{who}");
            for uri in [
                format!("/api/v1/category/{site}/tags"),
                format!("/api/v1/category/{site}/roots"),
            ] {
                let (status, body) = get(app, cookies, &uri).await;
                assert_eq!(status, StatusCode::OK);
                assert_eq!(body, site_tags, "{who}: {uri}");
            }
            let (status, body) = get(app, cookies, &format!("/api/v1/tag/{}", site_term.id)).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body, site_tag, "{who}");
        }

        // An id that names nothing answers as a hidden one does, so a 404
        // says nothing about whether an owned vocabulary exists.
        for uri in read_routes("b189_no_such_vocabulary", Uuid::now_v7()) {
            let (status, body) = get(app, "", &uri).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "GET {uri}: {body}");
        }

        // The pager counts what the caller may see: the total an anonymous
        // caller is told is the number of vocabularies they can list.
        let (_, body) = get(app, "", "/api/v1/categories?per_page=1").await;
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        let anonymous_listed = listed_vocabularies(app, "").await;
        assert_eq!(
            json["total"].as_u64().unwrap() as usize,
            anonymous_listed.len()
        );

        sqlx::query("DELETE FROM category WHERE id = $1")
            .bind(&site)
            .execute(&app.db)
            .await
            .ok();
        disable_plugin(app).await;
    });
}
