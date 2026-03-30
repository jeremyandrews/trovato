//! Language detection and translation workflow plugin for Ritrovo conferences.
//!
//! Provides:
//! - Language detection on conference insert (heuristic for Italian)
//! - Language badge and switcher on conference view
//! - Translation status tracking via item `data` JSONB

use trovato_sdk::prelude::*;

/// Common Italian articles and prepositions used for language detection.
const ITALIAN_MARKERS: &[&str] = &[
    " il ", " la ", " le ", " lo ", " gli ", " un ", " una ", " del ", " della ", " delle ",
    " dei ", " degli ", " nel ", " nella ", " nelle ", " nei ", " negli ", " al ", " alla ",
    " alle ", " ai ", " agli ", " di ", " da ", " in ", " con ", " su ", " per ", " che ", " non ",
    " sono ", " anche ", " come ", " più ", " questo ", " questa ", " questi ", " queste ",
    " stato ", " essere ", " hanno ", " molto ",
];

/// Detect whether text is likely Italian based on common word markers.
///
/// Returns `true` if the text contains a threshold number of Italian
/// marker words. Uses a simple heuristic — not a full language detector.
fn is_likely_italian(text: &str) -> bool {
    let lower = text.to_lowercase();
    let padded = format!(" {lower} ");
    let matches = ITALIAN_MARKERS
        .iter()
        .filter(|m| padded.contains(**m))
        .count();
    // Require at least 3 marker matches for short text, or proportional for longer
    matches >= 3
}

/// Detect language on conference insert and set translation_status.
///
/// When a new conference is created, this tap checks the title and
/// description fields for Italian language markers. If detected, it
/// sets `translation_status: "needs_translation"` in the item's
/// render output metadata.
#[plugin_tap]
pub fn tap_item_insert(item: Item) -> String {
    if item.item_type != "conference" {
        return String::new();
    }

    let title = &item.title;
    let description = item
        .fields
        .get("field_description")
        .and_then(|v| match v {
            serde_json::Value::String(s) => Some(s.as_str()),
            serde_json::Value::Object(obj) => obj.get("value").and_then(|v| v.as_str()),
            _ => None,
        })
        .unwrap_or("");

    let combined = format!("{title} {description}");

    let detected_language = if is_likely_italian(&combined) {
        "it"
    } else {
        "en"
    };

    // Return detection result as JSON for the kernel to process
    serde_json::json!({
        "detected_language": detected_language,
        "translation_status": if detected_language != "en" { "needs_translation" } else { "translated" }
    })
    .to_string()
}

/// Render language badge and translation switcher on conference view.
///
/// Shows a badge indicating the item's source language and provides
/// links to view the item in other available languages.
#[plugin_tap]
pub fn tap_item_view(item: Item) -> String {
    if item.item_type != "conference" {
        return String::new();
    }

    // Check if the title appears to be Italian
    let is_italian = is_likely_italian(&item.title);

    let lang_code = if is_italian { "it" } else { "en" };
    let lang_name = if is_italian { "Italiano" } else { "English" };

    let switcher = if is_italian {
        format!(
            r#"<a href="/conferences/{}" class="lang-switcher__link">View in English</a>"#,
            item.id
        )
    } else {
        format!(
            r#"<a href="/it/conferences/{}" class="lang-switcher__link">Vedi in italiano</a>"#,
            item.id
        )
    };

    format!(
        r#"<div class="lang-badge-switcher">
            <span class="lang-badge" lang="{lang_code}">{lang_name}</span>
            {switcher}
        </div>"#
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn detect_italian_text() {
        assert!(is_likely_italian(
            "La conferenza sulla intelligenza artificiale nel mondo della ricerca"
        ));
    }

    #[test]
    fn detect_english_text() {
        assert!(!is_likely_italian(
            "The conference on artificial intelligence in the world of research"
        ));
    }

    #[test]
    fn detect_short_italian() {
        // Short text with fewer markers may not trigger
        assert!(!is_likely_italian("Ciao"));
    }

    #[test]
    fn insert_detects_italian() {
        let mut fields = HashMap::new();
        fields.insert(
            "field_description".to_string(),
            serde_json::Value::String(
                "La conferenza sulla intelligenza artificiale nel mondo della ricerca scientifica"
                    .to_string(),
            ),
        );
        let item = Item {
            id: Uuid::nil(),
            item_type: "conference".to_string(),
            title: "Conferenza AI Italia".to_string(),
            fields,
            status: 1,
            author_id: Uuid::nil(),
            current_revision_id: None,
            stage_id: live_stage_id(),
            created: 0,
            changed: 0,
            language: None,
        };
        let result = __inner_tap_item_insert(item);
        assert!(result.contains("\"detected_language\":\"it\""));
        assert!(result.contains("needs_translation"));
    }

    #[test]
    fn insert_detects_english() {
        let item = Item {
            id: Uuid::nil(),
            item_type: "conference".to_string(),
            title: "AI Conference 2026".to_string(),
            fields: HashMap::new(),
            status: 1,
            author_id: Uuid::nil(),
            current_revision_id: None,
            stage_id: live_stage_id(),
            created: 0,
            changed: 0,
            language: None,
        };
        let result = __inner_tap_item_insert(item);
        assert!(result.contains("\"detected_language\":\"en\""));
        assert!(result.contains("\"translated\""));
    }

    #[test]
    fn insert_ignores_non_conference() {
        let item = Item {
            id: Uuid::nil(),
            item_type: "blog".to_string(),
            title: "Test".to_string(),
            fields: HashMap::new(),
            status: 1,
            author_id: Uuid::nil(),
            current_revision_id: None,
            stage_id: live_stage_id(),
            created: 0,
            changed: 0,
            language: None,
        };
        assert!(__inner_tap_item_insert(item).is_empty());
    }

    #[test]
    fn view_shows_language_badge() {
        let item = Item {
            id: Uuid::nil(),
            item_type: "conference".to_string(),
            title: "AI Conference 2026".to_string(),
            fields: HashMap::new(),
            status: 1,
            author_id: Uuid::nil(),
            current_revision_id: None,
            stage_id: live_stage_id(),
            created: 0,
            changed: 0,
            language: None,
        };
        let result = __inner_tap_item_view(item);
        assert!(result.contains("lang-badge"));
        assert!(result.contains("English"));
    }

    #[test]
    fn view_empty_for_non_conference() {
        let item = Item {
            id: Uuid::nil(),
            item_type: "blog".to_string(),
            title: "Test".to_string(),
            fields: HashMap::new(),
            status: 1,
            author_id: Uuid::nil(),
            current_revision_id: None,
            stage_id: live_stage_id(),
            created: 0,
            changed: 0,
            language: None,
        };
        assert!(__inner_tap_item_view(item).is_empty());
    }
}
