#![allow(clippy::unwrap_used, clippy::expect_used)]
//! S4 — the one-time grant of `publish content` on upgrade.
//!
//! Whether an item is published used to come straight from the client, with no
//! permission behind it. `publish content` is that permission, and a migration
//! grants it once to every role that could already create or edit content, so
//! no existing site loses the ability to publish the day it upgrades.
//!
//! The shape of this file is
//! `access_administration_pages_test::the_migration_grants_admission_to_a_role_holding_administer_site`,
//! which pins the same kind of grant for `access administration pages`: assert
//! the migration ran, then apply its own statement to fixtures of this test's
//! own making, scoped by `role_id` so it cannot reach another test's role.
//!
//! Its own file rather than that one's, because that file is scoped to the
//! admission permission — its header says so, and every other test in it is
//! about `/admin`. The grant tested here is about content.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use common::{TestApp, run_test, shared_app};
use trovato_kernel::models::Role;
use uuid::Uuid;

const PUBLISH: &str = "publish content";

/// The migration's own statement, with one addition: a `role_id` filter, so it
/// grants to this test's fixtures and cannot reach a role another test is
/// using. Without it a blanket grant would hand `publish content` to every role
/// in the database, including the ones
/// `item_form_roundtrip_test` builds to prove a user *without* it is refused —
/// and the binaries run in parallel.
async fn apply_migration_statement(app: &TestApp, role_ids: &[Uuid]) {
    sqlx::query(
        "INSERT INTO role_permissions (role_id, permission) \
         SELECT DISTINCT role_id, 'publish content' \
         FROM role_permissions \
         WHERE (permission IN ('create content', 'edit own content', 'edit any content') \
                OR permission LIKE 'create % content') \
           AND role_id = ANY($1) \
         ON CONFLICT DO NOTHING",
    )
    .bind(role_ids)
    .execute(&app.db)
    .await
    .expect("apply the migration statement");
}

async fn role_with(app: &TestApp, permission: &str) -> Role {
    let role = Role::create(&app.db, &format!("prepublish-{}", Uuid::now_v7().simple()))
        .await
        .expect("create role");
    common::track_test_role(&app.db, role.id);
    Role::add_permission(&app.db, role.id, permission)
        .await
        .unwrap_or_else(|e| panic!("grant `{permission}`: {e}"));
    role
}

async fn holds_publish(app: &TestApp, role: &Role) -> bool {
    Role::get_permissions(&app.db, role.id)
        .await
        .expect("read permissions")
        .iter()
        .any(|p| p == PUBLISH)
}

#[test]
fn the_migration_grants_publish_to_every_role_that_could_create_or_edit() {
    run_test(async {
        let app = shared_app().await;

        // The migration is recorded as applied: the statement below is the
        // shipped one, not a statement invented by this test.
        let applied: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM _sqlx_migrations WHERE version = 20261004000001)",
        )
        .fetch_one(&app.db)
        .await
        .expect("query the migration ledger");
        assert!(applied, "the grant migration must have run");

        // Four roles as a pre-upgrade site had them. The first three could put
        // content on the site; the fourth could only read it.
        let generic = role_with(app, "create content").await;
        let per_type = role_with(app, "create conference content").await;
        let own = role_with(app, "edit own content").await;
        let reader = role_with(app, "access content").await;

        for role in [&generic, &per_type, &own, &reader] {
            assert!(
                !holds_publish(app, role).await,
                "the fixtures must start without `{PUBLISH}`"
            );
        }

        let ids = vec![generic.id, per_type.id, own.id, reader.id];
        // Twice: `ON CONFLICT DO NOTHING` is what makes it safe to re-run on a
        // restored database, and a second upgrade would run it again.
        apply_migration_statement(app, &ids).await;
        apply_migration_statement(app, &ids).await;

        assert!(
            holds_publish(app, &generic).await,
            "`create content` must be granted `{PUBLISH}`"
        );
        assert!(
            holds_publish(app, &per_type).await,
            "a per-type create permission must be granted `{PUBLISH}` too — that is \
             the shape `/item/add/{{type}}` and every plugin's content types use"
        );
        assert!(
            holds_publish(app, &own).await,
            "`edit own content` must be granted `{PUBLISH}`"
        );

        // And the correction reaches no further than that: it is a one-time
        // grant to roles that could already publish, not a rule that reading
        // content implies publishing it.
        assert!(
            !holds_publish(app, &reader).await,
            "`access content` alone must not be granted `{PUBLISH}`"
        );

        // Exactly one row per role, which is the `DISTINCT` and the conflict
        // clause doing their job across two runs.
        let rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM role_permissions WHERE permission = $1 AND role_id = $2",
        )
        .bind(PUBLISH)
        .bind(generic.id)
        .fetch_one(&app.db)
        .await
        .unwrap();
        assert_eq!(
            rows, 1,
            "re-running the migration must not duplicate a grant"
        );
    });
}

/// `publish content` is in `KERNEL_PERMISSIONS`, so the permission grid renders
/// it and `config import` accepts it in a `role.*.yml`. A permission a site
/// cannot grant deliberately would make the one-time migration the only way to
/// ever hold it.
#[test]
fn the_permission_is_one_the_kernel_declares() {
    assert!(
        trovato_kernel::models::role::KERNEL_PERMISSIONS.contains(&PUBLISH),
        "`{PUBLISH}` must be declared, or no site can grant it"
    );
}
