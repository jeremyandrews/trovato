//! Asset libraries: the stylesheets and scripts a form widget needs, by name.
//!
//! The editor's script list used to live in one template,
//! `admin/content-form.html`, so the same `Blocks` field rendered on
//! `/item/{id}/edit` got a hidden input and a `<div data-block-editor>` that
//! nothing attached to. A library is defined once here and the form renderer
//! attaches it wherever a form contains the widget, on any page.
//!
//! Every third party script is pinned to a version and carries a `sha384`
//! integrity hash, rendered with `crossorigin="anonymous"`: the browser
//! refuses the file if the CDN ever serves different bytes. Kernel scripts are
//! files under `/static/js/`, so `script-src` needs no inline allowance.

use serde::Serialize;

use super::{ElementType, Form};

/// One script of a library.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Script {
    /// The script URL: `/static/js/…` or a pinned `https://cdn.jsdelivr.net/…`.
    pub src: &'static str,
    /// The Subresource Integrity hash, required for a third party script.
    pub integrity: Option<&'static str>,
}

/// A named set of stylesheets and scripts, attached together, in order.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Library {
    /// The name a form refers to it by.
    pub name: &'static str,
    /// Stylesheet URLs.
    pub css: &'static [&'static str],
    /// Scripts, in load order.
    pub js: &'static [Script],
}

const fn local(src: &'static str) -> Script {
    Script {
        src,
        integrity: None,
    }
}

const fn pinned(src: &'static str, integrity: &'static str) -> Script {
    Script {
        src,
        integrity: Some(integrity),
    }
}

/// Every library the kernel defines.
pub const LIBRARIES: &[Library] = &[
    Library {
        name: "form",
        css: &["/static/css/forms.css"],
        js: &[],
    },
    Library {
        name: "block_editor",
        css: &["/static/css/block-editor.css"],
        js: &[
            pinned(
                "https://cdn.jsdelivr.net/npm/@editorjs/editorjs@2.31.5/dist/editorjs.umd.js",
                "sha384-E5/pEEMYAvyHFTy440KAFTq7BNCQE989b8o0GU2oN7U8ZtTCBxLY7yqUOLrhEMtm",
            ),
            pinned(
                "https://cdn.jsdelivr.net/npm/@editorjs/header@2.8.8/dist/header.umd.js",
                "sha384-Oqp44fezQQlSFZnzotJupLkUgQK/KUQ3B+zKVhSGETp1RMpT+sg5kkAOgtuFkMRj",
            ),
            pinned(
                "https://cdn.jsdelivr.net/npm/@editorjs/list@1.9.0/dist/list.umd.js",
                "sha384-pt2axkhrlqv09EbFmJffXfINJyTZxEnHXulBal/0IZoIT/DIjN9Q8pxYzvJmol8z",
            ),
            pinned(
                "https://cdn.jsdelivr.net/npm/@editorjs/quote@2.7.6/dist/quote.umd.js",
                "sha384-z43sl0T5c0VaZ0qANtyMgguU+QmuuzTmE4lWCjGDs2IgVrbQn6ycdbCwemISQxug",
            ),
            pinned(
                "https://cdn.jsdelivr.net/npm/@editorjs/code@2.9.4/dist/code.umd.js",
                "sha384-pmKzfNHAxgIHBx/Nzm5HVAQyKQwkM/Mpcm2NSVPojR03VCm4g2kYBgzY9FsE4cXa",
            ),
            pinned(
                "https://cdn.jsdelivr.net/npm/@editorjs/image@2.10.3/dist/image.umd.js",
                "sha384-x+8X+am+peBvhMXyUZDpid1xuh/eJzrRGmPT7082F3Q4BL+GFvqjtx+b/MavaMhc",
            ),
            pinned(
                "https://cdn.jsdelivr.net/npm/@editorjs/delimiter@1.4.2/dist/delimiter.umd.js",
                "sha384-JxFg+sowMd+b7DsI8bCOvXPDtHPiIjXxSMk9WXaGWpDtSwzzMIb+tGwkaAh9qMgB",
            ),
            pinned(
                "https://cdn.jsdelivr.net/npm/@editorjs/embed@2.8.0/dist/embed.umd.js",
                "sha384-/1ZI1HlHcevdvuxte+CPpgCdkHnvAqD4P9oyVCu1bxTotpiAJDerFB+SObio3CH1",
            ),
            local("/static/js/block-editor.js"),
        ],
    },
    Library {
        name: "compound_field",
        css: &["/static/css/compound-field.css"],
        js: &[local("/static/js/compound-field.js")],
    },
    Library {
        name: "file_upload",
        css: &["/static/css/media-picker.css"],
        js: &[
            local("/static/js/file-upload.js"),
            local("/static/js/media-picker.js"),
        ],
    },
    Library {
        name: "record_ref",
        css: &["/static/css/record-ref.css"],
        js: &[local("/static/js/record-ref.js")],
    },
    Library {
        name: "form_states",
        css: &[],
        js: &[local("/static/js/form-states.js")],
    },
    Library {
        name: "ai_assist",
        css: &["/static/css/ai-assist.css"],
        js: &[local("/static/js/ai-assist.js")],
    },
];

/// The library named `name`, if the kernel defines one.
pub fn library(name: &str) -> Option<&'static Library> {
    LIBRARIES.iter().find(|l| l.name == name)
}

/// The library a widget needs, if any.
fn widget_library(widget: &str) -> Option<&'static str> {
    match widget {
        "blocks" => Some("block_editor"),
        "compound" => Some("compound_field"),
        "file" => Some("file_upload"),
        "record_reference" => Some("record_ref"),
        _ => None,
    }
}

/// The libraries `form` needs, in a stable order and each once: `form`
/// itself, then those its widgets need, then those it names. A name no library
/// answers to is dropped.
pub fn libraries_for(form: &Form) -> Vec<&'static Library> {
    let mut names: Vec<&str> = vec!["form"];
    for (_, element) in form.all_elements() {
        if let ElementType::Widget { widget } = &element.element_type
            && let Some(name) = widget_library(widget)
        {
            names.push(name);
        }
        if element.attr("data-visible-when-field").is_some() {
            names.push("form_states");
        }
    }
    names.extend(form.libraries.iter().map(String::as_str));
    let mut out: Vec<&'static Library> = Vec::new();
    for name in names {
        if let Some(lib) = library(name)
            && !out.iter().any(|l| l.name == lib.name)
        {
            out.push(lib);
        }
    }
    out
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::form::FormElement;

    #[test]
    fn every_third_party_script_is_pinned_with_integrity() {
        for lib in LIBRARIES {
            for script in lib.js {
                if script.src.starts_with("https://") {
                    assert!(
                        script.integrity.is_some_and(|h| h.starts_with("sha384-")),
                        "{} has no integrity hash",
                        script.src
                    );
                    assert!(script.src.contains('@'), "{} is not pinned", script.src);
                } else {
                    assert!(script.src.starts_with("/static/js/"), "{}", script.src);
                }
            }
        }
    }

    #[test]
    fn a_blocks_widget_attaches_the_editor_once() {
        let form = Form::new("f")
            .element("a", FormElement::widget("blocks"))
            .element("b", FormElement::widget("blocks"));
        let names: Vec<&str> = libraries_for(&form).iter().map(|l| l.name).collect();
        assert_eq!(names, vec!["form", "block_editor"]);
    }

    #[test]
    fn an_unknown_library_name_is_dropped() {
        let mut form = Form::new("f");
        form.libraries = vec![
            "https://evil.example/x.js".to_string(),
            "ai_assist".to_string(),
        ];
        let names: Vec<&str> = libraries_for(&form).iter().map(|l| l.name).collect();
        assert_eq!(names, vec!["form", "ai_assist"]);
    }
}
