//! Media library plugin for Trovato (BL-115 / B290).
//!
//! Declares four media types, using the same `ContentTypeDefinition`/
//! `FieldDefinition` machinery any content type uses — a media type is a
//! content type, nothing more:
//!
//! - `media_file`: an uploaded file of any allowed kind.
//! - `media_image`: an uploaded image, with alt text, caption and credit.
//! - `media_remote_video`: a remote video, named by its URL.
//! - `media_remote_embed`: a remote page to embed, named by its URL.
//!
//! Registers all four as media sources (`tap_media_source`) — the seam a
//! later plugin uses to add its own media type (a document preview source,
//! say): declare a content type as usual, then name it here. The kernel's
//! media library (`/admin/content/media`) and the media picker widget (a
//! `RecordReference("media")` field) read this registry, not this plugin,
//! so a new source needs no kernel change.
//!
//! Permissions are flat (`access media overview`, `create media`, `edit own
//! media`, `edit any media`, `delete own media`, `delete any media`),
//! matching Drupal's media module rather than the kernel's usual
//! per-content-type pattern (`crud_for_type` would give `edit any
//! media_file`, `edit any media_image`, and so on) — one media library, one
//! set of permissions, whichever of the four types an item happens to be.
//! `tap_item_access` is where that flat set is enforced: the kernel's own
//! `"{op} any/own {type}"` fallback does not know these names, so this plugin
//! grants directly from `user_permissions`.

use trovato_sdk::prelude::*;

/// The four media types this plugin declares, with the permission used to
/// check them (the flat set every tap below shares).
const MEDIA_TYPES: &[&str] = &[
    "media_file",
    "media_image",
    "media_remote_video",
    "media_remote_embed",
];

#[plugin_tap]
pub fn tap_item_info() -> Vec<ContentTypeDefinition> {
    vec![
        ContentTypeDefinition {
            machine_name: "media_file".into(),
            label: "File".into(),
            description: "An uploaded file: a document, archive or any other allowed type".into(),
            title_label: Some("Name".into()),
            fields: vec![
                FieldDefinition::new("field_file", FieldType::File)
                    .required()
                    .label("File"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "media_image".into(),
            label: "Image".into(),
            description: "An uploaded image, with alt text, caption and credit".into(),
            title_label: Some("Name".into()),
            fields: vec![
                FieldDefinition::new("field_file", FieldType::File)
                    .required()
                    .label("Image"),
                FieldDefinition::new("field_alt_text", FieldType::Text { max_length: None })
                    .label("Alt text"),
                FieldDefinition::new("field_caption", FieldType::TextLong).label("Caption"),
                FieldDefinition::new("field_credit", FieldType::Text { max_length: None })
                    .label("Credit"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "media_remote_video".into(),
            label: "Remote video".into(),
            description: "A video hosted elsewhere, named by its URL".into(),
            title_label: Some("Name".into()),
            fields: vec![
                FieldDefinition::new("field_remote_url", FieldType::Text { max_length: None })
                    .required()
                    .label("Video URL"),
                FieldDefinition::new("field_caption", FieldType::TextLong).label("Caption"),
                // A remote video has no file the kernel can derive a thumbnail
                // from; the media library picker (`RecordReference("media")`)
                // lets the editor choose one of their own images as its card
                // thumbnail instead of showing nothing.
                FieldDefinition::new(
                    "field_thumbnail",
                    FieldType::RecordReference("media".into()),
                )
                .label("Thumbnail"),
            ],
        },
        ContentTypeDefinition {
            machine_name: "media_remote_embed".into(),
            label: "Remote embed".into(),
            description: "A page hosted elsewhere, embedded by its URL".into(),
            title_label: Some("Name".into()),
            fields: vec![
                FieldDefinition::new("field_remote_url", FieldType::Text { max_length: None })
                    .required()
                    .label("Embed URL"),
                FieldDefinition::new("field_caption", FieldType::TextLong).label("Caption"),
                FieldDefinition::new(
                    "field_thumbnail",
                    FieldType::RecordReference("media".into()),
                )
                .label("Thumbnail"),
            ],
        },
    ]
}

#[plugin_tap]
pub fn tap_media_source() -> Vec<MediaSourceDefinition> {
    vec![
        MediaSourceDefinition::new("media_file", "File", "An uploaded file"),
        MediaSourceDefinition::new("media_image", "Image", "An uploaded image"),
        MediaSourceDefinition::new(
            "media_remote_video",
            "Remote video",
            "A video hosted elsewhere",
        ),
        MediaSourceDefinition::new(
            "media_remote_embed",
            "Remote embed",
            "A page hosted elsewhere, embedded",
        ),
    ]
}

#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    vec![
        PermissionDefinition::new("access media overview", "View the media library listing"),
        PermissionDefinition::new("create media", "Add new media of any type"),
        PermissionDefinition::new("edit own media", "Edit media this user added"),
        PermissionDefinition::new("edit any media", "Edit any media item"),
        PermissionDefinition::new("delete own media", "Delete media this user added"),
        PermissionDefinition::new("delete any media", "Delete any media item"),
    ]
}

#[plugin_tap]
pub fn tap_item_access(input: ItemAccessInput) -> AccessResult {
    if !MEDIA_TYPES.contains(&input.item_type.as_str()) {
        return AccessResult::Neutral;
    }
    let holds = |perm: &str| input.user_permissions.iter().any(|p| p == perm);
    let is_own = input.user_id == input.author_id;
    match input.operation.as_str() {
        "edit" if holds("edit any media") || (is_own && holds("edit own media")) => {
            AccessResult::Grant
        }
        "delete" if holds("delete any media") || (is_own && holds("delete own media")) => {
            AccessResult::Grant
        }
        _ => AccessResult::Neutral,
    }
}

#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![MenuDefinition::new("/media", "Media").permission("access media overview")]
}

#[plugin_tap]
pub fn tap_admin_nav() -> Vec<AdminNavDefinition> {
    vec![
        AdminNavDefinition::new(
            "Content",
            "Media",
            "/admin/content/media",
            "access media overview",
        )
        .with_keywords(["media library", "images", "files", "upload"]),
    ]
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn item_info_declares_four_media_types() {
        let types = __inner_tap_item_info();
        assert_eq!(types.len(), 4);
        let names: Vec<&str> = types.iter().map(|t| t.machine_name.as_str()).collect();
        assert_eq!(names, MEDIA_TYPES);
    }

    #[test]
    fn media_file_and_image_require_the_file_field() {
        let types = __inner_tap_item_info();
        for name in ["media_file", "media_image"] {
            let t = types.iter().find(|t| t.machine_name == name).unwrap();
            let field = t
                .fields
                .iter()
                .find(|f| f.field_name == "field_file")
                .unwrap();
            assert!(field.required, "{name}'s field_file should be required");
        }
    }

    #[test]
    fn remote_types_require_a_url_not_a_file() {
        let types = __inner_tap_item_info();
        for name in ["media_remote_video", "media_remote_embed"] {
            let t = types.iter().find(|t| t.machine_name == name).unwrap();
            assert!(t.fields.iter().all(|f| f.field_name != "field_file"));
            let url_field = t
                .fields
                .iter()
                .find(|f| f.field_name == "field_remote_url")
                .unwrap();
            assert!(url_field.required);
        }
    }

    #[test]
    fn tap_media_source_registers_every_declared_type() {
        let sources = __inner_tap_media_source();
        assert_eq!(sources.len(), 4);
        let names: Vec<&str> = sources.iter().map(|s| s.content_type.as_str()).collect();
        assert_eq!(names, MEDIA_TYPES);
    }

    #[test]
    fn perm_returns_six_flat_permissions() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 6);
        let names: Vec<&str> = perms.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "access media overview",
                "create media",
                "edit own media",
                "edit any media",
                "delete own media",
                "delete any media",
            ]
        );
    }

    fn access_input(
        item_type: &str,
        operation: &str,
        author_id: Uuid,
        user_id: Uuid,
        permissions: &[&str],
    ) -> ItemAccessInput {
        ItemAccessInput {
            item_id: Uuid::nil(),
            item_type: item_type.into(),
            author_id,
            operation: operation.into(),
            user_id,
            user_authenticated: true,
            user_permissions: permissions.iter().map(|s| s.to_string()).collect(),
            stage_id: None,
            stage_machine_name: None,
        }
    }

    #[test]
    fn edit_any_media_grants_for_any_author() {
        let input = access_input(
            "media_image",
            "edit",
            Uuid::now_v7(),
            Uuid::now_v7(),
            &["edit any media"],
        );
        assert_eq!(__inner_tap_item_access(input), AccessResult::Grant);
    }

    #[test]
    fn edit_own_media_grants_only_for_the_author() {
        let author = Uuid::now_v7();
        let owned = access_input("media_file", "edit", author, author, &["edit own media"]);
        assert_eq!(__inner_tap_item_access(owned), AccessResult::Grant);

        let not_owned = access_input(
            "media_file",
            "edit",
            author,
            Uuid::now_v7(),
            &["edit own media"],
        );
        assert_eq!(__inner_tap_item_access(not_owned), AccessResult::Neutral);
    }

    #[test]
    fn delete_any_and_delete_own_follow_the_same_rule() {
        let author = Uuid::now_v7();
        let any = access_input(
            "media_remote_video",
            "delete",
            author,
            Uuid::now_v7(),
            &["delete any media"],
        );
        assert_eq!(__inner_tap_item_access(any), AccessResult::Grant);

        let own = access_input(
            "media_remote_embed",
            "delete",
            author,
            author,
            &["delete own media"],
        );
        assert_eq!(__inner_tap_item_access(own), AccessResult::Grant);
    }

    #[test]
    fn neutral_without_the_right_permission_or_for_another_type() {
        let author = Uuid::now_v7();
        let no_perm = access_input("media_file", "edit", author, author, &[]);
        assert_eq!(__inner_tap_item_access(no_perm), AccessResult::Neutral);

        let other_type = access_input(
            "blog",
            "edit",
            author,
            author,
            &["edit own media", "edit any media"],
        );
        assert_eq!(__inner_tap_item_access(other_type), AccessResult::Neutral);
    }

    #[test]
    fn menu_points_media_at_the_library_with_the_right_permission() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].path, "/media");
        assert_eq!(menus[0].permission, "access media overview");
    }

    #[test]
    fn admin_nav_links_to_the_library_under_the_right_permission() {
        let links = __inner_tap_admin_nav();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].href, "/admin/content/media");
        assert_eq!(links[0].permission, "access media overview");
    }
}
