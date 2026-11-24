//! The page builder component file format, `<name>.component.toml`.
//!
//! One file declares one component: its type name, label, category, fields,
//! zones, the CSS classes its variants may add, its template and its
//! stylesheets. The kernel's standard components and every plugin's use the
//! same format and the same parser, so there is one path and not two.
//!
//! A file is read into `RawComponent` with unknown keys refused, then checked
//! into a [`ComponentDefinition`]: a misspelt key, a select option class the
//! component did not declare, or a key that does not belong to a field's type
//! is an error naming the file, at load, rather than a silently ignored line.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Where a component definition came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum ComponentSource {
    /// One of the kernel's standard components.
    Kernel,
    /// A component an enabled plugin ships, by plugin name.
    Plugin(String),
}

impl std::fmt::Display for ComponentSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Kernel => write!(f, "the kernel"),
            Self::Plugin(name) => write!(f, "plugin '{name}'"),
        }
    }
}

/// One option of a `select` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectOption {
    /// The stored value.
    pub value: String,
    /// What the editor shows. Defaults to the value.
    #[serde(default)]
    pub label: Option<String>,
    /// The CSS class this option adds to the component's `variant_classes`.
    /// Must be one of the component's declared `classes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
}

/// What a reference field points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceTarget {
    /// A content item, by its UUID.
    Item,
    /// A gather query, by its query id; the template receives its first page.
    Gather,
}

/// A field's type, with the limits that belong to that type.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldKind {
    /// Plain text. `multiline` makes it a textarea in the editor.
    Text {
        /// Most characters allowed.
        #[serde(skip_serializing_if = "Option::is_none")]
        max_length: Option<usize>,
        /// Fewest characters allowed in a non empty value.
        #[serde(skip_serializing_if = "Option::is_none")]
        min_length: Option<usize>,
        /// A textarea rather than a single line.
        multiline: bool,
    },
    /// Markdown, rendered and sanitized by the kernel. The template receives
    /// the source as `<name>` and the sanitized HTML as `<name>_html`.
    RichText {
        /// Most characters of Markdown source allowed.
        #[serde(skip_serializing_if = "Option::is_none")]
        max_length: Option<usize>,
        /// The smallest heading level the HTML may contain (default 2: the
        /// page template owns the level one heading).
        min_heading: u8,
    },
    /// A number, optionally an integer, within optional bounds.
    Number {
        /// Smallest value allowed.
        #[serde(skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        /// Largest value allowed.
        #[serde(skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        /// Whole numbers only.
        integer: bool,
    },
    /// A heading level, 2 to 6: the page template owns level 1.
    HeadingLevel,
    /// A URL: relative, or `http`, `https`, `mailto` or `tel`.
    Url,
    /// An image from the media library: `{ src, alt, decorative }`. Alt text
    /// is required unless `decorative` is true.
    Image {
        /// Most characters of alt text allowed.
        alt_max_length: usize,
    },
    /// A link: `{ url, label, new_tab }`. A link with a URL needs a label.
    Link {
        /// Most characters of label allowed.
        label_max_length: usize,
    },
    /// One of a declared list of values.
    Select {
        /// The allowed values, in the order the editor offers them.
        options: Vec<SelectOption>,
    },
    /// True or false.
    Boolean,
    /// A repeatable list of objects, each with the sub fields declared here.
    List {
        /// The fields of one entry.
        fields: Vec<FieldDefinition>,
        /// Fewest entries allowed.
        #[serde(skip_serializing_if = "Option::is_none")]
        min_items: Option<usize>,
        /// Most entries allowed.
        #[serde(skip_serializing_if = "Option::is_none")]
        max_items: Option<usize>,
        /// The values a new entry starts with in the editor.
        #[serde(skip_serializing_if = "Option::is_none")]
        default_item: Option<serde_json::Value>,
    },
    /// A reference to a content item or a gather listing.
    Reference {
        /// What it refers to.
        target: ReferenceTarget,
        /// For a gather, the most rows the template receives (default 10).
        limit: u32,
        /// For an item, the content type the editor searches.
        #[serde(skip_serializing_if = "Option::is_none")]
        item_type: Option<String>,
    },
}

impl FieldKind {
    /// The `type` name the file uses for this kind.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::RichText { .. } => "rich_text",
            Self::Number { .. } => "number",
            Self::HeadingLevel => "heading_level",
            Self::Url => "url",
            Self::Image { .. } => "image",
            Self::Link { .. } => "link",
            Self::Select { .. } => "select",
            Self::Boolean => "boolean",
            Self::List { .. } => "list",
            Self::Reference { .. } => "reference",
        }
    }
}

/// One field of a component, or of a list entry.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldDefinition {
    /// The prop name the value is stored under.
    pub name: String,
    /// What the editor shows. `None` lets the editor use the name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Help text shown under the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    /// A save without a value is refused.
    pub required: bool,
    /// The value a new component starts with in the editor. Not applied at
    /// render: a template gives an absent value its own default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// Accepted and validated, not offered in the editor.
    pub hidden: bool,
    /// Why the field is kept, if it is only kept so stored pages validate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
    /// The type and its limits.
    #[serde(flatten)]
    pub kind: FieldKind,
}

/// A checked component definition, as the registry, validator, renderer and
/// editor use it.
#[derive(Debug, Clone, Serialize)]
pub struct ComponentDefinition {
    /// PascalCase type name, as stored in a page's `type` (e.g. `"Hero"`).
    pub type_name: String,
    /// The editor's label.
    pub label: String,
    /// The editor's category.
    pub category: String,
    /// One sentence for the component picker.
    pub description: String,
    /// The fields, in editor order.
    pub fields: Vec<FieldDefinition>,
    /// Named drop zones; empty for a component with none.
    pub zone_names: Vec<String>,
    /// Whether the component has any drop zone.
    pub has_zones: bool,
    /// Every CSS class a variant may add to the component.
    pub classes: Vec<String>,
    /// The Tera template, resolved on the kernel's template search path.
    pub template: String,
    /// Stylesheet URLs the page includes whenever the component renders.
    pub stylesheets: Vec<String>,
    /// Offered in the editor's component picker.
    pub in_editor: bool,
    /// Who declared it.
    pub source: ComponentSource,
    /// A JSON-Schema-like summary of the fields, kept for clients that read
    /// the registry before field definitions existed.
    pub props: serde_json::Value,
}

impl ComponentDefinition {
    /// The field named `name`.
    pub fn field(&self, name: &str) -> Option<&FieldDefinition> {
        self.fields.iter().find(|f| f.name == name)
    }
}

// ---------------------------------------------------------------------------
// The file as written
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawComponent {
    #[serde(rename = "type")]
    type_name: String,
    label: String,
    category: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    stylesheets: Vec<String>,
    #[serde(default)]
    zones: Vec<String>,
    #[serde(default)]
    classes: Vec<String>,
    #[serde(default = "yes")]
    in_editor: bool,
    #[serde(default)]
    fields: Vec<RawField>,
}

fn yes() -> bool {
    true
}

/// Every key any field type takes; which ones a type accepts is checked in
/// [`RawField::check`], so a key the type does not use is an error too.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawField {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    help: Option<String>,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    default: Option<toml::Value>,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    deprecated: Option<String>,
    // text, rich_text
    #[serde(default)]
    max_length: Option<usize>,
    #[serde(default)]
    min_length: Option<usize>,
    #[serde(default)]
    multiline: Option<bool>,
    #[serde(default)]
    min_heading: Option<u8>,
    // number
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    #[serde(default)]
    integer: Option<bool>,
    // image, link
    #[serde(default)]
    alt_max_length: Option<usize>,
    #[serde(default)]
    label_max_length: Option<usize>,
    // select
    #[serde(default)]
    options: Option<Vec<SelectOption>>,
    // list
    #[serde(default)]
    fields: Option<Vec<RawField>>,
    #[serde(default)]
    min_items: Option<usize>,
    #[serde(default)]
    max_items: Option<usize>,
    #[serde(default)]
    default_item: Option<toml::Value>,
    // reference
    #[serde(default)]
    target: Option<ReferenceTarget>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    item_type: Option<String>,
}

/// Default most characters of image alt text.
pub const DEFAULT_ALT_MAX_LENGTH: usize = 250;
/// Default most characters of a link label.
pub const DEFAULT_LABEL_MAX_LENGTH: usize = 120;
/// Default rows a gather reference hands its template.
pub const DEFAULT_GATHER_LIMIT: u32 = 10;
/// Most rows a gather reference may ask for.
pub const MAX_GATHER_LIMIT: u32 = 50;

impl RawField {
    /// The keys this field set, other than the ones every field may set.
    fn set_keys(&self) -> Vec<&'static str> {
        let mut keys = Vec::new();
        let mut note = |set: bool, key: &'static str| {
            if set {
                keys.push(key);
            }
        };
        note(self.max_length.is_some(), "max_length");
        note(self.min_length.is_some(), "min_length");
        note(self.multiline.is_some(), "multiline");
        note(self.min_heading.is_some(), "min_heading");
        note(self.min.is_some(), "min");
        note(self.max.is_some(), "max");
        note(self.integer.is_some(), "integer");
        note(self.alt_max_length.is_some(), "alt_max_length");
        note(self.label_max_length.is_some(), "label_max_length");
        note(self.options.is_some(), "options");
        note(self.fields.is_some(), "fields");
        note(self.min_items.is_some(), "min_items");
        note(self.max_items.is_some(), "max_items");
        note(self.default_item.is_some(), "default_item");
        note(self.target.is_some(), "target");
        note(self.limit.is_some(), "limit");
        note(self.item_type.is_some(), "item_type");
        keys
    }

    fn check(self, classes: &[String], depth: usize) -> Result<FieldDefinition> {
        let name = self.name.clone();
        if !is_prop_name(&name) {
            bail!(
                "field name '{name}' must be letters, digits and underscores, starting with a letter"
            );
        }
        let allowed: &[&str] = match self.kind.as_str() {
            "text" => &["max_length", "min_length", "multiline"],
            "rich_text" => &["max_length", "min_heading"],
            "number" => &["min", "max", "integer"],
            "heading_level" | "url" | "boolean" => &[],
            "image" => &["alt_max_length"],
            "link" => &["label_max_length"],
            "select" => &["options"],
            "list" => &["fields", "min_items", "max_items", "default_item"],
            "reference" => &["target", "limit", "item_type"],
            other => bail!(
                "field '{name}' has unknown type '{other}' (expected text, rich_text, number, \
                 heading_level, url, image, link, select, boolean, list or reference)"
            ),
        };
        if let Some(key) = self.set_keys().into_iter().find(|k| !allowed.contains(k)) {
            bail!(
                "field '{name}': '{key}' does not apply to a {} field",
                self.kind
            );
        }

        let kind = match self.kind.as_str() {
            "text" => {
                if let (Some(min), Some(max)) = (self.min_length, self.max_length)
                    && min > max
                {
                    bail!("field '{name}': min_length {min} is more than max_length {max}");
                }
                FieldKind::Text {
                    max_length: self.max_length,
                    min_length: self.min_length,
                    multiline: self.multiline.unwrap_or(false),
                }
            }
            "rich_text" => {
                let min_heading = self.min_heading.unwrap_or(2);
                if !(1..=6).contains(&min_heading) {
                    bail!("field '{name}': min_heading must be 1 to 6");
                }
                FieldKind::RichText {
                    max_length: self.max_length,
                    min_heading,
                }
            }
            "number" => {
                if let (Some(min), Some(max)) = (self.min, self.max)
                    && min > max
                {
                    bail!("field '{name}': min {min} is more than max {max}");
                }
                FieldKind::Number {
                    min: self.min,
                    max: self.max,
                    integer: self.integer.unwrap_or(false),
                }
            }
            "heading_level" => FieldKind::HeadingLevel,
            "url" => FieldKind::Url,
            "boolean" => FieldKind::Boolean,
            "image" => FieldKind::Image {
                alt_max_length: self.alt_max_length.unwrap_or(DEFAULT_ALT_MAX_LENGTH),
            },
            "link" => FieldKind::Link {
                label_max_length: self.label_max_length.unwrap_or(DEFAULT_LABEL_MAX_LENGTH),
            },
            "select" => {
                let options = self.options.unwrap_or_default();
                if options.is_empty() {
                    bail!("select field '{name}' declares no options");
                }
                let mut seen = std::collections::HashSet::new();
                for option in &options {
                    if !seen.insert(option.value.as_str()) {
                        bail!(
                            "select field '{name}' declares the value '{}' twice",
                            option.value
                        );
                    }
                    if let Some(class) = &option.class
                        && !classes.contains(class)
                    {
                        bail!(
                            "select field '{name}', option '{}': class '{class}' is not in the \
                             declared classes list",
                            option.value
                        );
                    }
                }
                FieldKind::Select { options }
            }
            "list" => {
                if depth > 0 {
                    bail!("list field '{name}': a list entry cannot hold another list");
                }
                let raw = self.fields.unwrap_or_default();
                if raw.is_empty() {
                    bail!("list field '{name}' declares no fields");
                }
                let fields = check_fields(raw, classes, depth + 1)
                    .with_context(|| format!("in list field '{name}'"))?;
                if let (Some(min), Some(max)) = (self.min_items, self.max_items)
                    && min > max
                {
                    bail!("list field '{name}': min_items {min} is more than max_items {max}");
                }
                FieldKind::List {
                    fields,
                    min_items: self.min_items,
                    max_items: self.max_items,
                    default_item: self.default_item.map(toml_to_json),
                }
            }
            "reference" => {
                let Some(target) = self.target else {
                    bail!("reference field '{name}' needs a target: \"item\" or \"gather\"");
                };
                let limit = self.limit.unwrap_or(DEFAULT_GATHER_LIMIT);
                if limit == 0 || limit > MAX_GATHER_LIMIT {
                    bail!("reference field '{name}': limit must be 1 to {MAX_GATHER_LIMIT}");
                }
                if self.item_type.is_some() && target != ReferenceTarget::Item {
                    bail!("reference field '{name}': item_type applies to an item reference only");
                }
                FieldKind::Reference {
                    target,
                    limit,
                    item_type: self.item_type,
                }
            }
            other => bail!("field '{name}' has unknown type '{other}'"),
        };

        Ok(FieldDefinition {
            name,
            label: self.label,
            help: self.help,
            required: self.required,
            default: self.default.map(toml_to_json),
            hidden: self.hidden,
            deprecated: self.deprecated,
            kind,
        })
    }
}

fn check_fields(
    raw: Vec<RawField>,
    classes: &[String],
    depth: usize,
) -> Result<Vec<FieldDefinition>> {
    let mut out: Vec<FieldDefinition> = Vec::with_capacity(raw.len());
    for field in raw {
        let field = field.check(classes, depth)?;
        if out.iter().any(|f| f.name == field.name) {
            bail!("field '{}' is declared twice", field.name);
        }
        if depth == 0 && field.name == "id" {
            bail!("field name 'id' is reserved: the editor stores each component's id there");
        }
        out.push(field);
    }
    Ok(out)
}

/// A prop name: ASCII letters, digits and underscores, starting with a letter.
fn is_prop_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A PascalCase type name: an ASCII capital, then letters and digits.
fn is_type_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_alphanumeric())
        && name.len() <= 64
}

/// A CSS class name the sanitizer and a browser both read as one class.
fn is_class_name(class: &str) -> bool {
    !class.is_empty()
        && class.len() <= 100
        && class
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && class
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A template or stylesheet path: relative, `/` separated, no `..`.
fn is_relative_asset_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

/// Convert `PascalCase` to `kebab-case`: `"CardGrid"` → `"card-grid"`.
pub fn to_kebab_case(name: &str) -> String {
    let mut result = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            result.push('-');
        }
        result.push(ch.to_ascii_lowercase());
    }
    result
}

fn toml_to_json(value: toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(s) => serde_json::Value::String(s),
        toml::Value::Integer(i) => serde_json::Value::from(i),
        toml::Value::Float(f) => serde_json::Number::from_f64(f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        toml::Value::Boolean(b) => serde_json::Value::Bool(b),
        toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
        toml::Value::Array(a) => {
            serde_json::Value::Array(a.into_iter().map(toml_to_json).collect())
        }
        toml::Value::Table(t) => {
            serde_json::Value::Object(t.into_iter().map(|(k, v)| (k, toml_to_json(v))).collect())
        }
    }
}

/// The JSON-Schema-like `props` summary the registry served before fields
/// had definitions.
fn props_summary(fields: &[FieldDefinition]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for field in fields {
        let mut entry = serde_json::Map::new();
        let (ty, extra): (&str, Vec<(&str, serde_json::Value)>) = match &field.kind {
            FieldKind::Text { max_length, .. } | FieldKind::RichText { max_length, .. } => (
                "string",
                max_length
                    .map(|m| vec![("maxLength", serde_json::Value::from(m))])
                    .unwrap_or_default(),
            ),
            FieldKind::Url => ("string", vec![("format", "uri-reference".into())]),
            FieldKind::Number { min, max, integer } => {
                let mut extra = Vec::new();
                if let Some(min) = min {
                    extra.push(("minimum", serde_json::Value::from(*min)));
                }
                if let Some(max) = max {
                    extra.push(("maximum", serde_json::Value::from(*max)));
                }
                (if *integer { "integer" } else { "number" }, extra)
            }
            FieldKind::HeadingLevel => (
                "integer",
                vec![("minimum", 2.into()), ("maximum", 6.into())],
            ),
            FieldKind::Select { options } => (
                "string",
                vec![(
                    "enum",
                    options
                        .iter()
                        .map(|o| serde_json::Value::String(o.value.clone()))
                        .collect(),
                )],
            ),
            FieldKind::Boolean => ("boolean", vec![]),
            FieldKind::List { .. } => ("array", vec![]),
            FieldKind::Image { .. } | FieldKind::Link { .. } => ("object", vec![]),
            FieldKind::Reference { .. } => ("string", vec![]),
        };
        entry.insert("type".into(), ty.into());
        for (k, v) in extra {
            entry.insert(k.into(), v);
        }
        if field.required {
            entry.insert("required".into(), true.into());
        }
        if let Some(default) = &field.default {
            entry.insert("default".into(), default.clone());
        }
        map.insert(field.name.clone(), serde_json::Value::Object(entry));
    }
    serde_json::Value::Object(map)
}

/// The page root field names the kernel keeps for itself: `title`, the built
/// in root field; `props`, under which current Puck versions nest the root's
/// values; and `variant_classes`, which templates receive beside the values.
pub const RESERVED_ROOT_FIELD_NAMES: &[&str] = &["title", "props", "variant_classes"];

/// The page root file as written: `components/<name>.root.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoot {
    #[serde(default)]
    classes: Vec<String>,
    #[serde(default)]
    fields: Vec<RawField>,
}

/// One page level field a plugin declares, as the registry serves it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RootFieldDefinition {
    /// The field, exactly as a component field is declared.
    #[serde(flatten)]
    pub field: FieldDefinition,
    /// Who declared it.
    pub source: ComponentSource,
}

/// A checked page root file: the fields it declares and the CSS classes its
/// select options may add to the page's `page_root.variant_classes`.
#[derive(Debug, Clone, PartialEq)]
pub struct RootDeclaration {
    /// Every class a select option in this file may add.
    pub classes: Vec<String>,
    /// The fields, in the order the editor's page settings form shows them.
    pub fields: Vec<RootFieldDefinition>,
}

/// Parse and check one page root file.
///
/// The fields use the component field format, types, limits and checks; a
/// name the kernel reserves ([`RESERVED_ROOT_FIELD_NAMES`]) is refused, and a
/// select option's class must be one of the file's own `classes`.
pub fn parse_root(text: &str, source: ComponentSource) -> Result<RootDeclaration> {
    let raw: RawRoot = toml::from_str(text).context("not a valid page root file")?;
    if let Some(class) = raw.classes.iter().find(|c| !is_class_name(c)) {
        bail!("'{class}' is not a CSS class name");
    }
    for field in &raw.fields {
        if RESERVED_ROOT_FIELD_NAMES.contains(&field.name.as_str()) {
            bail!(
                "page root field name '{}' is reserved by the kernel ({})",
                field.name,
                RESERVED_ROOT_FIELD_NAMES.join(", ")
            );
        }
    }
    let fields = check_fields(raw.fields, &raw.classes, 0)?;
    Ok(RootDeclaration {
        classes: raw.classes,
        fields: fields
            .into_iter()
            .map(|field| RootFieldDefinition {
                field,
                source: source.clone(),
            })
            .collect(),
    })
}

/// Parse and check one component file.
///
/// `stylesheet_base` turns a declared stylesheet path into the URL a page
/// links (for a plugin, `/static/plugins/<name>/`); `stylesheet_exists` says
/// whether the file is there, so a typo is a load error and not a 404 on
/// every page that uses the component.
pub fn parse_component(
    text: &str,
    source: ComponentSource,
    stylesheet_base: &str,
    stylesheet_exists: &dyn Fn(&str) -> bool,
) -> Result<ComponentDefinition> {
    let raw: RawComponent = toml::from_str(text).context("not a valid component file")?;

    if !is_type_name(&raw.type_name) {
        bail!(
            "type '{}' must be PascalCase: an ASCII capital, then letters and digits",
            raw.type_name
        );
    }
    let type_name = raw.type_name;
    if raw.label.trim().is_empty() {
        bail!("component '{type_name}' has an empty label");
    }
    if raw.category.trim().is_empty() {
        bail!("component '{type_name}' has an empty category");
    }
    if let Some(class) = raw.classes.iter().find(|c| !is_class_name(c)) {
        bail!("component '{type_name}': '{class}' is not a CSS class name");
    }
    for zone in &raw.zones {
        if zone.is_empty()
            || !zone
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!(
                "component '{type_name}': zone name '{zone}' must be letters, digits, '-' and '_'"
            );
        }
    }

    let template = match raw.template {
        Some(t) => {
            if !is_relative_asset_path(&t) {
                bail!("component '{type_name}': template '{t}' must be a relative path");
            }
            t
        }
        None => format!("pb/{}.html", to_kebab_case(&type_name)),
    };

    let mut stylesheets = Vec::with_capacity(raw.stylesheets.len());
    for sheet in raw.stylesheets {
        if !is_relative_asset_path(&sheet) || !sheet.ends_with(".css") {
            bail!(
                "component '{type_name}': stylesheet '{sheet}' must be a relative path to a .css file"
            );
        }
        if !stylesheet_exists(&sheet) {
            bail!("component '{type_name}': stylesheet '{sheet}' does not exist");
        }
        stylesheets.push(format!("{stylesheet_base}{sheet}"));
    }

    let fields = check_fields(raw.fields, &raw.classes, 0)
        .with_context(|| format!("component '{type_name}'"))?;
    let props = props_summary(&fields);

    Ok(ComponentDefinition {
        type_name,
        label: raw.label,
        category: raw.category,
        description: raw.description,
        fields,
        has_zones: !raw.zones.is_empty(),
        zone_names: raw.zones,
        classes: raw.classes,
        template,
        stylesheets,
        in_editor: raw.in_editor,
        source,
        props,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<ComponentDefinition> {
        parse_component(text, ComponentSource::Kernel, "/static/", &|_| true)
    }

    #[test]
    fn a_minimal_file_gets_the_kebab_case_template() {
        let def =
            parse("type = \"PromoCards\"\nlabel = \"Promo\"\ncategory = \"content\"\n").unwrap();
        assert_eq!(def.template, "pb/promo-cards.html");
        assert!(def.fields.is_empty());
        assert!(def.in_editor);
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let err =
            parse("type = \"A\"\nlabel = \"A\"\ncategory = \"c\"\ncolour = \"red\"\n").unwrap_err();
        assert!(format!("{err:#}").contains("colour"), "{err:#}");
    }

    #[test]
    fn a_key_of_another_type_is_refused() {
        let err = parse(
            "type = \"A\"\nlabel = \"A\"\ncategory = \"c\"\n[[fields]]\nname = \"x\"\ntype = \"boolean\"\nmax_length = 3\n",
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("'max_length' does not apply to a boolean field"),
            "{err:#}"
        );
    }

    #[test]
    fn a_select_class_must_be_declared() {
        let err = parse(
            r#"type = "A"
label = "A"
category = "c"
classes = ["a--dark"]
[[fields]]
name = "tone"
type = "select"
options = [{ value = "light", class = "a--light" }]
"#,
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("'a--light' is not in"),
            "{err:#}"
        );
    }

    #[test]
    fn a_missing_stylesheet_is_refused() {
        let err = parse_component(
            "type = \"A\"\nlabel = \"A\"\ncategory = \"c\"\nstylesheets = [\"a.css\"]\n",
            ComponentSource::Plugin("p".into()),
            "/static/plugins/p/",
            &|_| false,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("does not exist"), "{err:#}");
    }

    #[test]
    fn a_stylesheet_cannot_climb_out() {
        let err = parse_component(
            "type = \"A\"\nlabel = \"A\"\ncategory = \"c\"\nstylesheets = [\"../x.css\"]\n",
            ComponentSource::Plugin("p".into()),
            "/static/plugins/p/",
            &|_| true,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("relative path"), "{err:#}");
    }

    #[test]
    fn nested_lists_are_refused() {
        let err = parse(
            r#"type = "A"
label = "A"
category = "c"
[[fields]]
name = "outer"
type = "list"
  [[fields.fields]]
  name = "inner"
  type = "list"
    [[fields.fields.fields]]
    name = "x"
    type = "text"
"#,
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("cannot hold another list"),
            "{err:#}"
        );
    }

    #[test]
    fn the_id_prop_is_reserved() {
        let err = parse(
            "type = \"A\"\nlabel = \"A\"\ncategory = \"c\"\n[[fields]]\nname = \"id\"\ntype = \"text\"\n",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("reserved"), "{err:#}");
    }

    #[test]
    fn a_root_file_declares_fields_with_the_component_format() {
        let root = parse_root(
            r#"classes = ["page--quiet"]
[[fields]]
name = "mode"
type = "select"
default = "a"
options = [{ value = "a" }, { value = "b", class = "page--quiet" }]
[[fields]]
name = "note"
type = "rich_text"
max_length = 20
"#,
            ComponentSource::Plugin("p".into()),
        )
        .unwrap();
        assert_eq!(root.classes, vec!["page--quiet"]);
        let names: Vec<&str> = root.fields.iter().map(|f| f.field.name.as_str()).collect();
        assert_eq!(names, vec!["mode", "note"]);
        assert_eq!(root.fields[0].source, ComponentSource::Plugin("p".into()));
    }

    #[test]
    fn a_root_file_cannot_take_a_reserved_name_or_an_unknown_key() {
        for name in ["title", "props", "variant_classes"] {
            let err = parse_root(
                &format!("[[fields]]\nname = \"{name}\"\ntype = \"text\"\n"),
                ComponentSource::Kernel,
            )
            .unwrap_err();
            assert!(format!("{err:#}").contains("reserved"), "{err:#}");
        }
        let err = parse_root("label = \"x\"\n", ComponentSource::Kernel).unwrap_err();
        assert!(format!("{err:#}").contains("label"), "{err:#}");
        let err = parse_root(
            "[[fields]]\nname = \"m\"\ntype = \"select\"\noptions = [{ value = \"a\", class = \"x\" }]\n",
            ComponentSource::Kernel,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("'x' is not in"), "{err:#}");
    }

    #[test]
    fn kebab_case() {
        assert_eq!(to_kebab_case("Hero"), "hero");
        assert_eq!(to_kebab_case("CardGrid"), "card-grid");
        assert_eq!(to_kebab_case("YouTubeEmbed"), "you-tube-embed");
        assert_eq!(to_kebab_case("Cta"), "cta");
    }
}
