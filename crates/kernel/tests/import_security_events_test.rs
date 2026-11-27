#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `trovato import` writes the same account security events as every other
//! path that creates or changes an account.
//!
//! The importer writes `users` with its own SQL inside one transaction (it
//! keeps the file's ids and dates, and a migrated account gets no
//! `tap_user_register` welcome mail), so it never passed through
//! `UserService`, where every other path's `account.created`,
//! `user.name_changed`, `user.mail_changed`, `user.status_changed` and
//! `password.*` events are written. An import created and changed accounts
//! with nothing in the security log to show it. It now queues the events,
//! built by the same functions, and writes them once the run commits.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use common::{TestApp, run_test, shared_app};
use serde_json::json;
use trovato_kernel::import::{ImportReport, import};
use uuid::Uuid;

async fn run(
    app: &TestApp,
    lines: &[serde_json::Value],
    dry_run: bool,
) -> anyhow::Result<ImportReport> {
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    import(&app.state, text.as_bytes(), dry_run).await
}

fn argon2_hash(password: &str) -> String {
    use argon2::password_hash::{PasswordHasher, SaltString, rand_core::OsRng};
    argon2::Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .unwrap()
        .to_string()
}

fn user_line(id: Uuid, name: &str) -> serde_json::Value {
    json!({
        "type": "user", "id": id, "name": name, "mail": format!("{name}@example.com"),
        "created": "2012-05-06T07:08:09Z"
    })
}

fn forget_users(app: &TestApp, ids: Vec<Uuid>) {
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        for id in ids {
            let _ = sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(id)
                .execute(&pool)
                .await;
        }
    });
}

/// `(kind, actor_id, details)` of every security event about `user_id`,
/// oldest first.
async fn events(app: &TestApp, user_id: Uuid) -> Vec<(String, Option<Uuid>, serde_json::Value)> {
    sqlx::query_as(
        "SELECT kind, actor_id, details FROM security_audit_log \
         WHERE user_id = $1 ORDER BY created, id",
    )
    .bind(user_id)
    .fetch_all(&app.db)
    .await
    .unwrap()
}

fn of_kind<'a>(
    events: &'a [(String, Option<Uuid>, serde_json::Value)],
    kind: &str,
) -> Vec<&'a serde_json::Value> {
    events
        .iter()
        .filter(|(k, _, _)| k == kind)
        .map(|(_, _, d)| d)
        .collect()
}

/// A new account in the file writes exactly one `account.created`, naming
/// the import as the path and the operator as who did it, and its role
/// grant one `user.roles_changed`.
#[test]
fn an_imported_account_writes_one_account_created_event() {
    run_test(async {
        let app = shared_app().await;
        let (active, blocked) = (Uuid::now_v7(), Uuid::now_v7());
        forget_users(app, vec![active, blocked]);
        let mut first = user_line(active, &common::username("b223imp_a"));
        let role = format!("b223imp_role_{}", active.simple());
        common::create_test_role(app, &role).await;
        first["roles"] = json!([role]);
        let mut second = user_line(blocked, &common::username("b223imp_b"));
        second["status"] = json!(0);

        let lines = vec![first, second];
        let report = run(app, &lines, false).await.expect("imports");
        assert_eq!(report.of("user").created, 2);

        let seen = events(app, active).await;
        let created = of_kind(&seen, "account.created");
        assert_eq!(created.len(), 1, "exactly one account.created: {seen:?}");
        assert_eq!(created[0]["path"], "import");
        assert_eq!(created[0]["active"], true);
        assert_eq!(created[0]["by"], "operator");
        let roles = of_kind(&seen, "user.roles_changed");
        assert_eq!(roles.len(), 1, "the role grant: {seen:?}");
        assert_eq!(roles[0]["change"], "added");
        assert_eq!(roles[0]["role"], role.as_str());

        let blocked_seen = events(app, blocked).await;
        let created = of_kind(&blocked_seen, "account.created");
        assert_eq!(created.len(), 1, "{blocked_seen:?}");
        assert_eq!(created[0]["active"], false, "status 0 starts blocked");

        // A rerun that changes nothing writes nothing more.
        let again = run(app, &lines, false).await.expect("rerun");
        assert_eq!(again.changes(), 0);
        assert_eq!(events(app, active).await.len(), seen.len());
        assert_eq!(events(app, blocked).await.len(), blocked_seen.len());
    });
}

/// Re-importing an account with a new name, address, status and password
/// writes one event of each kind, as the admin "Edit user" form would.
#[test]
fn a_changed_imported_account_writes_one_event_per_change() {
    run_test(async {
        let app = shared_app().await;
        let id = Uuid::now_v7();
        forget_users(app, vec![id]);
        let before = common::username("b223imp_c");
        run(app, &[user_line(id, &before)], false)
            .await
            .expect("first import");
        let first = events(app, id).await;
        assert_eq!(first.len(), 1, "only account.created: {first:?}");

        let after = common::username("b223imp_d");
        let mut changed = user_line(id, &after);
        changed["status"] = json!(0);
        changed["password_hash"] = json!(argon2_hash("imported-password-1"));
        let report = run(app, &[changed], false).await.expect("second import");
        assert_eq!(report.of("user").updated, 1);

        let seen = events(app, id).await;
        let names = of_kind(&seen, "user.name_changed");
        assert_eq!(names.len(), 1, "{seen:?}");
        assert_eq!(
            names[0]["from_hash"],
            trovato_kernel::audit::hash_subject(&before)
        );
        assert_eq!(
            names[0]["to_hash"],
            trovato_kernel::audit::hash_subject(&after)
        );
        assert!(
            !names[0].to_string().contains(&before),
            "never the name: {}",
            names[0]
        );
        let mails = of_kind(&seen, "user.mail_changed");
        assert_eq!(mails.len(), 1, "{seen:?}");
        assert!(
            !mails[0].to_string().contains("example.com"),
            "no address in the event: {}",
            mails[0]
        );
        let status = of_kind(&seen, "user.status_changed");
        assert_eq!(status.len(), 1, "{seen:?}");
        assert_eq!(status[0]["blocked"], true);
        let password = of_kind(&seen, "password.set");
        assert_eq!(password.len(), 1, "no password before, so set: {seen:?}");
        assert_eq!(password[0]["via"], "import");
        assert!(
            !password[0].to_string().contains("argon2"),
            "never the hash: {}",
            password[0]
        );
        assert!(
            seen.iter().all(|(_, actor, _)| actor.is_none()),
            "the operator is nobody's account: {seen:?}"
        );
        assert_eq!(seen.len(), first.len() + 4, "nothing else: {seen:?}");
    });
}

/// A dry run, and a run that a later bad record rolls back, change nothing,
/// so they record no change either: not to an account that already exists
/// (where a stray event would have a row to name), nor a creation.
#[test]
fn an_import_that_does_not_commit_writes_no_account_events() {
    run_test(async {
        let app = shared_app().await;
        let (existing, fresh) = (Uuid::now_v7(), Uuid::now_v7());
        forget_users(app, vec![existing, fresh]);
        run(
            app,
            &[user_line(existing, &common::username("b223imp_e"))],
            false,
        )
        .await
        .expect("first import");
        let before = events(app, existing).await;
        assert_eq!(before.len(), 1, "{before:?}");

        let renamed = || user_line(existing, &common::username("b223imp_f"));
        let report = run(
            app,
            &[renamed(), user_line(fresh, &common::username("b223imp_g"))],
            true,
        )
        .await
        .expect("checks");
        assert!(report.dry_run);
        assert_eq!(report.changes(), 2, "the dry run saw both changes");
        assert_eq!(
            events(app, existing).await.len(),
            before.len(),
            "a dry run records nothing"
        );

        let mut bad = user_line(Uuid::now_v7(), &common::username("b223imp_h"));
        bad["roles"] = json!(["b223 no such role"]);
        let result = run(
            app,
            &[
                renamed(),
                user_line(fresh, &common::username("b223imp_i")),
                bad,
            ],
            false,
        )
        .await;
        assert!(result.is_err(), "the bad record fails the run");
        let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1)")
            .bind(fresh)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert!(!exists, "rolled back");
        assert_eq!(
            events(app, existing).await.len(),
            before.len(),
            "a rolled-back rename records no user.name_changed"
        );
    });
}

/// A role grant the insert did not make (the row arrived some other way
/// between the importer's lookup and its insert, so `ON CONFLICT DO NOTHING`
/// added nothing) writes no `user.roles_changed`. A trigger that skips the
/// insert for one role stands in for the other writer, deterministically.
#[test]
fn a_grant_the_insert_did_not_make_writes_no_roles_changed_event() {
    run_test(async {
        let app = shared_app().await;
        let id = Uuid::now_v7();
        forget_users(app, vec![id]);
        let role = format!("b223imp_skip_{}", id.simple());
        let role_id = common::create_test_role(app, &role).await;

        let suffix = id.simple().to_string();
        let function = format!("b223_skip_grant_{suffix}");
        let trigger = format!("b223_skip_grant_{suffix}");
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS \
             $$ BEGIN RETURN NULL; END $$"
        )))
        .execute(&app.db)
        .await
        .unwrap();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE TRIGGER {trigger} BEFORE INSERT ON user_roles FOR EACH ROW \
             WHEN (NEW.role_id = '{role_id}') EXECUTE FUNCTION {function}()"
        )))
        .execute(&app.db)
        .await
        .unwrap();
        let pool = app.db.clone();
        let (drop_trigger, drop_function) = (
            format!("DROP TRIGGER IF EXISTS {trigger} ON user_roles"),
            format!("DROP FUNCTION IF EXISTS {function}()"),
        );
        common::defer_cleanup(move || async move {
            let _ = sqlx::query(sqlx::AssertSqlSafe(drop_trigger))
                .execute(&pool)
                .await;
            let _ = sqlx::query(sqlx::AssertSqlSafe(drop_function))
                .execute(&pool)
                .await;
        });

        let mut line = user_line(id, &common::username("b223imp_skip"));
        line["roles"] = json!([role]);
        run(app, &[line], false).await.expect("imports");

        let held: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_roles WHERE user_id = $1")
            .bind(id)
            .fetch_one(&app.db)
            .await
            .unwrap();
        assert_eq!(held, 0, "the insert added no row");
        let seen = events(app, id).await;
        assert!(
            of_kind(&seen, "user.roles_changed").is_empty(),
            "no grant was made, so none is recorded: {seen:?}"
        );
        assert_eq!(of_kind(&seen, "account.created").len(), 1, "{seen:?}");
    });
}
