#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Aliases, menu links, tiles, stages and a passwordless switch are written
//! through their services, so what a write must also do happens whichever
//! caller made it.

mod common;

use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::stage::{LIVE_STAGE_ID, StageVisibility, UpdateStage};
use trovato_kernel::models::{CreateItem, CreateMenuLink, CreateUrlAlias, MenuLink};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

fn menu_name() -> String {
    format!("b87b-{}", &Uuid::now_v7().simple().to_string()[20..])
}

fn forget_menu(app: &TestApp, menu: &str) {
    let pool = app.db.clone();
    let menu = menu.to_string();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM menu_link WHERE menu_name = $1")
            .bind(menu)
            .execute(&pool)
            .await;
    });
}

/// Deleting a link through the service moves its children up to its own
/// parent, in the delete's transaction, rather than to the top of the menu.
#[test]
fn a_deleted_links_children_move_up_one_level() {
    run_test(async {
        let app = shared_app().await;
        let menu = menu_name();
        forget_menu(app, &menu);
        let link = |path: &str, parent_id: Option<Uuid>| CreateMenuLink {
            menu_name: Some(menu.clone()),
            path: path.to_string(),
            title: path.to_string(),
            parent_id,
            weight: None,
            hidden: None,
            plugin: None,
            stage_id: None,
        };
        let links = app.state.menu_links();
        let top = links.create(link("/top", None)).await.unwrap();
        let middle = links.create(link("/middle", Some(top.id))).await.unwrap();
        let child = links.create(link("/child", Some(middle.id))).await.unwrap();

        assert!(links.delete(middle.id).await.unwrap());
        assert!(!links.delete(middle.id).await.unwrap(), "already gone");

        let child = MenuLink::find_by_id(&app.db, child.id)
            .await
            .unwrap()
            .expect("the child stays");
        assert_eq!(child.parent_id, Some(top.id));
    });
}

/// An alias set for an item is stored on the item's stage and in its
/// language, with its leading slash, and replaced rather than duplicated.
#[test]
fn an_item_alias_follows_the_items_stage_and_language() {
    run_test(async {
        let app = shared_app().await;
        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: "page".to_string(),
                    title: format!("b87b alias service {}", Uuid::now_v7().simple()),
                    author_id: Uuid::nil(),
                    status: Some(1),
                    promote: None,
                    sticky: None,
                    fields: Some(serde_json::json!({})),
                    stage_id: None,
                    language: Some("it".to_string()),
                    log: None,
                },
                &admin(),
            )
            .await
            .expect("create");
        let items = std::sync::Arc::clone(app.state.items());
        let id = item.id;
        common::defer_cleanup(move || async move {
            let _ = items.delete(id, &admin()).await;
        });

        let suffix = Uuid::now_v7().simple().to_string();
        let aliases = app.state.aliases();
        aliases
            .set_for_item(&item, &format!("b87b-first-{suffix}"))
            .await
            .unwrap();
        let set = aliases
            .set_for_item(&item, &format!("b87b-second-{suffix}"))
            .await
            .unwrap();
        assert_eq!(set.alias, format!("/b87b-second-{suffix}"));
        assert_eq!(set.language, "it");
        assert_eq!(set.stage_id, LIVE_STAGE_ID);
        let rows: Vec<String> = sqlx::query_scalar("SELECT alias FROM url_alias WHERE source = $1")
            .bind(format!("/item/{}", item.id))
            .fetch_all(&app.db)
            .await
            .unwrap();
        assert_eq!(rows, vec![format!("/b87b-second-{suffix}")]);

        assert_eq!(aliases.clear_for_item(&item).await.unwrap(), 1);

        let made = aliases
            .create(CreateUrlAlias {
                source: format!("item/{}", item.id),
                alias: format!("b87b-admin-{suffix}"),
                language: None,
                stage_id: None,
            })
            .await
            .unwrap();
        assert_eq!(made.source, format!("/item/{}", item.id));
        assert_eq!(made.alias, format!("/b87b-admin-{suffix}"));
        assert!(aliases.delete(made.id).await.unwrap());
    });
}

/// A stage made internal through the service is internal to the next access
/// decision: the stage view items are decided against is read, not cached.
#[test]
fn a_stage_visibility_change_holds_at_once() {
    run_test(async {
        let app = shared_app().await;
        let stage_id = common::create_test_stage(app, "b87bvis").await;
        let stages = app.state.stage();
        let visibility = || async {
            app.state
                .items()
                .stage_view()
                .await
                .unwrap()
                .stage(stage_id)
                .expect("the stage")
                .visibility
        };
        for wanted in [StageVisibility::Accessible, StageVisibility::Internal] {
            stages
                .update_stage(
                    stage_id,
                    UpdateStage {
                        visibility: Some(wanted.as_str().to_string()),
                        ..UpdateStage::default()
                    },
                )
                .await
                .unwrap()
                .expect("the stage");
            assert_eq!(visibility().await, wanted);
        }

        assert!(stages.delete_stage(stage_id).await.unwrap());
        assert!(stages.get_stage(stage_id).await.unwrap().is_none());
    });
}

/// Going passwordless through `UserService` clears the stored password and
/// the cached user with it.
#[test]
fn removing_a_password_goes_through_the_user_service() {
    run_test(async {
        let app = shared_app().await;
        let name = common::username("b87b-pwless");
        app.create_test_user(&name, "test-password-123", &format!("{name}@example.com"))
            .await;
        let id = common::user_id_of(app, &name).await;
        let users = app.state.users();
        assert!(!users.find_by_id(id).await.unwrap().unwrap().pass.is_empty());

        assert!(users.remove_password(id, &admin()).await.unwrap());
        assert!(
            users.find_by_id(id).await.unwrap().unwrap().pass.is_empty(),
            "the cached user has no password either"
        );
    });
}
