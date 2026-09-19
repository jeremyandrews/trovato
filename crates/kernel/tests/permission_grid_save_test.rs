#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Saving the permission grid does not delete what the grid did not display.
//!
//! Observed on a 0.102.0 site with the external `netgrasp` plugin enabled:
//! saving `/admin/people/permissions` deleted 29 grants across every role, all
//! of them plugin permissions including `administer netgrasp`. They had been
//! inserted by the plugin's migrations, the grid rendered the kernel's list and
//! nothing else, and the save had replace semantics — so every permission the
//! form never showed arrived looking exactly like a box someone had deliberately
//! unticked, and was taken away. They were restored with SQL.
//!
//! Dispatching `tap_perm` shrinks the problem, because a plugin's permissions
//! now appear in the grid. It does not remove it: a disabled plugin's grants, a
//! permission added by SQL, and one left by a migration are all still invisible
//! to this screen, and the save must be safe for them anyway.
//!
//! The rule this file pins: **a save replaces only the permissions the form
//! actually rendered.** A permission the grid does not know about is left
//! exactly as it was, granted or not.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for};
use trovato_kernel::models::Role;
use uuid::Uuid;

/// Serializes the saves in this file.
///
/// The permission grid is one form covering every role, so each test here reads
/// the whole grid and writes the whole grid back. Two of them interleaving means
/// one echoes a snapshot the other has already moved past, and the later write
/// wins for a role it was not testing. The lock makes each read-modify-write
/// atomic with respect to the others; nothing else in the suite posts this form.
static GRID_SAVE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn get(app: &TestApp, cookies: &str, path: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(path)
                .header("x-forwarded-for", test_ip_for("gridsave"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read body");
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// Read the grid and rebuild exactly the POST body the browser would send.
///
/// The form states which permissions it rendered as `permname_{i}` and checks
/// them as `perm_{i}_{role_id}`, so reconstructing it from the page is the only
/// honest way to test the save: a hand-written body would be a body the screen
/// never produces.
///
/// **Every other role's boxes are echoed back exactly as rendered.** A browser
/// submits the whole grid, so a body that carried only the role under test would
/// arrive as "every other role: nothing ticked" and revoke their permissions —
/// including the seeded `anonymous` and `authenticated` roles, which would break
/// unrelated tests sharing this database. Echoing is what a real save does.
///
/// `check` decides, for the role under test only, whether each rendered
/// permission's box is ticked.
async fn save_grid(
    app: &TestApp,
    cookies: &str,
    role_id: Uuid,
    check: impl Fn(&str) -> bool,
) -> StatusCode {
    let _guard = GRID_SAVE_LOCK.lock().await;

    let (status, html) = get(app, cookies, "/admin/people/permissions").await;
    assert_eq!(status, StatusCode::OK, "the grid must render");

    let token = {
        let marker = r#"name="_token" value=""#;
        let start = html.find(marker).expect("no CSRF token on the grid") + marker.len();
        let end = start + html[start..].find('"').unwrap();
        html[start..end].to_string()
    };

    // Which `perm_{i}_{role}` boxes the page rendered as already checked.
    let mut already_checked: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (at, _) in html.match_indices(r#"name="perm_"#) {
        let rest = &html[at..];
        let tag_end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..tag_end];
        let name_start = r#"name=""#.len();
        let name_end = tag[name_start..].find('"').unwrap() + name_start;
        if tag.contains("checked") {
            already_checked.insert(tag[name_start..name_end].to_string());
        }
    }

    let mut body = format!("_token={token}&_form_build_id=x");
    let mut index = 0usize;
    loop {
        let marker = format!(r#"name="permname_{index}" value=""#);
        let Some(at) = html.find(&marker) else {
            break;
        };
        let start = at + marker.len();
        let end = start + html[start..].find('"').unwrap();
        let name = html_unescape(&html[start..end]);

        body.push_str(&format!("&permname_{index}={}", urlencode(&name)));

        // The role under test: whatever `check` says. Every other role: exactly
        // what the page showed.
        for key in &already_checked {
            let prefix = format!("perm_{index}_");
            if let Some(other) = key.strip_prefix(&prefix)
                && other != role_id.to_string()
            {
                body.push_str(&format!("&{key}=1"));
            }
        }
        if check(&name) {
            body.push_str(&format!("&perm_{index}_{role_id}=1"));
        }
        index += 1;
    }
    assert!(index > 0, "the grid rendered no permissions at all");

    app.request_with_cookies(
        Request::post("/admin/people/permissions")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("x-forwarded-for", test_ip_for("gridsave"))
            .body(Body::from(body))
            .unwrap(),
        cookies,
    )
    .await
    .status()
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Tera autoescapes `.html` templates, so a rendered value comes back escaped.
fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
}

async fn grants(app: &TestApp, role_id: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT permission FROM role_permissions WHERE role_id = $1 ORDER BY permission",
    )
    .bind(role_id)
    .fetch_all(&app.db)
    .await
    .expect("read grants")
}

/// An administrator and a role nobody else is using.
async fn fixture(app: &TestApp) -> (String, Uuid, String) {
    let tag = Uuid::now_v7().simple().to_string();
    let role = Role::create(&app.db, &format!("gridsave_{tag}"))
        .await
        .expect("create role");
    let name = format!("gridsave_{tag}");
    let cookies = app
        .create_and_login_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    (cookies, role.id, tag)
}

/// The 0.102.0 data loss, as a test.
#[test]
fn a_grant_the_grid_never_rendered_survives_a_save() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, role_id, tag) = fixture(app).await;

        // A permission no plugin declares and the kernel does not define, so
        // the grid cannot render it: exactly the shape of `administer netgrasp`
        // left behind by a migration.
        let invisible = format!("administer nothing {tag}");
        Role::add_permission(&app.db, role_id, &invisible)
            .await
            .expect("seed the invisible grant");

        // Save the grid with everything unticked for this role: the most
        // destructive input the screen can produce.
        let status = save_grid(app, &cookies, role_id, |_| false).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "the save must succeed");

        assert!(
            grants(app, role_id).await.contains(&invisible),
            "a permission the grid never rendered must survive a save, \
             got {:?}",
            grants(app, role_id).await
        );
    });
}

/// The other half: a permission the grid *did* render, unticked, is removed.
///
/// Without this the safe rule above could be satisfied by a save that removes
/// nothing at all, which would be a different bug.
#[test]
fn a_rendered_permission_left_unchecked_is_removed() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, role_id, _tag) = fixture(app).await;

        Role::add_permission(&app.db, role_id, "access content")
            .await
            .expect("seed a visible grant");
        assert!(
            grants(app, role_id)
                .await
                .contains(&"access content".to_string())
        );

        let status = save_grid(app, &cookies, role_id, |_| false).await;
        assert_eq!(status, StatusCode::SEE_OTHER);

        assert!(
            !grants(app, role_id)
                .await
                .contains(&"access content".to_string()),
            "a rendered permission left unchecked must be revoked"
        );
    });
}

/// And a rendered permission that is checked is kept, or granted.
#[test]
fn a_rendered_permission_left_checked_is_kept() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, role_id, _tag) = fixture(app).await;

        let status = save_grid(app, &cookies, role_id, |name| name == "access content").await;
        assert_eq!(status, StatusCode::SEE_OTHER);

        let held = grants(app, role_id).await;
        assert!(
            held.contains(&"access content".to_string()),
            "a checked permission must be granted, got {held:?}"
        );
        assert_eq!(held.len(), 1, "and only the checked one, got {held:?}");
    });
}

/// All three rules at once, which is the situation that lost the data.
///
/// An administrator ticks one box on a role that also holds an invisible plugin
/// grant. The visible change applies; the invisible grant is untouched.
#[test]
fn ticking_one_box_does_not_disturb_an_invisible_grant() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, role_id, tag) = fixture(app).await;

        let invisible = format!("administer elsewhere {tag}");
        Role::add_permission(&app.db, role_id, &invisible)
            .await
            .expect("seed");
        Role::add_permission(&app.db, role_id, "access content")
            .await
            .expect("seed");

        // Swap which visible permission is held.
        let status = save_grid(app, &cookies, role_id, |name| name == "create content").await;
        assert_eq!(status, StatusCode::SEE_OTHER);

        let held = grants(app, role_id).await;
        assert!(held.contains(&invisible), "invisible grant kept: {held:?}");
        assert!(
            held.contains(&"create content".to_string()),
            "newly checked granted: {held:?}"
        );
        assert!(
            !held.contains(&"access content".to_string()),
            "newly unchecked revoked: {held:?}"
        );
    });
}
