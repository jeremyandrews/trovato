#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Site search covers every published item by its title, every text field, and
//! every published comment, and stays current when content goes away.
//!
//! Before this, the Pagefind export read only the fields configured in
//! `search_field_config` (falling back to the body), never read comments,
//! indexed the stored body HTML as text, and was rebuilt only when
//! `MAX(changed)` over published items rose, which a delete, an unpublish or a
//! comment never does. The Postgres full text vector likewise held the title
//! and the configured fields only.
//!
//! The fixture: a type with a body and two extra text fields (plus a file and a
//! number field, which are not text), a type with only a title, published and
//! unpublished comments, an unpublished item, and a published item on an
//! internal stage, which the anonymous visitor may not see.
//!
//! Requires Postgres + Redis (the shared `TestApp`); runs in CI.

mod common;

use common::{run_test, shared_app};
use trovato_kernel::models::comment::CommentStatus;
use trovato_kernel::models::stage::LIVE_STAGE_ID;
use trovato_kernel::models::{CreateComment, CreateItem, UpdateComment, UpdateItem};
use trovato_kernel::tap::UserContext;
use uuid::Uuid;

/// A type with a body, two extra text fields, and two fields that are not text.
const STORY_TYPE: &str = "b106_story";
/// A type with a title and nothing else.
const NOTE_TYPE: &str = "b106_note";

fn admin() -> UserContext {
    UserContext::administrator(Uuid::nil(), vec!["administer site".to_string()])
}

async fn ensure_fixture_types(app: &common::TestApp) {
    use trovato_sdk::types::{FieldDefinition, FieldType};

    let story_fields = vec![
        FieldDefinition::new("field_body", FieldType::TextLong).label("Body"),
        FieldDefinition::new("field_standfirst", FieldType::Text { max_length: None })
            .label("Standfirst"),
        FieldDefinition::new("field_aside", FieldType::TextLong).label("Aside"),
        FieldDefinition::new("field_attachment", FieldType::File).label("Attachment"),
        FieldDefinition::new("field_rank", FieldType::Integer).label("Rank"),
    ];
    for (machine, label, fields) in [
        (STORY_TYPE, "B106 story", story_fields),
        (NOTE_TYPE, "B106 note", Vec::new()),
    ] {
        let settings = serde_json::json!({ "fields": serde_json::to_value(&fields).unwrap() });
        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ($1, $2, 'search coverage fixture', true, 'Title', 'core', $3) \
             ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings",
        )
        .bind(machine)
        .bind(label)
        .bind(&settings)
        .execute(&app.db)
        .await
        .expect("seed a fixture type");
    }
}

async fn create_item(
    app: &common::TestApp,
    item_type: &str,
    title: &str,
    fields: serde_json::Value,
    status: i16,
    stage: Uuid,
) -> Uuid {
    app.state
        .items()
        .create(
            CreateItem {
                item_type: item_type.to_string(),
                title: title.to_string(),
                author_id: Uuid::nil(),
                status: Some(status),
                promote: Some(0),
                sticky: Some(0),
                fields: Some(fields),
                stage_id: Some(stage),
                language: Some("en".to_string()),
                log: Some("search coverage fixture".to_string()),
            },
            &admin(),
        )
        .await
        .expect("create a fixture item")
        .id
}

async fn create_comment(app: &common::TestApp, item_id: Uuid, body: &str, status: i16) -> Uuid {
    app.state
        .comments()
        .create(
            CreateComment {
                item_id,
                parent_id: None,
                author_id: Uuid::nil(),
                subject: None,
                body: body.to_string(),
                body_format: Some("filtered_html".to_string()),
                status: Some(status),
            },
            &admin(),
        )
        .await
        .expect("create a fixture comment")
        .id
}

/// Every word the fixture plants, each in exactly one place.
struct Words {
    title_only: String,
    body: String,
    aside: String,
    attachment: String,
    published_comment: String,
    reply: String,
    unpublished_comment: String,
    unpublished_item: String,
    hidden_item: String,
}

impl Words {
    fn new() -> Self {
        let tag = Uuid::now_v7().simple().to_string();
        let w = |what: &str| format!("zq{what}{tag}");
        Self {
            title_only: w("title"),
            body: w("body"),
            aside: w("aside"),
            attachment: w("attach"),
            published_comment: w("comment"),
            reply: w("reply"),
            unpublished_comment: w("hiddencomment"),
            unpublished_item: w("draft"),
            hidden_item: w("internal"),
        }
    }
}

struct Fixture {
    words: Words,
    story: Uuid,
    note: Uuid,
    draft: Uuid,
    hidden: Uuid,
}

async fn build_fixture(app: &common::TestApp) -> Fixture {
    app.ensure_plugin_enabled("trovato_comments").await;
    ensure_fixture_types(app).await;
    let words = Words::new();

    let story = create_item(
        app,
        STORY_TYPE,
        "A story with every kind of field",
        serde_json::json!({
            "field_body": {
                "value": format!("<p>The <strong>{}</strong> sits in markup &amp; entities.</p>", words.body),
                "format": "filtered_html",
            },
            "field_standfirst": "A plain standfirst",
            "field_aside": { "value": format!("<p>Only the aside says {}.</p>", words.aside) },
            "field_attachment": { "uri": format!("local://{}.pdf", words.attachment) },
            "field_rank": 7,
        }),
        1,
        LIVE_STAGE_ID,
    )
    .await;
    let note = create_item(
        app,
        NOTE_TYPE,
        &format!("Note titled {}", words.title_only),
        serde_json::json!({}),
        1,
        LIVE_STAGE_ID,
    )
    .await;
    let draft = create_item(
        app,
        STORY_TYPE,
        &format!("Draft {}", words.unpublished_item),
        serde_json::json!({ "field_body": { "value": words.unpublished_item.clone() } }),
        0,
        LIVE_STAGE_ID,
    )
    .await;
    let internal = common::create_test_stage(
        app,
        &format!(
            "b106_internal_{}",
            &Uuid::now_v7().simple().to_string()[24..]
        ),
    )
    .await;
    let hidden = create_item(
        app,
        STORY_TYPE,
        &format!("Internal {}", words.hidden_item),
        serde_json::json!({ "field_body": { "value": words.hidden_item.clone() } }),
        1,
        internal,
    )
    .await;

    let top = create_comment(
        app,
        story,
        &format!(
            "<p>I agree &mdash; <em>{}</em> &amp; more.</p>",
            words.published_comment
        ),
        CommentStatus::Published.as_i16(),
    )
    .await;
    // A published reply under a published comment is public too.
    app.state
        .comments()
        .create(
            CreateComment {
                item_id: story,
                parent_id: Some(top),
                author_id: Uuid::nil(),
                subject: None,
                body: format!("Replying with {}", words.reply),
                body_format: None,
                status: Some(CommentStatus::Published.as_i16()),
            },
            &admin(),
        )
        .await
        .expect("create a reply");
    create_comment(
        app,
        story,
        &format!("Held back {}", words.unpublished_comment),
        CommentStatus::Unpublished.as_i16(),
    )
    .await;
    create_comment(
        app,
        hidden,
        &format!("On an internal item {}", words.hidden_item),
        CommentStatus::Published.as_i16(),
    )
    .await;
    create_comment(
        app,
        draft,
        &format!("On a draft {}", words.unpublished_item),
        CommentStatus::Published.as_i16(),
    )
    .await;

    Fixture {
        words,
        story,
        note,
        draft,
        hidden,
    }
}

/// The text inside a fragment's `data-pagefind-body` region.
fn indexed_region(html: &str) -> &str {
    let body_at = html
        .find("data-pagefind-body")
        .unwrap_or_else(|| panic!("the fragment must mark its indexed region: {html}"));
    let start = html[body_at..].find('>').unwrap() + body_at + 1;
    let end = html[start..]
        .find("</div>")
        .map(|i| i + start)
        .expect("the indexed region must be closed");
    &html[start..end]
}

/// Every searchable value a document carries: its title, description and
/// field texts, and the text nodes of its indexed region (which is where the
/// comments are).
fn indexed_text(doc: &trovato_kernel::cron::IndexDocument) -> Vec<String> {
    let mut all = vec![doc.title.clone(), doc.description.clone()];
    all.extend(doc.fields.iter().map(|f| f.text.clone()));
    let html = trovato_kernel::cron::render_fragment(doc);
    for chunk in indexed_region(&html).split('<') {
        if let Some((_, text)) = chunk.split_once('>') {
            all.push(text.to_string());
        }
    }
    all
}

#[test]
fn the_pagefind_export_covers_titles_every_text_field_and_published_comments() {
    run_test(async {
        let app = shared_app().await;
        let f = build_fixture(app).await;
        let w = &f.words;

        let docs = trovato_kernel::cron::indexable_documents(&app.db, app.state.items())
            .await
            .expect("build the export list");
        let find = |word: &str| -> Vec<Uuid> {
            docs.iter()
                .filter(|d| {
                    indexed_region(&trovato_kernel::cron::render_fragment(d)).contains(word)
                })
                .map(|d| d.id)
                .collect()
        };

        assert_eq!(
            find(&w.title_only),
            vec![f.note],
            "a word only in a title finds its item"
        );
        assert_eq!(find(&w.body), vec![f.story], "the body is indexed");
        assert_eq!(
            find(&w.aside),
            vec![f.story],
            "a word only in an unconfigured second text field finds its item"
        );
        assert_eq!(
            find(&w.published_comment),
            vec![f.story],
            "a word only in a published comment finds the item it is on"
        );
        assert_eq!(
            find(&w.reply),
            vec![f.story],
            "a published reply is indexed too"
        );

        assert!(
            find(&w.unpublished_comment).is_empty(),
            "an unpublished comment is not indexed"
        );
        assert!(
            find(&w.unpublished_item).is_empty(),
            "an unpublished item is not indexed, nor its comments"
        );
        assert!(
            find(&w.hidden_item).is_empty(),
            "an item anonymous may not view is not indexed, nor its comments"
        );
        assert!(find(&w.attachment).is_empty(), "a file field is not text");
        assert!(docs.iter().all(|d| d.id != f.draft && d.id != f.hidden));

        // Plain text, never markup: neither a tag nor an escaped tag.
        let story = docs.iter().find(|d| d.id == f.story).unwrap();
        for text in indexed_text(story) {
            assert!(!text.contains('<'), "indexed text carries markup: {text:?}");
            assert!(
                !text.contains("&lt;"),
                "indexed text carries escaped markup: {text:?}"
            );
        }
        let fragment = trovato_kernel::cron::render_fragment(story);
        let region = indexed_region(&fragment);
        assert!(
            !region.contains("&lt;p"),
            "the body's markup is not indexed as text: {region}"
        );
        assert!(
            !region.contains("&amp;amp;"),
            "entities are not double encoded: {region}"
        );

        // The result still links to the item, never to a comment.
        assert!(
            fragment.contains(&format!("data-pagefind-meta=\"url:/item/{}\"", f.story)),
            "the result URL is the item's: {fragment}"
        );
    });
}

#[test]
fn the_postgres_search_covers_every_text_field_and_published_comments() {
    run_test(async {
        let app = shared_app().await;
        let f = build_fixture(app).await;
        let w = &f.words;

        let search = |word: String| async move {
            let raw = app
                .state
                .search()
                .search_unrecorded(
                    &word,
                    &trovato_kernel::search::SearchVisibility::public([LIVE_STAGE_ID]),
                    50,
                    0,
                )
                .await
                .expect("search");
            raw.results.into_iter().map(|r| r.id).collect::<Vec<Uuid>>()
        };

        assert_eq!(search(w.title_only.clone()).await, vec![f.note]);
        assert_eq!(
            search(w.aside.clone()).await,
            vec![f.story],
            "every text field"
        );
        assert_eq!(
            search(w.published_comment.clone()).await,
            vec![f.story],
            "a published comment"
        );
        assert_eq!(
            search(w.reply.clone()).await,
            vec![f.story],
            "a published reply"
        );
        assert!(search(w.unpublished_comment.clone()).await.is_empty());
        assert!(search(w.unpublished_item.clone()).await.is_empty());
        assert!(search(w.hidden_item.clone()).await.is_empty());

        // A comment edited, then unpublished, keeps the vector current.
        let later = format!("zqlater{}", Uuid::now_v7().simple());
        let id = create_comment(
            app,
            f.story,
            "first draft",
            CommentStatus::Published.as_i16(),
        )
        .await;
        assert!(search(later.clone()).await.is_empty());
        app.state
            .comments()
            .update(
                id,
                UpdateComment {
                    body: Some(format!("edited to say {later}")),
                    body_format: None,
                    status: None,
                    subject: None,
                },
                &admin(),
            )
            .await
            .expect("edit");
        assert_eq!(
            search(later.clone()).await,
            vec![f.story],
            "an edited comment"
        );
        app.state
            .comments()
            .update(
                id,
                UpdateComment {
                    body: None,
                    body_format: None,
                    status: Some(CommentStatus::Unpublished.as_i16()),
                    subject: None,
                },
                &admin(),
            )
            .await
            .expect("unpublish");
        assert!(
            search(later.clone()).await.is_empty(),
            "an unpublished comment drops out"
        );
        app.state
            .comments()
            .delete(id, &admin())
            .await
            .expect("delete");
    });
}

/// Whether `item` is queued for the static search index.
async fn queued(app: &common::TestApp, item: Uuid) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM search_index_queue WHERE item_id = $1)")
        .bind(item)
        .fetch_one(&app.db)
        .await
        .expect("read the search index queue")
}

/// Take `item` off the queue, as a drain would.
async fn clear_queued(app: &common::TestApp, item: Uuid) {
    sqlx::query("DELETE FROM search_index_queue WHERE item_id = $1")
        .bind(item)
        .execute(&app.db)
        .await
        .expect("clear the search index queue");
}

/// The writes a `MAX(changed)` poll never saw (deletes, unpublishes, every
/// comment write) each queue their item for the static index, from the
/// kernel's own write services.
#[test]
fn deletes_unpublishes_and_comment_writes_queue_their_item() {
    run_test(async {
        let app = shared_app().await;
        common::ensure_plugin_tables(app, "trovato_search");
        let f = build_fixture(app).await;

        // An item unpublished.
        clear_queued(app, f.note).await;
        app.state
            .items()
            .update(
                f.note,
                UpdateItem {
                    title: None,
                    status: Some(0),
                    promote: None,
                    sticky: None,
                    fields: None,
                    log: None,
                    stage_id: None,
                    author_id: None,
                    created: None,
                },
                &admin(),
            )
            .await
            .expect("unpublish");
        assert!(queued(app, f.note).await, "unpublishing an item queues it");

        // An item deleted.
        let doomed = create_item(
            app,
            NOTE_TYPE,
            "Doomed",
            serde_json::json!({}),
            1,
            LIVE_STAGE_ID,
        )
        .await;
        clear_queued(app, doomed).await;
        assert!(
            app.state
                .items()
                .delete(doomed, &admin())
                .await
                .expect("delete")
        );
        assert!(queued(app, doomed).await, "deleting an item queues it");

        // A comment created.
        clear_queued(app, f.story).await;
        let id = create_comment(app, f.story, "new", CommentStatus::Published.as_i16()).await;
        assert!(queued(app, f.story).await, "a new comment queues its item");

        // A comment edited.
        clear_queued(app, f.story).await;
        let edit = |body: Option<String>, status: Option<i16>| UpdateComment {
            body,
            body_format: None,
            status,
            subject: None,
        };
        app.state
            .comments()
            .update(id, edit(Some("edited".into()), None), &admin())
            .await
            .expect("edit");
        assert!(
            queued(app, f.story).await,
            "an edited comment queues its item"
        );

        // A comment unpublished.
        clear_queued(app, f.story).await;
        app.state
            .comments()
            .update(
                id,
                edit(None, Some(CommentStatus::Unpublished.as_i16())),
                &admin(),
            )
            .await
            .expect("unpublish");
        assert!(
            queued(app, f.story).await,
            "unpublishing a comment queues its item"
        );

        // A comment published.
        clear_queued(app, f.story).await;
        app.state
            .comments()
            .update(
                id,
                edit(None, Some(CommentStatus::Published.as_i16())),
                &admin(),
            )
            .await
            .expect("publish");
        assert!(
            queued(app, f.story).await,
            "publishing a comment queues its item"
        );

        // A comment deleted.
        clear_queued(app, f.story).await;
        assert!(
            app.state
                .comments()
                .delete(id, &admin())
                .await
                .expect("delete")
        );
        assert!(
            queued(app, f.story).await,
            "deleting a comment queues its item"
        );
    });
}
