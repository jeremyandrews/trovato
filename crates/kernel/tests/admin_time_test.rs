#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Admin dates in the viewer's own timezone and language (B246).
//!
//! - The Dashboard's panels and the Reports landing page's Cron line write
//!   their times when they are shown, in the viewer's timezone and admin
//!   language, never as UTC text.
//! - The revision pages, whose action names and title hold a date, hold its
//!   text: never a `<time>` element, escaped or not, and never a bare
//!   timestamp.
//!
//! The pseudo locales must be allowed before the shared app exists, so this
//! file is its own test binary.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, run_test, shared_app, test_ip_for};
use trovato_kernel::i18n;
use trovato_kernel::models::CreateItem;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::routes::admin_nav::Preferences;
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

fn pseudo_on() {
    trovato_test_utils::env::set_env_default("TROVATO_PSEUDO_LOCALES", "1");
    assert!(i18n::pseudo_locales_enabled());
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> (StatusCode, String) {
    let response = app
        .request_with_cookies(
            Request::get(uri)
                .header("x-forwarded-for", test_ip_for("b246time"))
                .body(Body::empty())
                .unwrap(),
            cookies,
        )
        .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// An administrator in `timezone` (and `language`, when given); returns the
/// session and the account's id. The account goes when the test ends.
async fn admin_in(
    app: &TestApp,
    prefix: &str,
    timezone: &str,
    language: Option<&str>,
) -> (String, Uuid) {
    let name = common::username(prefix);
    let password = "correct-horse-battery-staple";
    app.create_test_admin(&name, password, &format!("{name}@test.local"))
        .await;
    let id: Uuid =
        sqlx::query_scalar("UPDATE users SET timezone = $2 WHERE name = $1 RETURNING id")
            .bind(&name)
            .bind(timezone)
            .fetch_one(&app.db)
            .await
            .unwrap();
    app.state.users().invalidate(id);
    let pool = app.db.clone();
    common::defer_cleanup(move || async move {
        let _ = sqlx::query("DELETE FROM item WHERE author_id = $1")
            .bind(id)
            .execute(&pool)
            .await;
        let _ = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await;
    });
    if let Some(language) = language {
        Preferences::merge(&app.db, id, serde_json::json!({ "language": language }))
            .await
            .unwrap();
    }
    (app.login(&name, password).await, id)
}

/// The text the page writes for `ts`: escaped as a template prints it.
fn shown(lang: &str, tz: chrono_tz::Tz, ts: i64) -> String {
    tera::escape_html(&i18n::admin_time_text_at(
        None,
        lang,
        tz,
        ts,
        false,
        chrono::Utc::now().timestamp(),
        true,
    ))
}

/// Dashboard and Reports landing page for a viewer in Tokyo reading the
/// accented pseudo locale: the dates are Tokyo's, the month and the "Today"
/// are the pseudo locale's, and no panel or status line says UTC.
#[test]
fn the_dashboard_writes_times_in_the_viewers_timezone() {
    pseudo_on();
    run_test(async {
        let app = shared_app().await;
        let tokyo = chrono_tz::Asia::Tokyo;
        let (cookies, id) =
            admin_in(app, "b246tokyo", "Asia/Tokyo", Some(i18n::PSEUDO_ACCENTED)).await;

        // An item of the viewer's, changed ten days ago at 03:30 UTC (12:30
        // in Tokyo).
        let now = chrono::Utc::now().timestamp();
        let changed = (now - 10 * 86_400) / 86_400 * 86_400 + 3 * 3600 + 30 * 60;
        let viewer = UserContext::administrator(id, vec!["administer site".to_string()]);
        let title = format!("Tokyo item {}", Uuid::new_v4().simple());
        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: "page".to_string(),
                    title: title.clone(),
                    author_id: id,
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({})),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: Some("timezone test".to_string()),
                },
                &viewer,
            )
            .await
            .unwrap();
        sqlx::query("UPDATE item SET changed = $2 WHERE id = $1")
            .bind(item.id)
            .bind(changed)
            .execute(&app.db)
            .await
            .unwrap();
        app.state.items().invalidate(item.id);

        // One error, now.
        let message = format!("b246 tokyo error {}", Uuid::new_v4().simple());
        let logged: i64 = sqlx::query_scalar(
            "INSERT INTO system_log (created, level, log_type, message, context) \
             VALUES ($1, 'error', 'b246', $2, '{}'::jsonb) RETURNING created",
        )
        .bind(now)
        .bind(&message)
        .fetch_one(&app.db)
        .await
        .unwrap();
        let pool = app.db.clone();
        let cleanup_message = message.clone();
        common::defer_cleanup(move || async move {
            let _ = sqlx::query("DELETE FROM system_log WHERE message = $1")
                .bind(cleanup_message)
                .execute(&pool)
                .await;
        });

        let (status, html) = get(app, &cookies, "/admin").await;
        assert_eq!(status, StatusCode::OK);
        let item_time = shown(i18n::PSEUDO_ACCENTED, tokyo, changed);
        assert!(item_time.contains("12:30"), "{item_time}");
        let month = i18n::translate(
            None,
            i18n::PSEUDO_ACCENTED,
            &i18n::Message {
                context: "short month",
                ..i18n::Message::new(
                    [
                        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct",
                        "Nov", "Dec",
                    ][usize::try_from(
                        chrono::TimeZone::timestamp_opt(&tokyo, changed, 0)
                            .unwrap()
                            .format("%m")
                            .to_string()
                            .parse::<u32>()
                            .unwrap()
                            - 1,
                    )
                    .unwrap()],
                )
            },
            &[],
        );
        assert!(
            item_time.contains(&tera::escape_html(&month)),
            "{item_time} names {month}"
        );
        assert!(
            html.contains(&item_time),
            "Your recent content shows the item's Tokyo time {item_time}: {html}"
        );
        let error_time = shown(i18n::PSEUDO_ACCENTED, tokyo, logged);
        let today = i18n::translate(
            None,
            i18n::PSEUDO_ACCENTED,
            &i18n::Message::new("Today, {time}"),
            &[],
        );
        let yesterday = i18n::translate(
            None,
            i18n::PSEUDO_ACCENTED,
            &i18n::Message::new("Yesterday, {time}"),
            &[],
        );
        let head = |s: &str| tera::escape_html(s.split("{time}").next().unwrap());
        assert!(
            error_time.contains(&head(&today)) || error_time.contains(&head(&yesterday)),
            "{error_time}"
        );
        assert!(
            html.contains(&error_time),
            "Recent errors shows {error_time}: {html}"
        );
        assert!(!html.contains("UTC"), "no panel says UTC: {html}");

        // The Reports landing page's Cron line: a run is needed for a date.
        // A run recorded by anyone else is kept, whatever its date: the line
        // is compared with that run's own text. Without one, this test writes
        // its own, only if the key is still empty (`NX`), under a hostname no
        // one else uses, and removes it at the end only if it is still that
        // record.
        let mut redis = app
            .state
            .redis()
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        let ours = serde_json::json!({
            "timestamp": now, "hostname": format!("b246-{}", Uuid::new_v4()),
            "result": "Completed", "status": "completed", "tasks": [], "duration_ms": 1,
        })
        .to_string();
        let _: Option<String> = redis::cmd("SET")
            .arg("cron:last_run")
            .arg(&ours)
            .arg("NX")
            .arg("EX")
            .arg(600)
            .query_async(&mut redis)
            .await
            .unwrap();
        let mut found = false;
        for _ in 0..3 {
            let before = app
                .state
                .cron()
                .last_run()
                .await
                .unwrap()
                .map(|r| r.timestamp);
            let (status, html) = get(app, &cookies, "/admin/reports/overview").await;
            assert_eq!(status, StatusCode::OK);
            let after = app
                .state
                .cron()
                .last_run()
                .await
                .unwrap()
                .map(|r| r.timestamp);
            if before != after {
                continue;
            }
            let ts = after.expect("a cron run is recorded");
            let line = tera::escape_html(&i18n::translate(
                None,
                i18n::PSEUDO_ACCENTED,
                &i18n::Message::new("Last run {when}"),
                &[(
                    "when".to_string(),
                    i18n::admin_time_text_at(
                        None,
                        i18n::PSEUDO_ACCENTED,
                        tokyo,
                        ts,
                        false,
                        chrono::Utc::now().timestamp(),
                        true,
                    ),
                )],
            ));
            assert!(html.contains(&line), "the Cron line is {line}: {html}");
            assert!(!html.contains("UTC"), "no status line says UTC: {html}");
            found = true;
            break;
        }
        // Compare and delete in one step: a record another process wrote
        // since is left alone.
        let _: i64 = redis::Script::new(
            "if redis.call('GET', KEYS[1]) == ARGV[1] then \
             return redis.call('DEL', KEYS[1]) else return 0 end",
        )
        .key("cron:last_run")
        .arg(&ours)
        .invoke_async(&mut redis)
        .await
        .unwrap();
        assert!(found, "the cron record held still for one request");
    });
}

/// The revision pages name their actions and title by the revision's date:
/// its text, with seconds, in the viewer's timezone. Never markup, escaped
/// or not, and never a bare timestamp.
#[test]
fn revision_names_hold_the_dates_text() {
    pseudo_on();
    run_test(async {
        let app = shared_app().await;
        let (cookies, id) = admin_in(app, "b246revs", "Asia/Tokyo", None).await;
        let viewer = UserContext::administrator(id, vec!["administer site".to_string()]);
        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: "page".to_string(),
                    // Letters only: the test below looks for a run of ten
                    // digits in every name, and a hex id can hold one.
                    title: format!("Revised {}", letters_only(Uuid::new_v4())),
                    author_id: id,
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({})),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: Some("first".to_string()),
                },
                &viewer,
            )
            .await
            .unwrap();
        app.state
            .items()
            .update(
                item.id,
                trovato_kernel::models::UpdateItem {
                    title: Some(format!("{} again", item.title)),
                    status: None,
                    promote: None,
                    sticky: None,
                    fields: None,
                    log: Some("second".to_string()),
                    stage_id: None,
                    author_id: None,
                    created: None,
                },
                &viewer,
            )
            .await
            .unwrap();
        let revisions: Vec<(Uuid, i64)> = sqlx::query_as(
            "SELECT id, created FROM item_revision WHERE item_id = $1 ORDER BY created",
        )
        .bind(item.id)
        .fetch_all(&app.db)
        .await
        .unwrap();
        assert!(revisions.len() >= 2, "two revisions");
        // Both may be saved in one second: the old one is the one that is
        // not current.
        let current: Uuid =
            sqlx::query_scalar("SELECT current_revision_id FROM item WHERE id = $1")
                .bind(item.id)
                .fetch_one(&app.db)
                .await
                .unwrap();
        let (old, old_created) = *revisions.iter().find(|(r, _)| *r != current).unwrap();
        let new = current;

        let pages = [
            format!("/item/{}/revisions", item.id),
            format!("/item/{}/revisions/{old}", item.id),
            format!("/item/{}/revisions/compare?from={old}&to={new}", item.id),
            format!("/item/{}/revisions/{old}/delete", item.id),
        ];
        let text = tera::escape_html(&i18n::admin_time_text_at(
            None,
            "en",
            chrono_tz::Asia::Tokyo,
            old_created,
            true,
            chrono::Utc::now().timestamp(),
            true,
        ));
        for page in &pages {
            let (status, html) = get(app, &cookies, page).await;
            assert_eq!(status, StatusCode::OK, "{page}");
            assert!(html.contains(&text), "{page} names the revision {text}");
            let mut names: Vec<String> = Vec::new();
            for attr in ["aria-label=\"", "title=\""] {
                for (at, _) in html.match_indices(attr) {
                    let rest = &html[at + attr.len()..];
                    names.push(rest[..rest.find('"').unwrap_or(0)].to_string());
                }
            }
            let title_start = html.find("<title>").unwrap() + "<title>".len();
            names.push(
                html[title_start..title_start + html[title_start..].find("</title>").unwrap()]
                    .to_string(),
            );
            // Each action's own text (a link's or a button's).
            for tag in ["<a ", "<button "] {
                for (at, _) in html.match_indices(tag) {
                    let rest = &html[at..];
                    let open = rest.find('>').unwrap() + 1;
                    let close = rest
                        .find(if tag == "<a " { "</a>" } else { "</button>" })
                        .unwrap_or(open);
                    if close > open {
                        names.push(rest[open..close].to_string());
                    }
                }
            }
            for name in &names {
                assert!(
                    !name.contains("<time") && !name.contains("&lt;time"),
                    "{page}: a name holds markup: {name}"
                );
                let digits = name
                    .split(|c: char| !c.is_ascii_digit())
                    .map(str::len)
                    .max()
                    .unwrap_or(0);
                assert!(digits < 10, "{page}: a name holds a timestamp: {name}");
            }
        }
    });
}

/// Two revisions saved in the same second: their dates, to the second, are
/// the same, so the list adds each one's place in that second, and no two
/// actions or radios share a name.
#[test]
fn revisions_saved_in_one_second_keep_distinct_names() {
    run_test(async {
        let app = shared_app().await;
        let (cookies, id) = admin_in(app, "b246same", "Europe/Rome", None).await;
        let viewer = UserContext::administrator(id, vec!["administer site".to_string()]);
        let item = app
            .state
            .items()
            .create(
                CreateItem {
                    item_type: "page".to_string(),
                    title: format!("Same second {}", letters_only(Uuid::new_v4())),
                    author_id: id,
                    status: Some(1),
                    promote: Some(0),
                    sticky: Some(0),
                    fields: Some(serde_json::json!({})),
                    stage_id: Some(LIVE_STAGE_ID),
                    language: Some("en".to_string()),
                    log: Some("first".to_string()),
                },
                &viewer,
            )
            .await
            .unwrap();
        // Two older revisions written in one second.
        let second = chrono::Utc::now().timestamp() - 3 * 86_400;
        for log in ["older a", "older b"] {
            sqlx::query(
                "INSERT INTO item_revision (id, item_id, author_id, title, status, fields, created, log) \
                 VALUES ($1, $2, $3, $4, 1, '{}'::jsonb, $5, $6)",
            )
            .bind(Uuid::now_v7())
            .bind(item.id)
            .bind(id)
            .bind(&item.title)
            .bind(second)
            .bind(log)
            .execute(&app.db)
            .await
            .unwrap();
        }

        let (status, html) = get(app, &cookies, &format!("/item/{}/revisions", item.id)).await;
        assert_eq!(status, StatusCode::OK);
        let date = tera::escape_html(&i18n::admin_time_text_at(
            None,
            "en",
            chrono_tz::Europe::Rome,
            second,
            true,
            chrono::Utc::now().timestamp(),
            true,
        ));
        for n in ["1 of 2", "2 of 2"] {
            for name in [
                format!("View the revision of {date}, {n}"),
                format!("Compare from the revision of {date}, {n}"),
                format!("Compare to the revision of {date}, {n}"),
            ] {
                assert_eq!(html.matches(&name).count(), 1, "{name}: {html}");
            }
        }
        // The current revision, alone in its second, is named by its date.
        assert!(!html.contains(", 1 of 1"), "{html}");
        let mut names: Vec<&str> = html
            .match_indices("aria-label=\"")
            .map(|(at, attr)| {
                let rest = &html[at + attr.len()..];
                &rest[..rest.find('"').unwrap()]
            })
            .filter(|name| name.contains("revision of"))
            .collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            total,
            "every radio has its own name: {names:?}"
        );
        assert_eq!(total, 6, "{names:?}");
    });
}

/// `id` as 32 letters, each hex digit 0-9 written as g-p, so a title made from
/// it holds no run of digits.
fn letters_only(id: Uuid) -> String {
    id.simple()
        .to_string()
        .chars()
        .map(|c| match c.to_digit(10) {
            Some(d) => char::from(b'g' + d as u8),
            None => c,
        })
        .collect()
}
