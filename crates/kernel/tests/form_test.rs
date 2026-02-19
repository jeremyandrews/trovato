#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Form API tests.

use trovato_kernel::form::{AjaxCommand, AjaxResponse, ElementType, Form, FormElement};

#[test]
fn test_form_creation() {
    let form = Form::new("test_form")
        .title("Test Form")
        .action("/submit")
        .element("name", FormElement::textfield().title("Name").required())
        .element("email", FormElement::textfield().title("Email"))
        .element("submit", FormElement::submit("Save").weight(100));

    assert_eq!(form.form_id, "test_form");
    assert_eq!(form.title, Some("Test Form".to_string()));
    assert_eq!(form.action, "/submit");
    assert_eq!(form.elements.len(), 3);
}

#[test]
fn test_form_element_types() {
    let textfield = FormElement::textfield().max_length(100);
    assert!(matches!(
        textfield.element_type,
        ElementType::Textfield {
            max_length: Some(100)
        }
    ));

    let textarea = FormElement::textarea(5);
    assert!(matches!(
        textarea.element_type,
        ElementType::Textarea { rows: 5 }
    ));

    let checkbox = FormElement::checkbox();
    assert!(matches!(checkbox.element_type, ElementType::Checkbox));

    let select = FormElement::select(vec![
        ("a".to_string(), "Option A".to_string()),
        ("b".to_string(), "Option B".to_string()),
    ]);
    assert!(matches!(
        select.element_type,
        ElementType::Select {
            multiple: false,
            ..
        }
    ));
}

#[test]
fn test_form_element_builder() {
    let element = FormElement::textfield()
        .title("Username")
        .description("Enter your username")
        .placeholder("user@example.com")
        .required()
        .weight(10)
        .max_length(50);

    assert_eq!(element.title, Some("Username".to_string()));
    assert_eq!(element.description, Some("Enter your username".to_string()));
    assert_eq!(element.placeholder, Some("user@example.com".to_string()));
    assert!(element.required);
    assert_eq!(element.weight, 10);
    assert!(matches!(
        element.element_type,
        ElementType::Textfield {
            max_length: Some(50)
        }
    ));
}

#[test]
fn test_form_sorted_elements() {
    let form = Form::new("test")
        .element("c", FormElement::textfield().weight(30))
        .element("a", FormElement::textfield().weight(10))
        .element("b", FormElement::textfield().weight(20));

    let sorted = form.sorted_elements();
    assert_eq!(sorted[0].0, "a");
    assert_eq!(sorted[1].0, "b");
    assert_eq!(sorted[2].0, "c");
}

#[test]
fn test_fieldset_element() {
    let fieldset = FormElement::fieldset()
        .title("Personal Information")
        .child("first_name", FormElement::textfield().title("First Name"))
        .child("last_name", FormElement::textfield().title("Last Name"));

    assert_eq!(fieldset.title, Some("Personal Information".to_string()));
    assert_eq!(fieldset.children.len(), 2);
    assert!(fieldset.children.contains_key("first_name"));
    assert!(fieldset.children.contains_key("last_name"));
}

#[test]
fn test_collapsible_fieldset() {
    let fieldset = FormElement::fieldset_collapsible(true);
    assert!(matches!(
        fieldset.element_type,
        ElementType::Fieldset {
            collapsible: true,
            collapsed: true
        }
    ));
}

#[test]
fn test_ajax_response_builder() {
    let response = AjaxResponse::new()
        .replace("#container", "<div>New content</div>")
        .append("#list", "<li>Item</li>")
        .add_class("#element", "active")
        .redirect("/success");

    assert_eq!(response.commands.len(), 4);

    match &response.commands[0] {
        AjaxCommand::Replace { selector, html } => {
            assert_eq!(selector, "#container");
            assert_eq!(html, "<div>New content</div>");
        }
        _ => panic!("expected Replace command"),
    }

    match &response.commands[1] {
        AjaxCommand::Append { selector, html } => {
            assert_eq!(selector, "#list");
            assert_eq!(html, "<li>Item</li>");
        }
        _ => panic!("expected Append command"),
    }

    match &response.commands[2] {
        AjaxCommand::AddClass { selector, class } => {
            assert_eq!(selector, "#element");
            assert_eq!(class, "active");
        }
        _ => panic!("expected AddClass command"),
    }

    match &response.commands[3] {
        AjaxCommand::Redirect { url } => {
            assert_eq!(url, "/success");
        }
        _ => panic!("expected Redirect command"),
    }
}

#[test]
fn test_ajax_command_serialization() {
    let cmd = AjaxCommand::Replace {
        selector: "#test".to_string(),
        html: "<p>content</p>".to_string(),
    };

    let json = serde_json::to_string(&cmd).unwrap();
    assert!(json.contains("replace"));
    assert!(json.contains("#test"));

    let parsed: AjaxCommand = serde_json::from_str(&json).unwrap();
    assert!(matches!(parsed, AjaxCommand::Replace { .. }));
}

#[test]
fn test_form_serialization() {
    let form = Form::new("test")
        .title("Test")
        .element("name", FormElement::textfield().title("Name"));

    let json = serde_json::to_string(&form).unwrap();
    assert!(json.contains("test"));
    assert!(json.contains("textfield"));
    assert!(json.contains("Name"));

    let parsed: Form = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.form_id, "test");
    assert_eq!(parsed.title, Some("Test".to_string()));
}

#[test]
fn test_element_type_name() {
    assert_eq!(
        ElementType::Textfield { max_length: None }.type_name(),
        "textfield"
    );
    assert_eq!(ElementType::Checkbox.type_name(), "checkbox");
    assert_eq!(
        ElementType::Submit {
            value: "Save".to_string()
        }
        .type_name(),
        "submit"
    );
    assert_eq!(ElementType::Textarea { rows: 5 }.type_name(), "textarea");
    assert_eq!(ElementType::Container.type_name(), "container");
}
