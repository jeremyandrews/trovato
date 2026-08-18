#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Every config entity type either has an admin screen or is recorded as
//! deliberately import-only.
//!
//! `ROADMAP.md` sets the 1.0 bar as "a site can be built, configured and operated
//! through the interface". `KNOWN-ISSUES.md` has carried a prose list of what is
//! import-only, and prose drifts: menus were on the list of types *with* screens for
//! a while, and did not have one.
//!
//! So the list is a table here, and this file is the audit. For each of the thirteen
//! config entity types it is one of two things:
//!
//! - **`Screen`** — a path that must actually serve for an administrator. Not "a
//!   route exists": a real request, because a route that 500s is not a screen.
//! - **`ImportOnly`** — a sentence that must appear in `KNOWN-ISSUES.md`. Deciding
//!   that a type stays import-only is legitimate; deciding it silently is not.
//!
//! The table is asserted to cover `ENTITY_TYPE_ORDER` exactly, so adding a config
//! entity type without deciding which of the two it is **fails this test**. That is
//! the mechanism, and it is the only part of this file that will still be earning its
//! keep in a year.
//!
//! Requires Postgres + Redis (the shared `TestApp`).

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{TestApp, project_root, run_test, shared_app};
use trovato_kernel::config_storage::yaml::ENTITY_TYPE_ORDER;
use uuid::Uuid;

/// How a config entity type is managed.
enum Coverage {
    /// An admin path that must serve. The second element says which plugin gates it,
    /// if any: a gated route 404s until the plugin is enabled, which is correct
    /// behaviour and would otherwise look like a missing screen.
    Screen(&'static str, Option<&'static str>),
    /// Deliberately import-only. The string must appear in `KNOWN-ISSUES.md`, so the
    /// decision is written down where an operator will find it.
    ImportOnly(&'static str),
}

use Coverage::{ImportOnly, Screen};

/// The audit. One row per config entity type, no exceptions.
const COVERAGE: &[(&str, Coverage)] = &[
    ("variable", Screen("/admin/config/site", None)),
    (
        "language",
        ImportOnly("A site's language set is part of its definition"),
    ),
    ("role", Screen("/admin/people/roles", None)),
    ("item_type", Screen("/admin/structure/types", None)),
    (
        "category",
        Screen("/admin/structure/categories", Some("categories")),
    ),
    // Tags are managed per category, so the path carries one; the test seeds it.
    (
        "tag",
        Screen(
            "/admin/structure/categories/cfgaudit_topics/tags",
            Some("categories"),
        ),
    ),
    // Per content type, so the path carries one: the tutorial's `conference`.
    (
        "search_field_config",
        Screen("/admin/structure/types/conference/search", None),
    ),
    ("gather_query", Screen("/admin/gather", None)),
    ("stage", Screen("/admin/structure/stages", None)),
    ("url_alias", Screen("/admin/structure/aliases", None)),
    ("item", Screen("/admin/content", None)),
    ("tile", Screen("/admin/structure/tiles", None)),
    ("menu_link", Screen("/admin/structure/menus", None)),
];

async fn admin_cookies(app: &TestApp) -> String {
    let name = format!("cfgaudit_{}", Uuid::now_v7().simple());
    app.create_test_admin(&name, "test-password-123", &format!("{name}@example.com"))
        .await;
    app.login(&name, "test-password-123").await
}

/// The audit covers every config entity type, and nothing that is not one.
///
/// This is the test that makes the rest of the file hard to forget: a new config
/// entity type fails here until somebody decides whether it gets a screen.
#[test]
fn the_audit_covers_every_config_entity_type() {
    let audited: Vec<&str> = COVERAGE.iter().map(|(name, _)| *name).collect();

    for entity_type in ENTITY_TYPE_ORDER {
        assert!(
            audited.contains(entity_type),
            "config entity type '{entity_type}' is not in this file's audit table. \
             Decide whether it gets an admin screen or is deliberately import-only, \
             add the row, and if it is import-only say so in KNOWN-ISSUES.md."
        );
    }

    for name in &audited {
        assert!(
            ENTITY_TYPE_ORDER.contains(name),
            "'{name}' is audited here but is not a config entity type any more; \
             remove the row"
        );
    }

    assert_eq!(
        audited.len(),
        ENTITY_TYPE_ORDER.len(),
        "the audit table must have exactly one row per config entity type"
    );
}

/// Every type the audit claims has a screen has one that serves.
#[test]
fn every_claimed_screen_actually_serves() {
    run_test(async {
        let app = shared_app().await;
        let cookies = admin_cookies(app).await;

        // The tutorial content type, for the per-type search configuration screen.
        app.ensure_conference_type().await;

        // A category, for the per-category tag screen. Idempotent, and named so it
        // cannot collide with a real one.
        sqlx::query(
            "INSERT INTO category (id, label, description, hierarchy, weight) \
             VALUES ('cfgaudit_topics', 'Audit Topics', NULL, 0, 0) \
             ON CONFLICT (id) DO NOTHING",
        )
        .execute(&app.db)
        .await
        .expect("seed a category for the tag screen");

        for (entity_type, coverage) in COVERAGE {
            let Screen(path, gate) = coverage else {
                continue;
            };
            if let Some(plugin) = gate {
                app.ensure_plugin_enabled(plugin).await;
            }

            let response = app
                .request_with_cookies(Request::get(*path).body(Body::empty()).unwrap(), &cookies)
                .await;

            assert_eq!(
                response.status(),
                StatusCode::OK,
                "the {entity_type} screen at {path} must serve for an administrator, \
                 got {}. Either the screen is broken or this audit is out of date.",
                response.status()
            );
        }
    });
}

/// A screen is a screen for administrators only.
///
/// Not a completeness claim about permissions — that is each screen's own business —
/// but a floor: nothing in the audit is reachable by an anonymous visitor.
#[test]
fn no_claimed_screen_is_open_to_an_anonymous_visitor() {
    run_test(async {
        let app = shared_app().await;

        for (entity_type, coverage) in COVERAGE {
            let Screen(path, _) = coverage else {
                continue;
            };
            let response = app
                .request(Request::get(*path).body(Body::empty()).unwrap())
                .await;
            assert_ne!(
                response.status(),
                StatusCode::OK,
                "the {entity_type} screen at {path} served an anonymous visitor"
            );
        }
    });
}

/// Every import-only type says so in `KNOWN-ISSUES.md`, in words.
#[test]
fn every_import_only_type_is_documented_as_a_decision() {
    let known_issues =
        std::fs::read_to_string(project_root().join("KNOWN-ISSUES.md")).expect("KNOWN-ISSUES.md");

    for (entity_type, coverage) in COVERAGE {
        let ImportOnly(sentence) = coverage else {
            continue;
        };
        assert!(
            known_issues.contains(sentence),
            "'{entity_type}' is import-only by decision, so KNOWN-ISSUES.md must say \
             so. Expected to find: {sentence:?}"
        );
    }
}

/// And the documentation does not claim a screen exists where the audit says none
/// does, which is the specific way this drifted before.
#[test]
fn the_documentation_does_not_claim_a_screen_that_the_audit_denies() {
    let known_issues =
        std::fs::read_to_string(project_root().join("KNOWN-ISSUES.md")).expect("KNOWN-ISSUES.md");

    let claim_line = known_issues
        .lines()
        .find(|line| line.contains("do have admin screens"));

    if let Some(line) = claim_line {
        for (entity_type, coverage) in COVERAGE {
            if matches!(coverage, ImportOnly(_)) {
                // The plural the prose uses.
                let plural = format!("{entity_type}s");
                assert!(
                    !line.contains(&plural),
                    "KNOWN-ISSUES.md lists '{plural}' among the types with admin \
                     screens, and this audit says it is import-only: {line}"
                );
            }
        }
    }
}
