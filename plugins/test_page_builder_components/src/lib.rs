//! A13 fixture: a site plugin's own page builder components.
//!
//! The two components are declared in `components/`, rendered by
//! `templates/pb/` and styled by `static/`, and none of that is code: a plugin
//! ships page builder components without touching the kernel. The same goes
//! for `components/page.root.toml`, the page root fields a page uses to keep,
//! hide or replace the footer call to action that
//! `templates/elements/tile--custom_html--pb_footer_cta.html` renders. This module
//! only declares `pb_landing`, a content type whose `field_layout` is a page
//! builder field, so there is a page to compose them on.

use trovato_sdk::prelude::*;

/// The landing page content type.
pub const LANDING_TYPE: &str = "pb_landing";

/// Declare the landing page type.
#[plugin_tap]
pub fn tap_item_info() -> Vec<ContentTypeDefinition> {
    vec![ContentTypeDefinition {
        machine_name: LANDING_TYPE.into(),
        label: "Landing page".into(),
        description: "A page composed in the page builder".into(),
        title_label: None,
        fields: vec![FieldDefinition::new("field_layout", FieldType::PageBuilder).label("Layout")],
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declares_the_landing_page_type() {
        let types = __inner_tap_item_info();
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].machine_name, LANDING_TYPE);
        assert!(matches!(
            types[0].fields[0].field_type,
            FieldType::PageBuilder
        ));
    }
}
