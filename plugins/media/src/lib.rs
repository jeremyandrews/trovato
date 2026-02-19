//! Media plugin for Trovato.
//!
//! Provides a "media" content type wrapping file_managed with
//! alt text, caption, and credit fields.

use trovato_sdk::prelude::*;

#[plugin_tap]
pub fn tap_item_info() -> Vec<ContentTypeDefinition> {
    vec![ContentTypeDefinition {
        machine_name: "media".into(),
        label: "Media".into(),
        description: "A media entity with file, alt text, caption, and credit".into(),
        fields: vec![
            FieldDefinition::new("field_file", FieldType::File)
                .required()
                .label("File"),
            FieldDefinition::new("field_alt_text", FieldType::Text { max_length: None })
                .label("Alt Text"),
            FieldDefinition::new("field_caption", FieldType::TextLong).label("Caption"),
            FieldDefinition::new("field_credit", FieldType::Text { max_length: None })
                .label("Credit"),
        ],
    }]
}

#[plugin_tap]
pub fn tap_perm() -> Vec<PermissionDefinition> {
    PermissionDefinition::crud_for_type("media")
}

#[plugin_tap]
pub fn tap_item_access(input: ItemAccessInput) -> AccessResult {
    if input.item_type != "media" {
        return AccessResult::Neutral;
    }
    if input.user_id == input.author_id {
        return AccessResult::Grant;
    }
    AccessResult::Neutral
}

#[plugin_tap]
pub fn tap_menu() -> Vec<MenuDefinition> {
    vec![
        MenuDefinition::new("/media", "Media")
            .callback("media_browser")
            .permission("access content"),
        MenuDefinition::new("/admin/media", "Media")
            .callback("media_admin")
            .permission("view media content")
            .parent("/admin"),
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn item_info_returns_one_type() {
        let types = __inner_tap_item_info();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].machine_name, "media");
    }

    #[test]
    fn perm_returns_four_permissions() {
        let perms = __inner_tap_perm();
        assert_eq!(perms.len(), 4);
    }

    #[test]
    fn fields_has_file_required() {
        let types = __inner_tap_item_info();
        let file_field = types[0]
            .fields
            .iter()
            .find(|f| f.field_name == "field_file")
            .unwrap();
        assert!(file_field.required);
    }

    #[test]
    fn access_grant_for_author() {
        let author = Uuid::nil();
        let input = ItemAccessInput {
            item_id: Uuid::nil(),
            item_type: "media".into(),
            author_id: author,
            operation: "edit".into(),
            user_id: author,
        };
        assert_eq!(__inner_tap_item_access(input), AccessResult::Grant);
    }

    #[test]
    fn access_neutral_for_non_media() {
        let input = ItemAccessInput {
            item_id: Uuid::nil(),
            item_type: "blog".into(),
            author_id: Uuid::nil(),
            operation: "edit".into(),
            user_id: Uuid::nil(),
        };
        assert_eq!(__inner_tap_item_access(input), AccessResult::Neutral);
    }

    #[test]
    fn menu_returns_two_routes() {
        let menus = __inner_tap_menu();
        assert_eq!(menus.len(), 2);
        assert_eq!(menus[0].path, "/media");
    }
}
