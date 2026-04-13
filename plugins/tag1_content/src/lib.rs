//! Tag1 Consulting content types.
//!
//! Registers four content types for the Tag1.com migration:
//! - `blog` — Blog posts, team talks, white papers, how-to guides
//! - `case_study` — Client case studies
//! - `team_member` — Team directory
//! - `composed_page` — Service pages, product pages, and static pages

use trovato_sdk::prelude::*;

// -------------------------------------------------------------------------
// Content type definitions
// -------------------------------------------------------------------------

/// Register all Tag1 content types.
#[plugin_tap]
pub fn tap_item_info() -> Vec<ContentTypeDefinition> {
    vec![
        blog_type(),
        case_study_type(),
        team_member_type(),
        composed_page_type(),
    ]
}

/// Blog posts, team talks, white papers, and how-to guides.
///
/// Uses `collection_type` to discriminate between sub-types within
/// the unified "Insights" collection.
fn blog_type() -> ContentTypeDefinition {
    ContentTypeDefinition {
        machine_name: "blog".into(),
        label: "Blog Post".into(),
        description: "Blog posts, team talks, white papers, and how-to guides".into(),
        title_label: None,
        fields: vec![
            FieldDefinition::new("field_body", FieldType::PageBuilder)
                .required()
                .label("Body"),
            FieldDefinition::new(
                "field_summary",
                FieldType::Text {
                    max_length: Some(300),
                },
            )
            .label("Summary"),
            FieldDefinition::new(
                "field_tags",
                FieldType::RecordReference("category_term".into()),
            )
            .cardinality(-1)
            .label("Tags"),
            FieldDefinition::new(
                "field_author",
                FieldType::RecordReference("team_member".into()),
            )
            .label("Author"),
            FieldDefinition::new("field_image", FieldType::File).label("Hero Image"),
            FieldDefinition::new("field_image_alt", FieldType::Text { max_length: None })
                .label("Image Alt Text"),
            FieldDefinition::new(
                "field_collection_type",
                FieldType::Text {
                    max_length: Some(50),
                },
            )
            .label("Collection Type"),
            FieldDefinition::new("field_series_title", FieldType::Text { max_length: None })
                .label("Series Title"),
            FieldDefinition::new("field_series_weight", FieldType::Integer).label("Series Weight"),
        ],
    }
}

/// Client case studies with structured challenge/solution/results.
fn case_study_type() -> ContentTypeDefinition {
    ContentTypeDefinition {
        machine_name: "case_study".into(),
        label: "Case Study".into(),
        description: "Client case studies with challenge, solution, and results".into(),
        title_label: None,
        fields: vec![
            FieldDefinition::new("field_body", FieldType::PageBuilder)
                .required()
                .label("Body"),
            FieldDefinition::new("field_client_name", FieldType::Text { max_length: None })
                .label("Client Name"),
            FieldDefinition::new("field_challenge", FieldType::TextLong).label("Challenge"),
            FieldDefinition::new("field_solution", FieldType::TextLong).label("Solution"),
            FieldDefinition::new("field_results", FieldType::TextLong).label("Results"),
            FieldDefinition::new("field_image", FieldType::File).label("Image"),
            FieldDefinition::new("field_image_alt", FieldType::Text { max_length: None })
                .label("Image Alt Text"),
            FieldDefinition::new("field_logo", FieldType::File).label("Client Logo"),
            FieldDefinition::new(
                "field_tags",
                FieldType::RecordReference("category_term".into()),
            )
            .cardinality(-1)
            .label("Tags"),
        ],
    }
}

/// Team member profiles for the team directory.
fn team_member_type() -> ContentTypeDefinition {
    ContentTypeDefinition {
        machine_name: "team_member".into(),
        label: "Team Member".into(),
        description: "Team member profile with bio and social links".into(),
        title_label: Some("Full Name".into()),
        fields: vec![
            FieldDefinition::new("field_first_name", FieldType::Text { max_length: None })
                .required()
                .label("First Name"),
            FieldDefinition::new("field_last_name", FieldType::Text { max_length: None })
                .required()
                .label("Last Name"),
            FieldDefinition::new(
                "field_shortname",
                FieldType::Text {
                    max_length: Some(64),
                },
            )
            .required()
            .label("Short Name (URL slug)"),
            FieldDefinition::new("field_role", FieldType::Text { max_length: None })
                .label("Role / Title"),
            FieldDefinition::new("field_bio", FieldType::TextLong).label("Biography"),
            FieldDefinition::new("field_bio_highlight", FieldType::Text { max_length: None })
                .label("Bio Highlight"),
            FieldDefinition::new("field_image", FieldType::File).label("Profile Photo"),
            FieldDefinition::new("field_email", FieldType::Email).label("Email"),
            FieldDefinition::new("field_linkedin_url", FieldType::Text { max_length: None })
                .label("LinkedIn URL"),
            FieldDefinition::new("field_github_url", FieldType::Text { max_length: None })
                .label("GitHub URL"),
            FieldDefinition::new("field_twitter_url", FieldType::Text { max_length: None })
                .label("Twitter URL"),
            FieldDefinition::new("field_mastodon_url", FieldType::Text { max_length: None })
                .label("Mastodon URL"),
            FieldDefinition::new("field_bluesky_url", FieldType::Text { max_length: None })
                .label("Bluesky URL"),
            FieldDefinition::new("field_drupalorg_url", FieldType::Text { max_length: None })
                .label("Drupal.org URL"),
            FieldDefinition::new(
                "field_expertise",
                FieldType::RecordReference("category_term".into()),
            )
            .cardinality(-1)
            .label("Expertise Areas"),
        ],
    }
}

/// Composed pages built with the visual page builder.
///
/// Used for service pages, product pages, and static pages.
fn composed_page_type() -> ContentTypeDefinition {
    ContentTypeDefinition {
        machine_name: "composed_page".into(),
        label: "Composed Page".into(),
        description: "Rich pages built with the visual page builder".into(),
        title_label: None,
        fields: vec![
            FieldDefinition::new("field_body", FieldType::PageBuilder)
                .required()
                .label("Body"),
            FieldDefinition::new(
                "field_summary",
                FieldType::Text {
                    max_length: Some(300),
                },
            )
            .label("Summary / Meta Description"),
            FieldDefinition::new("field_image", FieldType::File).label("OG Image"),
        ],
    }
}

// -------------------------------------------------------------------------
// Permissions
// -------------------------------------------------------------------------

/// Register CRUD permissions for all Tag1 content types.
#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    let mut perms = Vec::new();
    perms.extend(PermissionDefinition::crud_for_type("blog"));
    perms.extend(PermissionDefinition::crud_for_type("case_study"));
    perms.extend(PermissionDefinition::crud_for_type("team_member"));
    perms.extend(PermissionDefinition::crud_for_type("composed_page"));
    perms
}

// -------------------------------------------------------------------------
// Access control
// -------------------------------------------------------------------------

/// Author-based access control: authors can edit/delete their own content.
#[plugin_tap]
pub fn tap_item_access(input: ItemAccessInput) -> AccessResult {
    let our_types = ["blog", "case_study", "team_member", "composed_page"];
    if !our_types.contains(&input.item_type.as_str()) {
        return AccessResult::Neutral;
    }

    // Authors can always access their own items
    if input.user_id == input.author_id {
        return AccessResult::Grant;
    }

    AccessResult::Neutral
}

// -------------------------------------------------------------------------
// Menu routes
// -------------------------------------------------------------------------

/// Register front-end menu routes.
#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/blog", "Insights").callback("blog_listing"),
        MenuDefinition::new("/our-work", "Our Work").callback("case_study_listing"),
        MenuDefinition::new("/why-tag1/team", "Team").callback("team_listing"),
    ]
}
