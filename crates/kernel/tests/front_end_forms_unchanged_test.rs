#![allow(clippy::unwrap_used, clippy::expect_used)]
//! The form templates the front end shares with the admin render exactly as
//! they did before B199, byte for byte, outside an admin page.
//!
//! B199 passed the words in `templates/form/*.html` through `t` and fixed the
//! file widget's label on admin pages. Outside the admin shell's translation
//! scope `t` returns its English source and `admin_render()` is false, so the
//! front end must see no difference. The expected output under
//! `tests/fixtures/front_end_forms/` was rendered from the templates on `main`
//! before the change (`B199_RECORD_FROM=<old templates dir> cargo test
//! --test front_end_forms_unchanged_test` rewrites it from another tree).
//!
//! No database: the theme engine alone.

use std::path::PathBuf;

use serde_json::json;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/front_end_forms")
}

/// Each shared template that changed, with a context that reaches every
/// changed line.
fn cases() -> Vec<(&'static str, &'static str, serde_json::Value)> {
    vec![
        (
            "select",
            "form/select.html",
            json!({
                "name": "colour",
                "element": {"title": "Colour", "required": false, "multiple": false,
                    "options": [["red", "Red"], ["blue", "Blue"]], "default_value": "blue",
                    "attributes": {}, "description": "Pick one."}
            }),
        ),
        (
            "container",
            "form/container.html",
            json!({
                "name": "links",
                "children": "<p>child</p>",
                "element": {"title": "Links", "attributes": {"data-add-another": "links"},
                    "description": "Add as many as you like."}
            }),
        ),
        (
            "container-full",
            "form/container.html",
            json!({
                "name": "links",
                "children": "",
                "element": {"title": "Links", "attributes": {"data-add-another": "links", "data-full": "1"}}
            }),
        ),
        (
            "file-upload",
            "form/file-upload.html",
            json!({
                "name": "attachments",
                "max_size": "5MB",
                "existing_files": [
                    {"id": "f1", "filename": "photo.jpg", "filesize": 20480, "filemime": "image/jpeg",
                     "url": "/files/photo.jpg", "alt_text": "A photo"},
                    {"id": "f2", "filename": "notes.pdf", "filesize": 3072, "filemime": "application/pdf",
                     "url": "/files/notes.pdf"}
                ],
                "element": {"title": "Attachments", "required": false, "multiple": true}
            }),
        ),
        (
            "file-upload-accept",
            "form/file-upload.html",
            json!({
                "name": "image",
                "element": {"title": "Image", "accept": "image/*", "required": true, "multiple": false}
            }),
        ),
        (
            "widget-compound",
            "form/widget--compound.html",
            json!({
                "name": "sections",
                "element": {"title": "Sections", "attributes": {}, "default_value": "[]", "required": false}
            }),
        ),
        (
            "widget-file",
            "form/widget--file.html",
            json!({
                "name": "document",
                "element": {"title": "Document", "required": true, "default_value": ""}
            }),
        ),
        (
            "widget-file-attached",
            "form/widget--file.html",
            json!({
                "name": "document",
                "element": {"title": "Document", "required": false, "default_value": "0193-file"}
            }),
        ),
    ]
}

fn render(templates: &PathBuf, template: &str, context: &serde_json::Value) -> String {
    let engine = trovato_kernel::theme::ThemeEngine::new(
        std::slice::from_ref(templates),
        None,
        std::sync::Arc::new(trovato_kernel::metrics::Metrics::new()),
    )
    .expect("the templates load");
    let context = tera::Context::from_value(context.clone()).unwrap();
    engine.tera().render(template, &context).expect("renders")
}

#[test]
fn the_shared_form_templates_render_on_the_front_end_exactly_as_before() {
    if let Ok(old) = std::env::var("B199_RECORD_FROM") {
        let old = PathBuf::from(old);
        std::fs::create_dir_all(fixtures()).unwrap();
        for (name, template, context) in cases() {
            std::fs::write(
                fixtures().join(format!("{name}.html")),
                render(&old, template, &context),
            )
            .unwrap();
        }
        return;
    }
    let templates = root().join("templates");
    for (name, template, context) in cases() {
        let expected = std::fs::read_to_string(fixtures().join(format!("{name}.html")))
            .unwrap_or_else(|_| panic!("the recorded output for {name}"));
        let now = render(&templates, template, &context);
        assert_eq!(
            now, expected,
            "{template} ({name}) renders differently on the front end"
        );
    }
}

/// And on an admin page the file widget's label names its file chooser, the
/// fix the front end does not get.
#[test]
fn on_an_admin_page_the_file_widget_label_names_the_file_chooser() {
    let templates = root().join("templates");
    let (_, template, context) = cases()
        .into_iter()
        .find(|(name, _, _)| *name == "widget-file")
        .unwrap();
    let html = trovato_kernel::i18n::scope("en", || render(&templates, template, &context));
    assert!(html.contains(r#"<label for="document-file""#), "{html}");
    assert!(html.contains(r#"<input type="file" id="document-file""#), "{html}");
}
