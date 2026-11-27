//! Page builder component registry.
//!
//! The registry is every component a page may use: the kernel's standard set
//! and every enabled plugin's, each declared in a `<name>.component.toml` file
//! ([`schema`]). It is built once at startup, from the plugins enabled then,
//! which is when every other plugin asset is picked up: enabling or disabling
//! a plugin takes effect on the next restart. It is used by:
//!
//! - the `/api/v1/page-builder/components` endpoint, which the editor reads on
//!   init to build its component list and field forms;
//! - server side validation of a saved page ([`ComponentRegistry::validate_page`]),
//!   on every write `ItemService` makes;
//! - the renderer, which finds each component's template, declared fields,
//!   variant classes and stylesheets here.
//!
//! Two declarations of one type name are refused when the registry is built,
//! naming both sources: a plugin cannot silently replace a standard component
//! or another plugin's.
//!
//! The registry also holds the page root fields: page level values (beyond
//! the built in `title`) a plugin declares in `components/<name>.root.toml`,
//! with the same field types, limits and checks as component fields. The
//! editor builds its page settings form from them, a save is checked against
//! them, and page, region and tile templates receive the validated values as
//! `page_root` (see `page_builder::page_root_values`). Two plugins declaring
//! one root field name are refused the same way as a duplicate component.

pub mod schema;
mod validate;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub use schema::{
    ComponentDefinition, ComponentSource, FieldDefinition, FieldKind, RESERVED_ROOT_FIELD_NAMES,
    ReferenceTarget, RootFieldDefinition, SelectOption, to_kebab_case,
};
pub use validate::{FieldError, image_prop_without_alt, is_gather_id, is_safe_url};

/// Deepest nesting of zones a page may have.
pub const MAX_ZONE_DEPTH: usize = 10;

/// The standard components, in the order the editor offers them.
const STANDARD: &[(&str, &str)] = &[
    (
        "columns.component.toml",
        include_str!("page_builder_components/standard/columns.component.toml"),
    ),
    (
        "section-wrapper.component.toml",
        include_str!("page_builder_components/standard/section-wrapper.component.toml"),
    ),
    (
        "hero.component.toml",
        include_str!("page_builder_components/standard/hero.component.toml"),
    ),
    (
        "text-block.component.toml",
        include_str!("page_builder_components/standard/text-block.component.toml"),
    ),
    (
        "cta.component.toml",
        include_str!("page_builder_components/standard/cta.component.toml"),
    ),
    (
        "content-feature.component.toml",
        include_str!("page_builder_components/standard/content-feature.component.toml"),
    ),
    (
        "blockquote-extended.component.toml",
        include_str!("page_builder_components/standard/blockquote-extended.component.toml"),
    ),
    (
        "you-tube-embed.component.toml",
        include_str!("page_builder_components/standard/you-tube-embed.component.toml"),
    ),
    (
        "logo-row.component.toml",
        include_str!("page_builder_components/standard/logo-row.component.toml"),
    ),
    (
        "card-grid.component.toml",
        include_str!("page_builder_components/standard/card-grid.component.toml"),
    ),
    (
        "accordion.component.toml",
        include_str!("page_builder_components/standard/accordion.component.toml"),
    ),
    (
        "summary-box.component.toml",
        include_str!("page_builder_components/standard/summary-box.component.toml"),
    ),
    (
        "table-of-contents.component.toml",
        include_str!("page_builder_components/standard/table-of-contents.component.toml"),
    ),
];

/// An enabled plugin the registry reads component files from.
#[derive(Debug, Clone)]
pub struct PluginComponentSource {
    /// The plugin's name.
    pub name: String,
    /// The plugin's directory: components are in `components/`, stylesheets
    /// under `static/`.
    pub dir: PathBuf,
}

/// Registry of available page builder components.
#[derive(Debug, Clone)]
pub struct ComponentRegistry {
    /// In registration order: standard, then plugins by name, files by name.
    components: Vec<ComponentDefinition>,
    /// Type name to (index, file it was read from).
    index: HashMap<String, (usize, String)>,
    /// The declared page root fields, in load order: plugins by name, files
    /// by name, fields in file order.
    root_fields: Vec<RootFieldDefinition>,
    /// The same fields without their source, for the shared field checks.
    root_field_defs: Vec<FieldDefinition>,
    /// Root field name to the file it was read from.
    root_files: HashMap<String, String>,
    /// Every class a root select option may add to `page_root.variant_classes`.
    root_classes: Vec<String>,
}

impl ComponentRegistry {
    /// A registry holding the standard components only.
    ///
    /// # Panics
    ///
    /// If a standard component file is invalid. They are compiled into the
    /// kernel and parsed by `the_standard_components_load` on every test run,
    /// so a failure here is a build defect, not a runtime condition.
    pub fn new() -> Self {
        // Infallible at runtime: see `# Panics`.
        #[allow(clippy::expect_used)]
        Self::standard().expect("the standard page builder components are valid")
    }

    fn standard() -> Result<Self> {
        let mut registry = Self {
            components: Vec::new(),
            index: HashMap::new(),
            root_fields: Vec::new(),
            root_field_defs: Vec::new(),
            root_files: HashMap::new(),
            root_classes: Vec::new(),
        };
        for (file, text) in STANDARD {
            let def =
                schema::parse_component(text, ComponentSource::Kernel, "/static/", &|_| false)
                    .with_context(|| format!("standard component {file}"))?;
            registry.insert(def, format!("kernel {file}"))?;
        }
        Ok(registry)
    }

    /// The standard components plus every component file each of `plugins`
    /// ships under `components/`.
    ///
    /// Plugins are read in name order and their files in file name order, so
    /// the editor's list is stable. Any invalid file, a stylesheet that is not
    /// there, or a type name declared twice is an error naming the file (and,
    /// for a duplicate, both declarations): the server does not start with a
    /// component it would render differently from what its author wrote.
    pub fn load(plugins: &[PluginComponentSource]) -> Result<Self> {
        let mut registry = Self::standard()?;
        let mut plugins: Vec<&PluginComponentSource> = plugins.iter().collect();
        plugins.sort_by(|a, b| a.name.cmp(&b.name));
        for plugin in plugins {
            for path in component_files(&plugin.dir)? {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                let static_dir = plugin.dir.join("static");
                let def = schema::parse_component(
                    &text,
                    ComponentSource::Plugin(plugin.name.clone()),
                    &format!("/static/plugins/{}/", plugin.name),
                    &|sheet| static_dir.join(sheet).is_file(),
                )
                .with_context(|| {
                    format!(
                        "plugin '{}': page builder component {}",
                        plugin.name,
                        path.display()
                    )
                })?;
                registry.insert(def, path.display().to_string())?;
            }
            for path in plugin_files(&plugin.dir, ".root.toml")? {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                let declaration =
                    schema::parse_root(&text, ComponentSource::Plugin(plugin.name.clone()))
                        .with_context(|| {
                            format!(
                                "plugin '{}': page root file {}",
                                plugin.name,
                                path.display()
                            )
                        })?;
                registry.insert_root(declaration, path.display().to_string())?;
            }
        }
        Ok(registry)
    }

    fn insert_root(&mut self, declaration: schema::RootDeclaration, file: String) -> Result<()> {
        for root_field in declaration.fields {
            let name = root_field.field.name.clone();
            if let Some(existing_file) = self.root_files.get(&name) {
                let existing = self
                    .root_fields
                    .iter()
                    .find(|f| f.field.name == name)
                    .map(|f| f.source.to_string())
                    .unwrap_or_default();
                bail!(
                    "page root field '{name}' is declared twice: by {existing} ({existing_file}) \
                     and by {} ({file}). Rename one of them, or disable one of the plugins.",
                    root_field.source,
                );
            }
            // A default the field's own rules refuse would be put in every
            // page that leaves the field alone, so it is an error at load.
            if let Some(default) = &root_field.field.default {
                let mut errors = Vec::new();
                let fields = std::slice::from_ref(&root_field.field);
                validate::Checker {
                    location: "root",
                    component: "",
                    errors: &mut errors,
                    allow_h1: false,
                }
                .check_props(
                    fields,
                    &serde_json::json!({ name.clone(): default }),
                    "",
                );
                if let Some(error) = errors.first() {
                    bail!(
                        "{} ({file}): page root field '{name}' has a default its own rules \
                         refuse: {}",
                        root_field.source,
                        error.message
                    );
                }
            }
            self.root_files.insert(name, file.clone());
            self.root_field_defs.push(root_field.field.clone());
            self.root_fields.push(root_field);
        }
        for class in declaration.classes {
            if !self.root_classes.contains(&class) {
                self.root_classes.push(class);
            }
        }
        Ok(())
    }

    /// The declared page root fields, in load order, each with its source.
    pub fn root_fields(&self) -> &[RootFieldDefinition] {
        &self.root_fields
    }

    /// The declared page root fields' definitions, in load order.
    pub fn root_field_definitions(&self) -> &[FieldDefinition] {
        &self.root_field_defs
    }

    /// Every class a page root select option may add.
    pub fn root_classes(&self) -> &[String] {
        &self.root_classes
    }

    fn insert(&mut self, def: ComponentDefinition, file: String) -> Result<()> {
        if let Some((existing, existing_file)) = self.index.get(&def.type_name) {
            let existing = &self.components[*existing];
            bail!(
                "page builder component type '{}' is declared twice: by {} ({existing_file}) \
                 and by {} ({file}). Rename one of them, or disable one of the plugins.",
                def.type_name,
                existing.source,
                def.source,
            );
        }
        self.index
            .insert(def.type_name.clone(), (self.components.len(), file));
        self.components.push(def);
        Ok(())
    }

    /// Get a component definition by type name.
    pub fn get(&self, type_name: &str) -> Option<&ComponentDefinition> {
        self.index.get(type_name).map(|(i, _)| &self.components[*i])
    }

    /// All component definitions, in registration order: the standard set in
    /// the editor's order, then each plugin's.
    pub fn all(&self) -> Vec<&ComponentDefinition> {
        self.components.iter().collect()
    }

    /// The number of registered components.
    pub fn len(&self) -> usize {
        self.components.len()
    }

    /// Whether the registry is empty (it never is: the standard set is always
    /// registered).
    pub fn is_empty(&self) -> bool {
        self.components.is_empty()
    }

    /// Refuse to start with a component whose template no directory on the
    /// template search path supplies.
    pub fn check_templates(&self, has_template: impl Fn(&str) -> bool) -> Result<()> {
        for def in &self.components {
            if !has_template(&def.template) {
                bail!(
                    "page builder component '{}' from {} needs the template '{}', and no \
                     directory on the template search path (the enabled plugins' templates/ \
                     and TEMPLATES_DIR) has it",
                    def.type_name,
                    def.source,
                    def.template
                );
            }
        }
        Ok(())
    }

    /// Validate a Puck page JSON against the registry.
    ///
    /// Returns every error (empty = valid): unknown component types, values
    /// outside their field's type or limits, an image without alt text, a
    /// heading level of 1, zones nested deeper than [`MAX_ZONE_DEPTH`], and
    /// in the page root ([`Self::validate_root`]) a value outside its root
    /// field's rules or a key no enabled plugin declares.
    pub fn validate_page(&self, page: &serde_json::Value) -> Vec<FieldError> {
        let mut errors = Vec::new();
        if let Some(root) = page.get("root") {
            self.validate_root(root, &mut errors);
        }
        let Some(content) = page.get("content") else {
            errors.push(page_error("page data needs a content list"));
            return errors;
        };
        let Some(content) = content.as_array() else {
            errors.push(page_error("content must be a list of components"));
            return errors;
        };
        let mut h1_seen = false;
        for (i, component) in content.iter().enumerate() {
            self.validate_component(
                &format!("content[{i}]"),
                component,
                0,
                &mut errors,
                &mut h1_seen,
            );
        }
        errors
    }

    /// Check a page's `root` against the declared root fields.
    ///
    /// Both shapes the editor's Puck may write are read: values directly
    /// under `root`, as the stored pages have them, and under `root.props`,
    /// as current Puck versions keep them ([`root_values`]). `title` is the
    /// built in root field and must be text. Any other key no enabled plugin
    /// declares is refused, so a misspelt name or a value meant for a disabled
    /// plugin is not saved silently. Only a write checks this: a stored page
    /// is never validated to render, and an unknown key is simply not handed
    /// to templates.
    pub fn validate_root(&self, root: &serde_json::Value, errors: &mut Vec<FieldError>) {
        let push = |field: &str, message: String, errors: &mut Vec<FieldError>| {
            errors.push(FieldError {
                location: "root".to_string(),
                component: String::new(),
                field: field.to_string(),
                message,
            });
        };
        if root.is_null() {
            return;
        }
        if !root.is_object() {
            push("", "the page root must be an object".to_string(), errors);
            return;
        }
        if let Some(props) = root.get("props")
            && !props.is_object()
            && !props.is_null()
        {
            push("props", "must be an object".to_string(), errors);
            return;
        }
        let values = root_values(root);
        if let Some(title) = values.get("title")
            && !title.is_string()
            && !title.is_null()
        {
            push("title", "must be text".to_string(), errors);
        }
        for key in values.keys() {
            if key != "title" && !self.root_files.contains_key(key) {
                push(
                    key,
                    format!("'{key}' is not a page root field any enabled plugin declares"),
                    errors,
                );
            }
        }
        validate::Checker {
            location: "root",
            component: "",
            errors,
            allow_h1: false,
        }
        .check_props(
            &self.root_field_defs,
            &serde_json::Value::Object(values),
            "",
        );
    }

    fn validate_component(
        &self,
        location: &str,
        component: &serde_json::Value,
        depth: usize,
        errors: &mut Vec<FieldError>,
        h1_seen: &mut bool,
    ) {
        let type_name = component.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let push = |field: &str, message: String, errors: &mut Vec<FieldError>| {
            errors.push(FieldError {
                location: location.to_string(),
                component: type_name.to_string(),
                field: field.to_string(),
                message,
            });
        };
        if depth > MAX_ZONE_DEPTH {
            push(
                "",
                format!("zones are nested more than {MAX_ZONE_DEPTH} deep"),
                errors,
            );
            return;
        }
        let props = component.get("props").unwrap_or(&serde_json::Value::Null);
        if !props.is_null() && !props.is_object() {
            push("", "props must be an object".to_string(), errors);
            return;
        }

        match self.get(type_name) {
            None => push("", format!("unknown component type '{type_name}'"), errors),
            Some(def) => {
                validate::Checker {
                    location,
                    component: type_name,
                    errors,
                    allow_h1: def.may_be_page_heading,
                }
                .check_props(&def.fields, props, "");

                // The checks that predate field definitions, by prop name, for
                // the props a definition does not declare with a type of its own.
                let min_heading = if def.may_be_page_heading { 1 } else { 2 };
                if def.field("headingLevel").is_none()
                    && let Some(level) = props.get("headingLevel").and_then(|l| l.as_u64())
                    && level < min_heading
                {
                    push(
                        "headingLevel",
                        "must be a heading level from 2 to 6 (the page title is the level 1 heading)"
                            .to_string(),
                        errors,
                    );
                }
                // At most one component on the page may set headingLevel: 1
                // (and only one `may_be_page_heading` component may ever ask
                // to): the page's H1 is rendered once, or not at all.
                if def.may_be_page_heading
                    && props.get("headingLevel").and_then(|l| l.as_u64()) == Some(1)
                {
                    if *h1_seen {
                        push(
                            "headingLevel",
                            "only one component per page may render the page's H1 heading"
                                .to_string(),
                            errors,
                        );
                    } else {
                        *h1_seen = true;
                    }
                }
                if validate::image_prop_without_alt(props) {
                    push(
                        "imageAlt",
                        "an image needs alt text, or isDecorative must be true (WCAG 1.1.1)"
                            .to_string(),
                        errors,
                    );
                }
            }
        }

        // Recurse into zones
        if let Some(zones) = component.get("zones").and_then(|z| z.as_object()) {
            for (zone, children) in zones {
                if let Some(arr) = children.as_array() {
                    for (j, child) in arr.iter().enumerate() {
                        self.validate_component(
                            &format!("{location}.zones.{zone}[{j}]"),
                            child,
                            depth + 1,
                            errors,
                            h1_seen,
                        );
                    }
                }
            }
        }
    }
}

impl Default for ComponentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn page_error(message: &str) -> FieldError {
    FieldError {
        location: String::new(),
        component: String::new(),
        field: String::new(),
        message: message.to_string(),
    }
}

/// A page root's values, from either shape Puck writes: directly under
/// `root` (the stored shape, and older Puck versions'), and under
/// `root.props` (current Puck versions'). A key in both takes the value
/// under `props`. Anything that is not an object holds no values.
pub fn root_values(root: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let mut values = serde_json::Map::new();
    let Some(obj) = root.as_object() else {
        return values;
    };
    for (key, value) in obj {
        if key != "props" {
            values.insert(key.clone(), value.clone());
        }
    }
    if let Some(props) = obj.get("props").and_then(|p| p.as_object()) {
        for (key, value) in props {
            values.insert(key.clone(), value.clone());
        }
    }
    values
}

/// The `*.component.toml` files under `dir/components`, sorted by name. A
/// plugin without the directory has none.
fn component_files(dir: &Path) -> Result<Vec<PathBuf>> {
    plugin_files(dir, ".component.toml")
}

/// The files under `dir/components` whose names end in `suffix`, sorted by
/// name.
fn plugin_files(dir: &Path, suffix: &str) -> Result<Vec<PathBuf>> {
    let components = dir.join("components");
    if !components.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&components)
        .with_context(|| format!("failed to read {}", components.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(suffix))
        })
        .collect();
    files.sort();
    Ok(files)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_standard_components_load() {
        ComponentRegistry::standard().unwrap();
    }

    #[test]
    fn registry_has_all_12_components() {
        let registry = ComponentRegistry::new();
        assert_eq!(registry.len(), 13); // 12 + TableOfContents
        for name in [
            "Hero",
            "Columns",
            "TextBlock",
            "CardGrid",
            "Cta",
            "Accordion",
            "ContentFeature",
            "LogoRow",
            "SummaryBox",
            "SectionWrapper",
            "BlockquoteExtended",
            "YouTubeEmbed",
            "TableOfContents",
        ] {
            let def = registry.get(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(def.source, ComponentSource::Kernel);
            assert_eq!(def.template, format!("pb/{}.html", to_kebab_case(name)));
        }
    }

    #[test]
    fn the_standard_set_is_in_editor_order() {
        let registry = ComponentRegistry::new();
        let order: Vec<&str> = registry
            .all()
            .iter()
            .map(|d| d.type_name.as_str())
            .collect();
        assert_eq!(&order[..3], &["Columns", "SectionWrapper", "Hero"]);
        assert_eq!(order.last(), Some(&"TableOfContents"));
    }

    #[test]
    fn validate_page_catches_unknown_component() {
        let registry = ComponentRegistry::new();
        let page = json!({ "content": [ { "type": "NonexistentWidget", "props": {} } ] });
        let errors = registry.validate_page(&page);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].message.contains("NonexistentWidget"));
        assert_eq!(errors[0].location, "content[0]");
    }

    #[test]
    fn validate_page_catches_h1_heading() {
        // A component without `may_be_page_heading` keeps the old rule.
        let registry = ComponentRegistry::new();
        let page = json!({ "content": [ { "type": "Cta", "props": { "headingLevel": 1 } } ] });
        let errors = registry.validate_page(&page);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].field, "headingLevel");
    }

    #[test]
    fn hero_may_render_the_page_h1() {
        let registry = ComponentRegistry::new();
        let page = json!({ "content": [
            { "type": "Hero", "props": { "title": "T", "headingLevel": 1 } }
        ] });
        assert!(registry.validate_page(&page).is_empty());
    }

    #[test]
    fn a_second_h1_component_is_refused() {
        let registry = ComponentRegistry::new();
        let page = json!({ "content": [
            { "type": "Hero", "props": { "title": "A", "headingLevel": 1 } },
            { "type": "Hero", "props": { "title": "B", "headingLevel": 1 } }
        ] });
        let errors = registry.validate_page(&page);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].location, "content[1]");
        assert_eq!(errors[0].field, "headingLevel");
        assert!(errors[0].message.contains("only one component"));
    }

    #[test]
    fn a_second_h1_nested_in_a_zone_is_also_refused() {
        let registry = ComponentRegistry::new();
        let page = json!({ "content": [
            { "type": "Hero", "props": { "title": "A", "headingLevel": 1 } },
            { "type": "Columns", "props": { "layout": "1/2+1/2" }, "zones": {
                "zone-0": [ { "type": "Hero", "props": { "title": "B", "headingLevel": 1 } } ],
                "zone-1": []
            } }
        ] });
        let errors = registry.validate_page(&page);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].location, "content[1].zones.zone-0[0]");
    }

    #[test]
    fn a_nested_card_image_without_alt_is_refused() {
        let registry = ComponentRegistry::new();
        let page = json!({ "content": [
            { "type": "CardGrid", "props": { "cards": [
                { "title": "A", "image_url": "/img/a.jpg" }
            ] } }
        ] });
        let errors = registry.validate_page(&page);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].message.contains("alt text"), "{errors:?}");
    }

    #[test]
    fn a_blockquote_avatar_marked_decorative_is_allowed() {
        let registry = ComponentRegistry::new();
        let page = json!({ "content": [
            { "type": "BlockquoteExtended", "props": {
                "text": "Quote", "attribution": "A. Author",
                "imageUrl": "/img/avatar.jpg", "isDecorative": true
            } }
        ] });
        assert!(registry.validate_page(&page).is_empty());
    }

    #[test]
    fn a_blockquote_avatar_without_alt_or_decorative_is_refused() {
        let registry = ComponentRegistry::new();
        let page = json!({ "content": [
            { "type": "BlockquoteExtended", "props": {
                "text": "Quote", "attribution": "A. Author",
                "imageUrl": "/img/avatar.jpg"
            } }
        ] });
        let errors = registry.validate_page(&page);
        assert_eq!(errors.len(), 1, "{errors:?}");
    }

    #[test]
    fn validate_page_catches_missing_alt() {
        let registry = ComponentRegistry::new();
        let page = json!({ "content": [
            { "type": "ContentFeature", "props": { "imageUrl": "/img/photo.jpg" } }
        ] });
        let errors = registry.validate_page(&page);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].message.contains("alt text"));
    }

    #[test]
    fn validate_page_allows_decorative_image() {
        let registry = ComponentRegistry::new();
        let page = json!({ "content": [
            { "type": "ContentFeature", "props": { "imageUrl": "/img/bg.jpg", "isDecorative": true } }
        ] });
        assert!(registry.validate_page(&page).is_empty());
    }

    #[test]
    fn validate_valid_page() {
        let registry = ComponentRegistry::new();
        let page = json!({ "content": [
            { "type": "Hero", "props": { "title": "Hello", "variant": "standard" } },
            { "type": "TextBlock", "props": { "content": "Some text" } }
        ] });
        assert!(registry.validate_page(&page).is_empty());
    }

    #[test]
    fn an_unknown_variant_is_refused_in_a_zone() {
        let registry = ComponentRegistry::new();
        let page = json!({ "content": [
            { "type": "Columns", "props": { "layout": "1/2+1/2" }, "zones": {
                "zone-0": [ { "type": "Cta", "props": { "variant": "blinking" } } ]
            } }
        ] });
        let errors = registry.validate_page(&page);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].location, "content[0].zones.zone-0[0]");
        assert_eq!(errors[0].field, "variant");
        assert!(errors[0].message.contains("blinking"));
    }

    #[test]
    fn a_javascript_url_is_refused() {
        let registry = ComponentRegistry::new();
        let page = json!({ "content": [
            { "type": "Hero", "props": { "ctaText": "Go", "ctaUrl": "javascript:alert(1)" } }
        ] });
        let errors = registry.validate_page(&page);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].field, "ctaUrl");
    }

    #[test]
    fn a_legacy_style_prop_still_validates() {
        // Pages saved before variants replaced the inline style props.
        let registry = ComponentRegistry::new();
        let page = json!({ "content": [
            { "type": "Hero", "props": { "title": "x", "backgroundColor": "#123456" } },
            { "type": "Columns", "props": { "layout": "1/2+1/2", "gap": "3rem" } },
            { "type": "SectionWrapper", "props": { "backgroundColor": "red" } },
            { "type": "Cta", "props": { "backgroundColor": "blue" } }
        ] });
        assert!(registry.validate_page(&page).is_empty());
    }

    fn plugin_dir(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "trovato-pb-registry-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        for (rel, body) in files {
            let path = dir.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        dir
    }

    const PROMO: &str = r#"
type = "Promo"
label = "Promo"
category = "marketing"
stylesheets = ["promo.css"]
[[fields]]
name = "title"
type = "text"
max_length = 10
required = true
"#;

    #[test]
    fn a_plugin_component_is_registered_with_its_stylesheet_url() {
        let dir = plugin_dir(
            "ok",
            &[
                ("components/promo.component.toml", PROMO),
                ("static/promo.css", ".promo{}"),
            ],
        );
        let registry = ComponentRegistry::load(&[PluginComponentSource {
            name: "site".into(),
            dir: dir.clone(),
        }])
        .unwrap();
        let def = registry.get("Promo").unwrap();
        assert_eq!(def.source, ComponentSource::Plugin("site".into()));
        assert_eq!(def.stylesheets, vec!["/static/plugins/site/promo.css"]);
        assert_eq!(registry.all().last().unwrap().type_name, "Promo");

        let errors = registry.validate_page(&json!({ "content": [
            { "type": "Promo", "props": { "title": "far too long a title" } },
            { "type": "Promo", "props": {} }
        ] }));
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].message.contains("at most 10 characters"));
        assert_eq!(errors[1].message, "is required");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn two_plugins_declaring_one_type_are_refused_naming_both() {
        let a = plugin_dir(
            "dup-a",
            &[
                ("components/promo.component.toml", PROMO),
                ("static/promo.css", ""),
            ],
        );
        let b = plugin_dir(
            "dup-b",
            &[
                ("components/promo.component.toml", PROMO),
                ("static/promo.css", ""),
            ],
        );
        let err = ComponentRegistry::load(&[
            PluginComponentSource {
                name: "alpha".into(),
                dir: a.clone(),
            },
            PluginComponentSource {
                name: "beta".into(),
                dir: b.clone(),
            },
        ])
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("plugin 'alpha'"), "{msg}");
        assert!(msg.contains("plugin 'beta'"), "{msg}");
        assert!(msg.contains("'Promo'"), "{msg}");
        std::fs::remove_dir_all(a).ok();
        std::fs::remove_dir_all(b).ok();
    }

    #[test]
    fn a_plugin_cannot_replace_a_standard_component() {
        let dir = plugin_dir(
            "hero",
            &[(
                "components/hero.component.toml",
                "type = \"Hero\"\nlabel = \"Mine\"\ncategory = \"c\"\n",
            )],
        );
        let err = ComponentRegistry::load(&[PluginComponentSource {
            name: "site".into(),
            dir: dir.clone(),
        }])
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("the kernel") && msg.contains("plugin 'site'"),
            "{msg}"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    const FOOTER_ROOT: &str = r#"
classes = ["page--no-footer-cta"]
[[fields]]
name = "footer_mode"
type = "select"
default = "default"
options = [
  { value = "default" },
  { value = "hide", class = "page--no-footer-cta" },
  { value = "custom" },
]
[[fields]]
name = "footer_text"
type = "rich_text"
max_length = 20
"#;

    fn root_registry(tag: &str) -> (ComponentRegistry, PathBuf) {
        let dir = plugin_dir(tag, &[("components/page.root.toml", FOOTER_ROOT)]);
        let registry = ComponentRegistry::load(&[PluginComponentSource {
            name: "site".into(),
            dir: dir.clone(),
        }])
        .unwrap();
        (registry, dir)
    }

    #[test]
    fn a_plugin_declares_page_root_fields() {
        let (registry, dir) = root_registry("root-ok");
        let names: Vec<&str> = registry
            .root_fields()
            .iter()
            .map(|f| f.field.name.as_str())
            .collect();
        assert_eq!(names, vec!["footer_mode", "footer_text"]);
        assert_eq!(
            registry.root_fields()[0].source,
            ComponentSource::Plugin("site".into())
        );
        assert_eq!(registry.root_classes(), ["page--no-footer-cta"]);
        // The standard registry declares none.
        assert!(ComponentRegistry::new().root_fields().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn root_values_are_validated_in_either_shape() {
        let (registry, dir) = root_registry("root-validate");
        let valid = [
            json!({ "content": [], "root": { "title": "T", "footer_mode": "hide" } }),
            json!({ "content": [], "root": { "props": { "title": "T", "footer_mode": "custom", "footer_text": "Hi" } } }),
            json!({ "content": [], "root": {} }),
            json!({ "content": [] }),
        ];
        for page in &valid {
            assert!(registry.validate_page(page).is_empty(), "{page}");
        }

        for (page, field, needle) in [
            (
                json!({ "content": [], "root": { "footer_mode": "blink" } }),
                "footer_mode",
                "'blink' is not one of: default, hide, custom",
            ),
            (
                json!({ "content": [], "root": { "props": { "footer_text": "x".repeat(21) } } }),
                "footer_text",
                "at most 20 characters",
            ),
            (
                json!({ "content": [], "root": { "title": "T", "colour": "red" } }),
                "colour",
                "not a page root field",
            ),
            (
                json!({ "content": [], "root": { "title": 3 } }),
                "title",
                "must be text",
            ),
        ] {
            let errors = registry.validate_page(&page);
            assert_eq!(errors.len(), 1, "{page}: {errors:?}");
            assert_eq!(errors[0].location, "root");
            assert_eq!(errors[0].component, "");
            assert_eq!(errors[0].field, field);
            assert!(errors[0].message.contains(needle), "{errors:?}");
            assert!(
                errors[0]
                    .to_string()
                    .starts_with(&format!("root: {field}: "))
            );
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_root_key_without_a_declaring_plugin_is_refused_on_save() {
        // The plugin that declared it is disabled: the standard registry.
        let registry = ComponentRegistry::new();
        let errors = registry.validate_page(&json!({
            "content": [], "root": { "title": "T", "footer_mode": "hide" }
        }));
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].field, "footer_mode");
    }

    #[test]
    fn two_plugins_declaring_one_root_field_are_refused_naming_both() {
        let a = plugin_dir("root-dup-a", &[("components/page.root.toml", FOOTER_ROOT)]);
        let b = plugin_dir(
            "root-dup-b",
            &[(
                "components/other.root.toml",
                "[[fields]]\nname = \"footer_text\"\ntype = \"text\"\n",
            )],
        );
        let err = ComponentRegistry::load(&[
            PluginComponentSource {
                name: "beta".into(),
                dir: b.clone(),
            },
            PluginComponentSource {
                name: "alpha".into(),
                dir: a.clone(),
            },
        ])
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("page root field 'footer_text'"), "{msg}");
        assert!(msg.contains("plugin 'alpha'"), "{msg}");
        assert!(msg.contains("plugin 'beta'"), "{msg}");
        std::fs::remove_dir_all(a).ok();
        std::fs::remove_dir_all(b).ok();
    }

    #[test]
    fn a_root_field_cannot_redeclare_title_or_default_outside_its_rules() {
        let dir = plugin_dir(
            "root-title",
            &[(
                "components/page.root.toml",
                "[[fields]]\nname = \"title\"\ntype = \"text\"\n",
            )],
        );
        let err = ComponentRegistry::load(&[PluginComponentSource {
            name: "site".into(),
            dir: dir.clone(),
        }])
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("reserved") && msg.contains("plugin 'site'"),
            "{msg}"
        );
        std::fs::remove_dir_all(dir).ok();

        let dir = plugin_dir(
            "root-default",
            &[(
                "components/page.root.toml",
                "[[fields]]\nname = \"mode\"\ntype = \"select\"\ndefault = \"z\"\noptions = [{ value = \"a\" }]\n",
            )],
        );
        let err = ComponentRegistry::load(&[PluginComponentSource {
            name: "site".into(),
            dir: dir.clone(),
        }])
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("default its own rules refuse"), "{msg}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn root_values_read_both_shapes_props_first() {
        let values =
            root_values(&json!({ "title": "flat", "a": 1, "props": { "title": "nested" } }));
        assert_eq!(values["title"], "nested");
        assert_eq!(values["a"], 1);
        assert!(!values.contains_key("props"));
        assert!(root_values(&json!("x")).is_empty());
    }

    #[test]
    fn a_missing_template_is_named() {
        let registry = ComponentRegistry::new();
        let err = registry
            .check_templates(|t| t != "pb/hero.html")
            .unwrap_err();
        assert!(err.to_string().contains("pb/hero.html"), "{err}");
        registry.check_templates(|_| true).unwrap();
    }

    /// The editor's tests run against `standard-registry.json`, the standard
    /// registry exactly as `/api/v1/page-builder/components` serves it, so the
    /// editor is tested against what the kernel declares. Regenerate with
    /// `TROVATO_UPDATE_FIXTURES=1 cargo test -p trovato-kernel --lib editor_fixture`.
    #[test]
    fn the_editor_fixture_is_the_standard_registry() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../plugins/trovato_page_builder/editor/src/__fixtures__/standard-registry.json",
        );
        let registry = ComponentRegistry::new();
        let mut current = serde_json::to_string_pretty(
            &json!({ "data": registry.all(), "root_fields": registry.root_fields() }),
        )
        .unwrap();
        current.push('\n');
        if std::env::var_os("TROVATO_UPDATE_FIXTURES").is_some() {
            std::fs::write(&path, &current).unwrap();
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == current,
            "{} is out of date; regenerate it with TROVATO_UPDATE_FIXTURES=1",
            path.display()
        );
    }
}
