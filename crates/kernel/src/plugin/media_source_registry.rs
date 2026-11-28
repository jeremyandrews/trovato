//! Registry of the media sources plugins declare from `tap_media_source`.
//!
//! Built at boot from the tap's JSON output, exactly like
//! [`crate::plugin::admin_nav_registry::AdminNavRegistry`] and for the same
//! reason: a declaration is a constant, so it is collected once here rather
//! than asked for during a request.
//!
//! A media source names a content type, already declared from `tap_item_info`,
//! that the media library (`/admin/content/media`) lists and the media picker
//! widget (a `RecordReference("media")` field) offers. See
//! [`trovato_sdk::types::MediaSourceDefinition`] for the whole contract.

use std::collections::HashSet;

use tracing::warn;
use trovato_sdk::types::MediaSourceDefinition;

/// Longest content type name, label or description accepted.
const MAX_TEXT_LEN: usize = 255;

/// One media source a plugin declared, with the plugin that declared it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PluginMediaSource {
    pub plugin: String,
    pub content_type: String,
    pub label: String,
    pub description: String,
}

/// Why one declaration was dropped.
#[derive(Debug, Clone)]
pub struct MediaSourceRejection {
    pub plugin: String,
    pub content_type: String,
    pub reason: String,
}

/// Every media source declared by the plugins that answered `tap_media_source`.
#[derive(Debug, Default)]
pub struct MediaSourceRegistry {
    sources: Vec<PluginMediaSource>,
    rejections: Vec<MediaSourceRejection>,
}

impl MediaSourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse the dispatch results, dropping what cannot be used.
    ///
    /// A rejection is never fatal: one plugin's malformed declaration must
    /// not stop a site booting, so it is recorded, logged once, and skipped.
    pub fn from_tap_results(results: Vec<(String, String)>) -> Self {
        let mut registry = Self::new();
        let mut seen_types: HashSet<String> = HashSet::new();

        for (plugin, json) in results {
            // `#[plugin_tap]` serializes the return value, so an array arrives
            // as a JSON array — but a String-returning tap arrives as a JSON
            // string wrapping one, the same double encoding the view and api
            // paths already accept (G-VIEW-OUTPUT-JSON-ENCODED).
            let parsed = serde_json::from_str::<serde_json::Value>(&json)
                .map(|value| match value {
                    serde_json::Value::String(ref inner) => {
                        serde_json::from_str::<serde_json::Value>(inner).unwrap_or(value)
                    }
                    other => other,
                })
                .and_then(serde_json::from_value::<Vec<MediaSourceDefinition>>);

            let definitions = match parsed {
                Ok(definitions) => definitions,
                Err(e) => {
                    registry.reject(&plugin, "<unparsed>", format!("output did not parse: {e}"));
                    continue;
                }
            };

            for def in definitions {
                if let Err(reason) = validate(&def, &seen_types) {
                    registry.reject(&plugin, &def.content_type, reason);
                    continue;
                }
                seen_types.insert(def.content_type.clone());
                registry.sources.push(PluginMediaSource {
                    plugin: plugin.clone(),
                    content_type: def.content_type,
                    label: def.label,
                    description: def.description,
                });
            }
        }

        for rejection in &registry.rejections {
            warn!(
                plugin = %rejection.plugin,
                content_type = %rejection.content_type,
                reason = %rejection.reason,
                "dropping an invalid plugin media source declaration"
            );
        }

        registry
    }

    fn reject(&mut self, plugin: &str, content_type: &str, reason: String) {
        self.rejections.push(MediaSourceRejection {
            plugin: plugin.to_string(),
            content_type: content_type.to_string(),
            reason,
        });
    }

    /// Every media source a plugin declared.
    pub fn sources(&self) -> &[PluginMediaSource] {
        &self.sources
    }

    pub fn rejections(&self) -> &[MediaSourceRejection] {
        &self.rejections
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// Whether `content_type` names a registered media source.
    pub fn is_media_type(&self, content_type: &str) -> bool {
        self.sources.iter().any(|s| s.content_type == content_type)
    }

    /// Every registered content type name, in declaration order.
    pub fn content_types(&self) -> impl Iterator<Item = &str> {
        self.sources.iter().map(|s| s.content_type.as_str())
    }

    /// The source declaration for `content_type`, if one is registered.
    pub fn get(&self, content_type: &str) -> Option<&PluginMediaSource> {
        self.sources.iter().find(|s| s.content_type == content_type)
    }
}

/// Reject a declaration this registry cannot use safely: an empty field, a
/// content type already claimed by another plugin, or a field longer than
/// [`MAX_TEXT_LEN`].
fn validate(def: &MediaSourceDefinition, seen_types: &HashSet<String>) -> Result<(), String> {
    if def.content_type.trim().is_empty() {
        return Err("content_type is empty".to_string());
    }
    if def.label.trim().is_empty() {
        return Err("label is empty".to_string());
    }
    for (field, value) in [
        ("content_type", &def.content_type),
        ("label", &def.label),
        ("description", &def.description),
    ] {
        if value.len() > MAX_TEXT_LEN {
            return Err(format!("{field} is longer than {MAX_TEXT_LEN} bytes"));
        }
    }
    if seen_types.contains(&def.content_type) {
        return Err("another plugin already declared this content type as a media source".into());
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn def(content_type: &str) -> MediaSourceDefinition {
        MediaSourceDefinition::new(content_type, "Label", "Description")
    }

    #[test]
    fn a_well_formed_declaration_is_kept() {
        let registry = MediaSourceRegistry::from_tap_results(vec![(
            "trovato_media".to_string(),
            serde_json::to_string(&vec![def("media_file")]).unwrap(),
        )]);
        assert_eq!(registry.sources().len(), 1);
        assert_eq!(registry.sources()[0].plugin, "trovato_media");
        assert!(registry.is_media_type("media_file"));
        assert!(!registry.is_media_type("media_other"));
        assert!(registry.rejections().is_empty());
    }

    #[test]
    fn an_empty_content_type_is_rejected() {
        let registry = MediaSourceRegistry::from_tap_results(vec![(
            "trovato_media".to_string(),
            serde_json::to_string(&vec![def("")]).unwrap(),
        )]);
        assert!(registry.sources().is_empty());
        assert_eq!(registry.rejections().len(), 1);
    }

    #[test]
    fn an_empty_label_is_rejected() {
        let mut d = def("media_file");
        d.label = String::new();
        let registry = MediaSourceRegistry::from_tap_results(vec![(
            "trovato_media".to_string(),
            serde_json::to_string(&vec![d]).unwrap(),
        )]);
        assert!(registry.sources().is_empty());
        assert_eq!(registry.rejections().len(), 1);
    }

    #[test]
    fn two_plugins_cannot_claim_the_same_content_type() {
        let registry = MediaSourceRegistry::from_tap_results(vec![
            (
                "trovato_media".to_string(),
                serde_json::to_string(&vec![def("media_file")]).unwrap(),
            ),
            (
                "other".to_string(),
                serde_json::to_string(&vec![def("media_file")]).unwrap(),
            ),
        ]);
        assert_eq!(registry.sources().len(), 1);
        assert_eq!(registry.sources()[0].plugin, "trovato_media");
        assert_eq!(registry.rejections().len(), 1);
    }

    #[test]
    fn multiple_types_from_one_plugin_are_all_kept() {
        let registry = MediaSourceRegistry::from_tap_results(vec![(
            "trovato_media".to_string(),
            serde_json::to_string(&vec![
                def("media_file"),
                def("media_image"),
                def("media_remote_video"),
                def("media_remote_embed"),
            ])
            .unwrap(),
        )]);
        assert_eq!(registry.sources().len(), 4);
        assert_eq!(registry.content_types().collect::<Vec<_>>().len(), 4);
    }

    #[test]
    fn malformed_json_is_rejected_without_panicking() {
        let registry = MediaSourceRegistry::from_tap_results(vec![(
            "trovato_media".to_string(),
            "not json".to_string(),
        )]);
        assert!(registry.sources().is_empty());
        assert_eq!(registry.rejections().len(), 1);
    }

    #[test]
    fn get_returns_the_matching_source() {
        let registry = MediaSourceRegistry::from_tap_results(vec![(
            "trovato_media".to_string(),
            serde_json::to_string(&vec![def("media_file")]).unwrap(),
        )]);
        assert_eq!(registry.get("media_file").unwrap().label, "Label");
        assert!(registry.get("media_other").is_none());
    }
}
