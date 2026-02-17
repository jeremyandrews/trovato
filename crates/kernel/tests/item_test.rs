//! Integration tests for Item CRUD operations.
//!
//! These tests verify the Item model, ItemService, and related functionality.

use trovato_kernel::content::{FilterPipeline, FormBuilder};
use trovato_kernel::models::{
    CreateItem, CreateItemType, Item, ItemRevision, ItemType, UpdateItem,
};
use trovato_kernel::tap::{RequestState, UserContext};
use trovato_sdk::types::{
    AccessResult, ContentTypeDefinition, FieldDefinition, FieldType, Item as SdkItem,
    MenuDefinition, PermissionDefinition, RecordRef, TextValue,
};
use uuid::Uuid;

// ============================================================================
// Filter Pipeline Tests
// ============================================================================

#[test]
fn filter_plain_text_escapes_html() {
    let pipeline = FilterPipeline::for_format("plain_text");
    let input = "<script>alert('xss')</script>";
    let output = pipeline.process(input);

    assert!(!output.contains("<script>"));
    assert!(output.contains("&lt;script&gt;"));
}

#[test]
fn filter_filtered_html_removes_scripts() {
    let pipeline = FilterPipeline::for_format("filtered_html");
    let input = "<p>Hello</p><script>alert('xss')</script><p>World</p>";
    let output = pipeline.process(input);

    assert!(output.contains("<p>Hello</p>"));
    assert!(output.contains("<p>World</p>"));
    assert!(!output.contains("<script>"));
}

#[test]
fn filter_filtered_html_removes_event_handlers() {
    let pipeline = FilterPipeline::for_format("filtered_html");
    let input = r#"<a href="/page" onclick="alert('xss')">Link</a>"#;
    let output = pipeline.process(input);

    assert!(!output.contains("onclick"));
    assert!(output.contains(r#"href="/page""#));
}

#[test]
fn filter_full_html_allows_everything() {
    let pipeline = FilterPipeline::for_format("full_html");
    let input = "<script>alert('test')</script>";
    let output = pipeline.process(input);

    assert_eq!(input, output);
}

// ============================================================================
// Form Builder Tests
// ============================================================================

fn test_content_type() -> ContentTypeDefinition {
    ContentTypeDefinition {
        machine_name: "blog".to_string(),
        label: "Blog Post".to_string(),
        description: "A blog article".to_string(),
        fields: vec![
            FieldDefinition {
                field_name: "body".to_string(),
                field_type: FieldType::TextLong,
                label: "Body".to_string(),
                required: true,
                cardinality: 1,
                settings: serde_json::json!({}),
            },
            FieldDefinition {
                field_name: "summary".to_string(),
                field_type: FieldType::Text {
                    max_length: Some(255),
                },
                label: "Summary".to_string(),
                required: false,
                cardinality: 1,
                settings: serde_json::json!({}),
            },
            FieldDefinition {
                field_name: "featured".to_string(),
                field_type: FieldType::Boolean,
                label: "Featured".to_string(),
                required: false,
                cardinality: 1,
                settings: serde_json::json!({}),
            },
        ],
    }
}

#[test]
fn form_builder_add_form_includes_title() {
    let builder = FormBuilder::new(test_content_type());
    let form = builder.build_add_form("/item/add/blog");

    assert!(form.contains(r#"name="title""#));
    assert!(form.contains(r#"action="/item/add/blog""#));
    assert!(form.contains(r#"type="submit""#));
}

#[test]
fn form_builder_add_form_includes_all_fields() {
    let builder = FormBuilder::new(test_content_type());
    let form = builder.build_add_form("/item/add/blog");

    // Check for body field (TextLong -> textarea)
    assert!(form.contains(r#"name="body""#));
    assert!(form.contains("<textarea"));

    // Check for summary field (Text -> input)
    assert!(form.contains(r#"name="summary""#));

    // Check for featured field (Boolean -> checkbox)
    assert!(form.contains(r#"name="featured""#));
    assert!(form.contains(r#"type="checkbox""#));
}

#[test]
fn form_builder_add_form_marks_required_fields() {
    let builder = FormBuilder::new(test_content_type());
    let form = builder.build_add_form("/item/add/blog");

    // Body is required
    assert!(form.contains("Body *")); // Label with asterisk
}

#[test]
fn form_builder_edit_form_includes_values() {
    let builder = FormBuilder::new(test_content_type());

    let item = Item {
        id: Uuid::now_v7(),
        current_revision_id: Some(Uuid::now_v7()),
        item_type: "blog".to_string(),
        title: "Test Post".to_string(),
        author_id: Uuid::nil(),
        status: 1,
        created: 0,
        changed: 0,
        promote: 0,
        sticky: 0,
        fields: serde_json::json!({
            "body": {"value": "Hello world", "format": "filtered_html"},
            "summary": {"value": "A test summary"},
            "featured": {"value": true}
        }),
        stage_id: "live".to_string(),
        language: "en".to_string(),
    };

    let form = builder.build_edit_form(&item, "/item/123/edit");

    // Title value is included
    assert!(form.contains(r#"value="Test Post""#));

    // Body value is included
    assert!(form.contains("Hello world"));

    // Summary value is included
    assert!(form.contains("A test summary"));

    // Checkbox is checked for featured
    assert!(form.contains("checked"));
}

// ============================================================================
// Item Model Tests
// ============================================================================

#[test]
fn item_status_checks() {
    let item = Item {
        id: Uuid::now_v7(),
        current_revision_id: Some(Uuid::now_v7()),
        item_type: "page".to_string(),
        title: "Test".to_string(),
        author_id: Uuid::nil(),
        status: 1,
        created: 0,
        changed: 0,
        promote: 1,
        sticky: 0,
        fields: serde_json::json!({}),
        stage_id: "live".to_string(),
        language: "en".to_string(),
    };

    assert!(item.is_published());
    assert!(item.is_promoted());
    assert!(!item.is_sticky());
}

#[test]
fn create_item_input_defaults() {
    let input = CreateItem {
        item_type: "blog".to_string(),
        title: "Test Post".to_string(),
        author_id: Uuid::nil(),
        status: None,
        promote: None,
        sticky: None,
        fields: None,
        stage_id: None,
        language: None,
        log: None,
    };

    assert_eq!(input.item_type, "blog");
    assert!(input.status.is_none());
    assert!(input.fields.is_none());
}

#[test]
fn update_item_input_partial() {
    let input = UpdateItem {
        title: Some("Updated Title".to_string()),
        status: None,
        promote: None,
        sticky: None,
        fields: None,
        log: Some("Changed title".to_string()),
    };

    assert!(input.title.is_some());
    assert!(input.status.is_none());
    assert!(input.log.is_some());
}

// ============================================================================
// ItemType Model Tests
// ============================================================================

#[test]
fn create_item_type_input() {
    let input = CreateItemType {
        type_name: "blog".to_string(),
        label: "Blog Post".to_string(),
        description: Some("A blog article".to_string()),
        has_title: Some(true),
        title_label: Some("Title".to_string()),
        plugin: "blog".to_string(),
        settings: Some(serde_json::json!({"fields": []})),
    };

    assert_eq!(input.type_name, "blog");
    assert_eq!(input.label, "Blog Post");
}

// ============================================================================
// UserContext Tests
// ============================================================================

#[test]
fn user_context_anonymous() {
    let ctx = UserContext::anonymous();
    assert!(!ctx.authenticated);
    assert!(!ctx.is_admin());
    assert!(!ctx.has_permission("edit content"));
}

#[test]
fn user_context_admin() {
    let ctx = UserContext::authenticated(Uuid::now_v7(), vec!["administer site".to_string()]);

    assert!(ctx.authenticated);
    assert!(ctx.is_admin());
}

#[test]
fn user_context_permissions() {
    let ctx = UserContext::authenticated(
        Uuid::now_v7(),
        vec![
            "access content".to_string(),
            "create page content".to_string(),
            "edit own page content".to_string(),
        ],
    );

    assert!(ctx.has_permission("access content"));
    assert!(ctx.has_permission("create page content"));
    assert!(!ctx.has_permission("delete any content"));
}

// ============================================================================
// Request State Tests
// ============================================================================

#[test]
fn request_state_without_services() {
    let state = RequestState::without_services(UserContext::anonymous());
    assert!(!state.has_services());
    assert_eq!(state.user.id, Uuid::nil());
}

#[test]
fn request_state_context() {
    let mut state = RequestState::without_services(UserContext::anonymous());

    assert!(state.get_context("key").is_none());

    state.set_context("key".to_string(), "value".to_string());

    assert_eq!(state.get_context("key"), Some("value"));
}

// ============================================================================
// Content Type Definition Tests
// ============================================================================

#[test]
fn content_type_definition_fields() {
    let ct = test_content_type();

    assert_eq!(ct.machine_name, "blog");
    assert_eq!(ct.fields.len(), 3);

    let body = &ct.fields[0];
    assert_eq!(body.field_name, "body");
    assert!(body.required);
}

#[test]
fn field_definition_builder() {
    let field = FieldDefinition::new(
        "title",
        FieldType::Text {
            max_length: Some(255),
        },
    )
    .label("Article Title")
    .required()
    .cardinality(1);

    assert_eq!(field.field_name, "title");
    assert_eq!(field.label, "Article Title");
    assert!(field.required);
    assert_eq!(field.cardinality, 1);
}

// ============================================================================
// SDK Types Serialization Tests
// ============================================================================

#[test]
fn text_value_plain() {
    let tv = TextValue::plain("Hello world");
    assert_eq!(tv.value, "Hello world");
    assert_eq!(tv.format, "plain_text");
}

#[test]
fn text_value_html() {
    let tv = TextValue::html("<p>Paragraph</p>");
    assert_eq!(tv.format, "filtered_html");
}

#[test]
fn text_value_custom() {
    let tv = TextValue::new("content", "full_html");
    assert_eq!(tv.value, "content");
    assert_eq!(tv.format, "full_html");
}

#[test]
fn record_ref_construction() {
    let id = Uuid::now_v7();
    let rr = RecordRef::new(id, "user");
    assert_eq!(rr.target_id, id);
    assert_eq!(rr.target_type, "user");
}

#[test]
fn access_result_serialization() {
    let grant = AccessResult::Grant;
    let deny = AccessResult::Deny;
    let neutral = AccessResult::Neutral;

    let grant_json = serde_json::to_string(&grant).unwrap();
    let deny_json = serde_json::to_string(&deny).unwrap();
    let neutral_json = serde_json::to_string(&neutral).unwrap();

    assert_eq!(grant_json, "\"Grant\"");
    assert_eq!(deny_json, "\"Deny\"");
    assert_eq!(neutral_json, "\"Neutral\"");

    // Roundtrip
    let parsed: AccessResult = serde_json::from_str(&grant_json).unwrap();
    assert_eq!(parsed, AccessResult::Grant);
}

#[test]
fn menu_definition_builder() {
    let menu = MenuDefinition::new("/admin/content", "Content")
        .callback("admin_content_list")
        .permission("administer content")
        .parent("/admin");

    assert_eq!(menu.path, "/admin/content");
    assert_eq!(menu.title, "Content");
    assert_eq!(menu.callback, "admin_content_list");
    assert_eq!(menu.permission, "administer content");
    assert_eq!(menu.parent, Some("/admin".to_string()));
}

#[test]
fn permission_definition() {
    let perm = PermissionDefinition::new("create blog content", "Allow creating blog posts");
    assert_eq!(perm.name, "create blog content");
    assert_eq!(perm.description, "Allow creating blog posts");
}

#[test]
fn sdk_item_field_access() {
    use std::collections::HashMap;

    let mut fields = HashMap::new();
    fields.insert(
        "body".to_string(),
        serde_json::json!({"value": "Hello world", "format": "filtered_html"}),
    );
    fields.insert("views".to_string(), serde_json::json!(100));

    let item = SdkItem {
        id: Uuid::now_v7(),
        item_type: "blog".to_string(),
        title: "Test".to_string(),
        fields,
        status: 1,
        author_id: Uuid::nil(),
        revision_id: None,
        stage_id: None,
        created: 0,
        changed: 0,
    };

    // Get text value
    let text = item.get_text_value("body");
    assert!(text.is_some());
    let tv = text.unwrap();
    assert_eq!(tv.value, "Hello world");

    // Get simple string via get_text
    let simple = item.get_text("body");
    assert_eq!(simple, Some("Hello world".to_string()));

    // Field that doesn't exist
    assert!(item.get_text("nonexistent").is_none());
}

#[test]
fn sdk_item_set_field() {
    use std::collections::HashMap;

    let mut item = SdkItem {
        id: Uuid::now_v7(),
        item_type: "page".to_string(),
        title: "Test".to_string(),
        fields: HashMap::new(),
        status: 1,
        author_id: Uuid::nil(),
        revision_id: None,
        stage_id: None,
        created: 0,
        changed: 0,
    };

    item.set_field("tags", vec!["rust", "wasm"]);

    let tags: Option<Vec<String>> = item.get_field("tags");
    assert!(tags.is_some());
    assert_eq!(tags.unwrap(), vec!["rust", "wasm"]);
}

// ============================================================================
// Filter Pipeline Edge Cases
// ============================================================================

#[test]
fn filter_plain_text_converts_newlines() {
    let pipeline = FilterPipeline::for_format("plain_text");
    let input = "Line 1\nLine 2";
    let output = pipeline.process(input);
    assert!(output.contains("<br>"));
}

#[test]
fn filter_unknown_format_defaults_to_plain_text() {
    let pipeline = FilterPipeline::for_format("unknown_format");
    let input = "<b>bold</b>";
    let output = pipeline.process(input);
    // Should escape HTML
    assert!(output.contains("&lt;b&gt;"));
}

#[test]
fn filter_filtered_html_removes_style_tags() {
    let pipeline = FilterPipeline::for_format("filtered_html");
    let input = "<style>body{color:red}</style><p>Text</p>";
    let output = pipeline.process(input);
    assert!(!output.contains("<style>"));
    assert!(output.contains("<p>Text</p>"));
}

#[test]
fn filter_filtered_html_removes_data_urls() {
    let pipeline = FilterPipeline::for_format("filtered_html");
    let input = r#"<img src="data:image/svg+xml;base64,PHN2Zz4..." alt="test">"#;
    let output = pipeline.process(input);
    assert!(!output.contains("data:"));
}

#[test]
fn filter_filtered_html_converts_bare_urls() {
    let pipeline = FilterPipeline::for_format("filtered_html");
    let input = "Check out https://example.com for more info.";
    let output = pipeline.process(input);
    assert!(output.contains("<a href=\"https://example.com\""));
    assert!(output.contains("target=\"_blank\""));
    assert!(output.contains("rel=\"noopener\""));
}

#[test]
fn filter_filtered_html_keeps_existing_links() {
    let pipeline = FilterPipeline::for_format("filtered_html");
    let input = r#"<a href="https://example.com">Link</a>"#;
    let output = pipeline.process(input);
    // Should not double-wrap the URL
    assert_eq!(output.matches("href=").count(), 1);
}

// ============================================================================
// FormBuilder All Field Types
// ============================================================================

fn comprehensive_content_type() -> ContentTypeDefinition {
    ContentTypeDefinition {
        machine_name: "test".to_string(),
        label: "Test Content".to_string(),
        description: "Tests all field types".to_string(),
        fields: vec![
            FieldDefinition::new("body", FieldType::TextLong)
                .label("Body")
                .required(),
            FieldDefinition::new(
                "summary",
                FieldType::Text {
                    max_length: Some(255),
                },
            )
            .label("Summary"),
            FieldDefinition::new("views", FieldType::Integer).label("View Count"),
            FieldDefinition::new("rating", FieldType::Float).label("Rating"),
            FieldDefinition::new("featured", FieldType::Boolean).label("Featured"),
            FieldDefinition::new("publish_date", FieldType::Date).label("Publish Date"),
            FieldDefinition::new("contact", FieldType::Email).label("Contact Email"),
            FieldDefinition::new("related", FieldType::RecordReference("article".to_string()))
                .label("Related Article"),
            FieldDefinition::new("attachment", FieldType::File).label("Attachment"),
        ],
    }
}

#[test]
fn form_builder_renders_all_field_types() {
    let builder = FormBuilder::new(comprehensive_content_type());
    let form = builder.build_add_form("/item/add/test");

    // TextLong -> textarea
    assert!(form.contains("<textarea"));
    assert!(form.contains(r#"name="body""#));

    // Text with maxlength
    assert!(form.contains(r#"name="summary""#));
    assert!(form.contains("maxlength=\"255\""));

    // Integer -> number input
    assert!(form.contains(r#"name="views""#));
    assert!(form.contains(r#"type="number""#));

    // Float -> number with step
    assert!(form.contains(r#"name="rating""#));
    assert!(form.contains("step=\"any\""));

    // Boolean -> checkbox
    assert!(form.contains(r#"name="featured""#));
    assert!(form.contains(r#"type="checkbox""#));

    // Date
    assert!(form.contains(r#"name="publish_date""#));
    assert!(form.contains(r#"type="date""#));

    // Email
    assert!(form.contains(r#"name="contact""#));
    assert!(form.contains(r#"type="email""#));

    // RecordReference -> UUID input with help
    assert!(form.contains(r#"name="related""#));
    assert!(form.contains("UUID"));

    // File
    assert!(form.contains(r#"name="attachment""#));
    assert!(form.contains(r#"type="file""#));
}

#[test]
fn form_builder_edit_form_populates_all_types() {
    let builder = FormBuilder::new(comprehensive_content_type());

    let item = Item {
        id: Uuid::now_v7(),
        current_revision_id: Some(Uuid::now_v7()),
        item_type: "test".to_string(),
        title: "Test Item".to_string(),
        author_id: Uuid::nil(),
        status: 0, // Unpublished
        created: 0,
        changed: 0,
        promote: 0,
        sticky: 0,
        fields: serde_json::json!({
            "body": {"value": "Body content", "format": "plain_text"},
            "summary": {"value": "Summary text"},
            "views": {"value": 42},
            "rating": {"value": 4.5},
            "featured": {"value": true},
            "publish_date": {"value": "2026-01-15"},
            "contact": {"value": "test@example.com"},
            "related": {"target_id": "550e8400-e29b-41d4-a716-446655440000"},
        }),
        stage_id: "live".to_string(),
        language: "en".to_string(),
    };

    let form = builder.build_edit_form(&item, "/item/123/edit");

    // Values populated
    assert!(form.contains("Body content"));
    assert!(form.contains("Summary text"));
    assert!(form.contains("value=\"42\""));
    assert!(form.contains("value=\"4.5\""));
    assert!(form.contains("checked")); // featured checkbox
    assert!(form.contains("2026-01-15"));
    assert!(form.contains("test@example.com"));
    assert!(form.contains("550e8400-e29b-41d4-a716-446655440000"));

    // Unpublished - checkbox not checked (status=0)
    assert!(form.contains(r#"<input type="checkbox" name="status""#));
}

#[test]
fn form_builder_escapes_html_in_values() {
    let builder = FormBuilder::new(test_content_type());

    let item = Item {
        id: Uuid::now_v7(),
        current_revision_id: Some(Uuid::now_v7()),
        item_type: "blog".to_string(),
        title: "<script>alert('xss')</script>".to_string(),
        author_id: Uuid::nil(),
        status: 1,
        created: 0,
        changed: 0,
        promote: 0,
        sticky: 0,
        fields: serde_json::json!({
            "body": {"value": "<b>bold</b>", "format": "filtered_html"},
        }),
        stage_id: "live".to_string(),
        language: "en".to_string(),
    };

    let form = builder.build_edit_form(&item, "/item/123/edit");

    // Title should be escaped
    assert!(form.contains("&lt;script&gt;"));
    assert!(!form.contains("<script>alert"));

    // Body value should be escaped
    assert!(form.contains("&lt;b&gt;bold&lt;/b&gt;"));
}

// ============================================================================
// ItemRevision Tests
// ============================================================================

#[test]
fn item_revision_struct() {
    let rev = ItemRevision {
        id: Uuid::now_v7(),
        item_id: Uuid::now_v7(),
        author_id: Uuid::nil(),
        title: "Revision Title".to_string(),
        status: 1,
        fields: serde_json::json!({"body": {"value": "Content"}}),
        created: 1700000000,
        log: Some("Updated body".to_string()),
    };

    assert_eq!(rev.title, "Revision Title");
    assert!(rev.log.is_some());
}

#[test]
fn item_revision_optional_log() {
    let rev = ItemRevision {
        id: Uuid::now_v7(),
        item_id: Uuid::now_v7(),
        author_id: Uuid::nil(),
        title: "No Log".to_string(),
        status: 1,
        fields: serde_json::json!({}),
        created: 0,
        log: None,
    };

    assert!(rev.log.is_none());
}

// ============================================================================
// Additional Model Tests
// ============================================================================

#[test]
fn item_status_unpublished() {
    let item = Item {
        id: Uuid::now_v7(),
        current_revision_id: None,
        item_type: "page".to_string(),
        title: "Draft".to_string(),
        author_id: Uuid::nil(),
        status: 0,
        created: 0,
        changed: 0,
        promote: 0,
        sticky: 1,
        fields: serde_json::json!({}),
        stage_id: "live".to_string(),
        language: "en".to_string(),
    };

    assert!(!item.is_published());
    assert!(!item.is_promoted());
    assert!(item.is_sticky());
}

#[test]
fn item_type_struct() {
    let it = ItemType {
        type_name: "blog".to_string(),
        label: "Blog Post".to_string(),
        description: Some("A blog article".to_string()),
        has_title: true,
        title_label: Some("Title".to_string()),
        plugin: "blog".to_string(),
        settings: serde_json::json!({"fields": []}),
    };

    assert_eq!(it.type_name, "blog");
    assert!(it.description.is_some());
}

#[test]
fn create_item_with_all_fields() {
    let input = CreateItem {
        item_type: "page".to_string(),
        title: "Full Page".to_string(),
        author_id: Uuid::now_v7(),
        status: Some(0),
        promote: Some(1),
        sticky: Some(1),
        fields: Some(serde_json::json!({"body": {"value": "Content"}})),
        stage_id: Some("preview".to_string()),
        language: Some("en".to_string()),
        log: Some("Initial creation".to_string()),
    };

    assert_eq!(input.status, Some(0));
    assert_eq!(input.promote, Some(1));
    assert_eq!(input.stage_id, Some("preview".to_string()));
}

#[test]
fn update_item_with_all_fields() {
    let input = UpdateItem {
        title: Some("New Title".to_string()),
        status: Some(1),
        promote: Some(1),
        sticky: Some(0),
        fields: Some(serde_json::json!({"body": {"value": "Updated"}})),
        log: Some("Major revision".to_string()),
    };

    assert!(input.title.is_some());
    assert!(input.fields.is_some());
}

// ============================================================================
// UserContext Additional Tests
// ============================================================================

#[test]
fn user_context_empty_permissions() {
    let ctx = UserContext::authenticated(Uuid::now_v7(), vec![]);
    assert!(ctx.authenticated);
    assert!(!ctx.is_admin());
    assert!(!ctx.has_permission("anything"));
}

#[test]
fn user_context_multiple_permission_checks() {
    let ctx = UserContext::authenticated(
        Uuid::now_v7(),
        vec![
            "access content".to_string(),
            "create blog content".to_string(),
            "edit own blog content".to_string(),
        ],
    );

    // Has these
    assert!(ctx.has_permission("access content"));
    assert!(ctx.has_permission("create blog content"));
    assert!(ctx.has_permission("edit own blog content"));

    // Doesn't have these
    assert!(!ctx.has_permission("delete any content"));
    assert!(!ctx.has_permission("administer site"));
    assert!(!ctx.has_permission("create page content"));
}

// ============================================================================
// ContentTypeDefinition Tests
// ============================================================================

#[test]
fn content_type_serialization_roundtrip() {
    let ct = comprehensive_content_type();
    let json = serde_json::to_string(&ct).unwrap();
    let parsed: ContentTypeDefinition = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.machine_name, "test");
    assert_eq!(parsed.fields.len(), 9);
}

#[test]
fn field_type_serialization() {
    // Text with max length
    let text = FieldType::Text {
        max_length: Some(255),
    };
    let json = serde_json::to_string(&text).unwrap();
    assert!(json.contains("255"));

    // TextLong
    let long = FieldType::TextLong;
    let json = serde_json::to_string(&long).unwrap();
    assert_eq!(json, "\"TextLong\"");

    // RecordReference
    let rr = FieldType::RecordReference("article".to_string());
    let json = serde_json::to_string(&rr).unwrap();
    assert!(json.contains("article"));
}
