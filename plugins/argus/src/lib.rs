//! Argus plugin for Trovato.
//!
//! News intelligence use case: 7 content types for articles, stories, topics,
//! feeds, entities, reactions, and discussions. Validates composite gather
//! responses via includes.

use trovato_sdk::prelude::*;

/// The 7 Argus content types.
///
/// Uses `field_` prefix (new plugin, no existing data constraints).
///
/// Self-references: Several types reference others in this same Vec (e.g.,
/// `argus_article` → `argus_feed`, `argus_topic`, `argus_story`). This is safe
/// because `sync_from_plugins` registers all types without validating reference
/// targets at registration time.
#[plugin_tap]
pub fn tap_item_info() -> Vec<ContentTypeDefinition> {
    vec![
        ContentTypeDefinition {
            machine_name: "argus_article".into(),
            label: "Article".into(),
            description: "Fetched news article with analysis".into(),
            fields: vec![
                FieldDefinition::new("field_url", FieldType::Text { max_length: None })
                    .required()
                    .label("URL"),
                FieldDefinition::new("field_content", FieldType::TextLong).label("Content"),
                FieldDefinition::new("field_relevance_score", FieldType::Float)
                    .label("Relevance Score"),
                FieldDefinition::new("field_summary", FieldType::TextLong).label("Summary"),
                FieldDefinition::new("field_critical_analysis", FieldType::TextLong)
                    .label("Critical Analysis"),
                FieldDefinition::new("field_vector_embedding", FieldType::TextLong)
                    .label("Vector Embedding"),
                FieldDefinition::new(
                    "field_feed_id",
                    FieldType::RecordReference("argus_feed".into()),
                )
                .label("Feed"),
                FieldDefinition::new(
                    "field_topic_id",
                    FieldType::RecordReference("argus_topic".into()),
                )
                .label("Topic"),
                FieldDefinition::new(
                    "field_story_id",
                    FieldType::RecordReference("argus_story".into()),
                )
                .label("Story"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "argus_story".into(),
            label: "Story".into(),
            description: "Aggregated narrative from multiple articles".into(),
            fields: vec![
                FieldDefinition::new("field_summary", FieldType::TextLong)
                    .required()
                    .label("Summary"),
                FieldDefinition::new("field_source_attribution", FieldType::TextLong)
                    .label("Source Attribution"),
                FieldDefinition::new(
                    "field_topic_id",
                    FieldType::RecordReference("argus_topic".into()),
                )
                .label("Topic"),
                FieldDefinition::new("field_article_count", FieldType::Integer)
                    .label("Article Count"),
                FieldDefinition::new("field_relevance_score", FieldType::Float)
                    .label("Relevance Score"),
                FieldDefinition::new("field_active", FieldType::Boolean).label("Active"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "argus_topic".into(),
            label: "Topic".into(),
            description: "Monitored topic with relevance criteria".into(),
            fields: vec![
                FieldDefinition::new("field_name", FieldType::Text { max_length: None })
                    .required()
                    .label("Name"),
                FieldDefinition::new("field_relevance_prompt", FieldType::TextLong)
                    .label("Relevance Prompt"),
                FieldDefinition::new("field_threshold", FieldType::Float).label("Threshold"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "argus_feed".into(),
            label: "Feed".into(),
            description: "RSS/Atom feed source".into(),
            fields: vec![
                FieldDefinition::new("field_url", FieldType::Text { max_length: None })
                    .required()
                    .label("URL"),
                FieldDefinition::new("field_name", FieldType::Text { max_length: None })
                    .required()
                    .label("Name"),
                FieldDefinition::new("field_fetch_interval", FieldType::Integer)
                    .label("Fetch Interval"),
                FieldDefinition::new("field_health_status", FieldType::Text { max_length: None })
                    .label("Health Status"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "argus_entity".into(),
            label: "Entity".into(),
            description: "Named entity (person, org, place) extracted from articles".into(),
            fields: vec![
                FieldDefinition::new("field_canonical_name", FieldType::Text { max_length: None })
                    .required()
                    .label("Canonical Name"),
                FieldDefinition::new("field_aliases", FieldType::TextLong).label("Aliases"),
                FieldDefinition::new("field_type", FieldType::Text { max_length: None })
                    .label("Entity Type"),
                FieldDefinition::new("field_description", FieldType::TextLong).label("Description"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "argus_reaction".into(),
            label: "Reaction".into(),
            description: "User reaction to content".into(),
            fields: vec![
                FieldDefinition::new("field_user_id", FieldType::Text { max_length: None })
                    .required()
                    .label("User"),
                FieldDefinition::new("field_item_id", FieldType::Text { max_length: None })
                    .required()
                    .label("Item"),
                FieldDefinition::new("field_reaction_type", FieldType::Text { max_length: None })
                    .required()
                    .label("Reaction Type"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "argus_discussion".into(),
            label: "Discussion".into(),
            description: "Threaded discussion on a story".into(),
            fields: vec![
                FieldDefinition::new(
                    "field_story_id",
                    FieldType::RecordReference("argus_story".into()),
                )
                .required()
                .label("Story"),
                FieldDefinition::new(
                    "field_parent_id",
                    FieldType::RecordReference("argus_discussion".into()),
                )
                .label("Parent"),
                FieldDefinition::new("field_user_id", FieldType::Text { max_length: None })
                    .required()
                    .label("User"),
                FieldDefinition::new("field_content", FieldType::TextLong)
                    .required()
                    .label("Content"),
            ],
        },
    ]
}

const ARGUS_TYPES: &[&str] = &[
    "argus_article",
    "argus_story",
    "argus_topic",
    "argus_feed",
    "argus_entity",
    "argus_reaction",
    "argus_discussion",
];

/// Permissions: view / create / edit / delete for each of the 7 content types.
///
/// Permission format matches kernel fallback: "{operation} {type} content".
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    ARGUS_TYPES
        .iter()
        .flat_map(|t| PermissionDefinition::crud_for_type(t))
        .collect()
}

/// Menu routes: /stories and /feeds listings.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/stories", "Stories")
            .callback("argus_story_list")
            .permission("access content"),
        MenuDefinition::new("/feeds", "Feeds")
            .callback("argus_feed_list")
            .permission("access content"),
    ]
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn item_info_returns_seven_types() {
        let types = __inner_tap_item_info();
        assert_eq!(types.len(), 7);
        let names: Vec<&str> = types.iter().map(|t| t.machine_name.as_str()).collect();
        assert!(names.contains(&"argus_article"));
        assert!(names.contains(&"argus_story"));
        assert!(names.contains(&"argus_topic"));
        assert!(names.contains(&"argus_feed"));
        assert!(names.contains(&"argus_entity"));
        assert!(names.contains(&"argus_reaction"));
        assert!(names.contains(&"argus_discussion"));
    }

    #[test]
    fn argus_article_has_nine_fields() {
        let types = __inner_tap_item_info();
        let article = types
            .iter()
            .find(|t| t.machine_name == "argus_article")
            .unwrap();
        assert_eq!(article.fields.len(), 9);
    }

    #[test]
    fn perm_returns_twenty_eight_permissions() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 28); // 4 per type × 7 types (view/create/edit/delete)
    }

    #[test]
    fn menu_returns_two_routes() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 2);
        assert_eq!(menus[0].path, "/stories");
        assert_eq!(menus[1].path, "/feeds");
    }

    #[test]
    fn perm_format_matches_kernel_fallback() {
        let perms = __inner_tap_perm();
        for perm in &perms {
            assert!(
                !perm.name.contains(" any "),
                "permission '{}' must not contain 'any' — kernel fallback uses '{{op}} {{type}} content'",
                perm.name
            );
        }
    }
}
