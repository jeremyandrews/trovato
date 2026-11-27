#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Every field type survives load, edit and save unchanged, on every edit path
//! (B125).
//!
//! The form pipeline (`content::form` and `content::submission`) read a stored
//! value tolerantly — a bare scalar or a `{"value": …}` wrapper, a reference
//! as a uuid or `{"target_id": …}` — but wrote back only the canonical shape.
//! So one save without a single change rewrote `{"value": "Barga"}` as
//! `"Barga"`, `{"target_id": …}` as a bare uuid, dropped a long text's
//! `summary`, turned a plain string body into `{value, format}`, and lost a
//! stored number held as a string. What a field held depended on which form
//! last saved it.
//!
//! These drive the real router. Each edit page is fetched and its form is
//! submitted unchanged, the way a browser submits it (a textarea's newlines as
//! CRLF, a number or date input's value sanitized, the first option of a select
//! with none selected); the JSON route gets back exactly what `/api/item/{id}`
//! returned. The stored `fields` must be byte for byte what they were, the new
//! revision must hold the same fields, and the item page and the API must
//! render the same before and after.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TestApp, run_test, shared_app};
use serde_json::{Value, json};
use uuid::Uuid;

const TYPE: &str = "b125_round_trip";
const TARGET: &str = "b125_round_trip_target";

async fn body_string(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("read body");
    String::from_utf8_lossy(&bytes).to_string()
}

/// One field of every type the kernel ships, plus multi value variants.
fn probe_fields() -> Vec<trovato_sdk::types::FieldDefinition> {
    use trovato_sdk::types::{FieldDefinition, FieldType};
    vec![
        FieldDefinition::new("f_text", FieldType::Text { max_length: None }).label("Text"),
        FieldDefinition::new("f_long", FieldType::TextLong).label("Long"),
        FieldDefinition::new("f_int", FieldType::Integer).label("Integer"),
        FieldDefinition::new("f_float", FieldType::Float).label("Float"),
        FieldDefinition::new("f_bool", FieldType::Boolean).label("Flag"),
        FieldDefinition::new("f_bool_off", FieldType::Boolean).label("Off flag"),
        FieldDefinition::new("f_date", FieldType::Date).label("Date"),
        FieldDefinition::new("f_email", FieldType::Email).label("Email"),
        FieldDefinition::new("f_ref", FieldType::RecordReference(TARGET.to_string()))
            .label("Reference"),
        FieldDefinition::new("f_file", FieldType::File).label("File"),
        FieldDefinition::new("f_blocks", FieldType::Blocks).label("Blocks"),
        {
            let mut compound = FieldDefinition::new(
                "f_compound",
                FieldType::Compound {
                    allowed_types: vec!["text".to_string()],
                    min_items: None,
                    max_items: None,
                },
            )
            .label("Compound");
            compound.settings = json!({"section_types": [{
                "machine_name": "text",
                "label": "Text",
                "fields": [{"field_name": "body", "field_type": "TextLong", "label": "Body", "required": true}]
            }]});
            compound
        },
        FieldDefinition::new("f_page", FieldType::PageBuilder).label("Layout"),
        FieldDefinition::new("f_tags", FieldType::Text { max_length: None })
            .label("Tags")
            .cardinality(-1),
        FieldDefinition::new("f_refs", FieldType::RecordReference(TARGET.to_string()))
            .label("References")
            .cardinality(-1),
        FieldDefinition::new("f_nums", FieldType::Integer)
            .label("Numbers")
            .cardinality(3),
        FieldDefinition::new("f_link", FieldType::Link).label("Link"),
        FieldDefinition::new("f_list", FieldType::List)
            .label("List")
            .allowed_values(&[("gold", "Gold"), ("silver", "Silver"), ("bronze", "Bronze")]),
        FieldDefinition::new("f_list_multi", FieldType::List)
            .label("List (multi)")
            .cardinality(-1)
            .allowed_values(&[("gold", "Gold"), ("silver", "Silver"), ("bronze", "Bronze")]),
        FieldDefinition::new("f_tel", FieldType::Telephone).label("Telephone"),
        FieldDefinition::new("f_tel_multi", FieldType::Telephone)
            .label("Telephone (multi)")
            .cardinality(-1),
    ]
}

async fn ensure_types(app: &TestApp) {
    use trovato_sdk::types::{FieldDefinition, FieldType};
    for (name, label, fields) in [
        (
            TARGET,
            "B125 Target",
            vec![FieldDefinition::new(
                "f_note",
                FieldType::Text { max_length: None },
            )],
        ),
        (TYPE, "B125 Round Trip", probe_fields()),
    ] {
        let settings = json!({ "fields": serde_json::to_value(&fields).unwrap() });
        sqlx::query(
            "INSERT INTO item_type (type, label, description, has_title, title_label, plugin, settings) \
             VALUES ($1, $2, '', true, 'Title', 'b125_test', $3) \
             ON CONFLICT (type) DO UPDATE SET settings = EXCLUDED.settings",
        )
        .bind(name)
        .bind(label)
        .bind(&settings)
        .execute(&app.db)
        .await
        .unwrap();
        app.state
            .content_types()
            .replace(name, label, None, settings)
            .await
            .ok();
    }
}

fn admin_ctx(uid: Uuid) -> trovato_kernel::tap::UserContext {
    trovato_kernel::tap::UserContext::administrator(
        uid,
        vec!["administer site".to_string(), "publish content".to_string()],
    )
}

async fn create(app: &TestApp, uid: Uuid, item_type: &str, title: &str, fields: Value) -> Uuid {
    app.state
        .items()
        .create(
            trovato_kernel::models::CreateItem {
                item_type: item_type.to_string(),
                title: title.to_string(),
                author_id: uid,
                status: Some(1),
                promote: None,
                sticky: None,
                fields: Some(fields),
                stage_id: None,
                language: None,
                log: None,
            },
            &admin_ctx(uid),
        )
        .await
        .expect("seed item")
        .id
}

async fn file_row(app: &TestApp, owner: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO file_managed (id, owner_id, filename, uri, filemime, filesize, status, created, changed) \
         VALUES ($1, $2, 'b125.txt', $3, 'text/plain', 1, 1, 0, 0)",
    )
    .bind(id)
    .bind(owner)
    .bind(format!("local://b125/{}.txt", id.simple()))
    .execute(&app.db)
    .await
    .unwrap();
    id
}

/// The layout builder page: standard components, one nested in a zone.
fn landing_page() -> Value {
    json!({
        "root": { "title": "Landing" },
        "content": [
            { "type": "Hero", "props": { "id": "Hero-1", "title": "Round trip", "subtitle": "Saved unchanged" } },
            { "type": "Columns", "props": { "id": "Columns-1", "layout": "1/2+1/2", "gapSize": "large" },
              "zones": {
                "zone-0": [ { "type": "TextBlock", "props": { "id": "TextBlock-1", "content": "Standard **text**." } } ],
                "zone-1": []
              } }
        ]
    })
}

/// The stored shapes to probe: the canonical shape the form writes, and the
/// shapes plugins, importers and older kernels wrote, which the form reads.
fn cases(target: Uuid, other: Uuid, file: Uuid) -> Vec<(&'static str, Value)> {
    vec![
        (
            "canonical",
            json!({
                "f_text": "Plain & <text> \"quoted\"",
                "f_long": {"value": "<p>Body</p>\nline two\n", "format": "full_html"},
                "f_int": 42,
                "f_float": 1.5,
                "f_bool": true,
                "f_bool_off": false,
                "f_date": "2026-11-22",
                "f_email": "ed@example.com",
                "f_ref": target.to_string(),
                "f_file": file.to_string(),
                "f_blocks": [{"type": "paragraph", "weight": 0, "data": {"text": "Hello"}}],
                "f_compound": {"sections": [{"type": "text", "weight": 0, "data": {"body": {"value": "Hi", "format": "plain_text"}}}]},
                "f_page": landing_page(),
                "f_tags": ["one", "two"],
                "f_refs": [target.to_string(), other.to_string()],
                "f_nums": [1, 2],
                "f_link": {"uri": format!("/item/{target}"), "title": "Target one", "new_tab": true},
                "f_list": "gold",
                "f_list_multi": ["gold", "silver"],
                "f_tel": {"value": "+39 0583 123456", "e164": "+390583123456"},
                "f_tel_multi": [
                    {"value": "+39 0583 123456", "e164": "+390583123456"},
                    {"value": "0583 123456"}
                ],
                "plugin_meta": {"written_by": "a plugin"},
            }),
        ),
        (
            "wrapped",
            json!({
                "f_text": {"value": "Barga"},
                "f_long": {"value": "Wrapped body", "format": "filtered_html", "summary": "A summary"},
                "f_int": {"value": 2},
                "f_float": 2,
                "f_bool": {"value": true},
                "f_date": "2026-01-01T10:00:00Z",
                "f_email": {"value": "x@example.com"},
                "f_ref": {"target_id": target.to_string(), "target_type": TARGET},
                "f_tags": "single",
                "f_refs": [{"target_id": target.to_string()}, {"target_id": other.to_string()}],
                "f_nums": 7,
                "f_link": {"uri": "https://example.org/a?b#c", "title": "External"},
                "f_list": "silver",
                "f_tel": {"value": "0583 123456"},
            }),
        ),
        (
            "legacy",
            json!({
                "f_text": "line\nbreak",
                "f_long": "Plain string body\nwith two lines",
                "f_int": "7",
                "f_float": "2.50",
                "f_bool": "1",
                "f_bool_off": "0",
                "f_date": 1_763_769_600,
                "f_email": " padded@example.com ",
                "f_nums": ["4", 5],
                "f_tel": "0039 0583 123456",
            }),
        ),
    ]
}

// --- A browser, as far as a form submission goes -------------------------

fn decode(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&#x2F;", "/")
        .replace("&#47;", "/")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The attributes of one start tag, `<name a="b" c>` (values decoded).
fn attributes(tag: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    let body = tag.trim_start_matches('<');
    let mut rest = body[body.find(char::is_whitespace).unwrap_or(body.len())..].trim_start();
    while !rest.is_empty() && !rest.starts_with('>') && !rest.starts_with("/>") {
        let end = rest
            .find(|c: char| c == '=' || c.is_whitespace() || c == '>')
            .unwrap_or(rest.len());
        let name = rest[..end].to_ascii_lowercase();
        rest = &rest[end..];
        if let Some(after) = rest.strip_prefix("=\"") {
            let close = after.find('"').unwrap_or(after.len());
            out.push((name, Some(decode(&after[..close]))));
            rest = after[close..].trim_start_matches('"').trim_start();
        } else {
            out.push((name, None));
            rest = rest.trim_start();
        }
    }
    out
}

fn attr<'a>(attrs: &'a [(String, Option<String>)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_deref().unwrap_or(""))
}

/// The HTML `value sanitization algorithm` for the input types the forms use.
fn sanitize(input_type: &str, value: &str) -> String {
    match input_type {
        "number" => {
            if value.parse::<f64>().is_ok_and(f64::is_finite) {
                value.to_string()
            } else {
                String::new()
            }
        }
        "date" => {
            let b = value.as_bytes();
            let ok = value.len() == 10
                && b[4] == b'-'
                && b[7] == b'-'
                && value
                    .chars()
                    .enumerate()
                    .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
            if ok { value.to_string() } else { String::new() }
        }
        "email" => value.replace(['\r', '\n'], "").trim().to_string(),
        "hidden" => value.to_string(),
        _ => value.replace(['\r', '\n'], ""),
    }
}

/// What a browser posts for the `<form>` posting to `action`, unchanged:
/// every enabled named control, a checkbox only when checked, a select's
/// selected option or else its first, a textarea's text with one leading
/// newline dropped and its newlines sent as CRLF.
fn browser_submission(html: &str, action: &str) -> Vec<(String, String)> {
    let escaped = action.replace('/', "&#x2F;");
    let start = html
        .find(&format!("action=\"{action}\""))
        .or_else(|| html.find(&format!("action=\"{escaped}\"")))
        .unwrap_or_else(|| panic!("no form posting to {action}"));
    let start = html[..start].rfind("<form").unwrap();
    let end = start + html[start..].find("</form>").unwrap();
    let form = &html[start..end];

    let mut pairs = Vec::new();
    let mut at = 0;
    while let Some(lt) = form[at..].find('<') {
        let pos = at + lt;
        let close = pos + form[pos..].find('>').unwrap();
        let tag = &form[pos..=close];
        at = close + 1;
        let lower = tag.to_ascii_lowercase();
        let attrs = attributes(tag);
        let Some(name) = attr(&attrs, "name").map(str::to_string) else {
            continue;
        };
        if attr(&attrs, "disabled").is_some() {
            continue;
        }
        if lower.starts_with("<input") {
            let input_type = attr(&attrs, "type").unwrap_or("text").to_ascii_lowercase();
            let value = attr(&attrs, "value").unwrap_or("");
            match input_type.as_str() {
                "checkbox" | "radio" => {
                    if attr(&attrs, "checked").is_some() {
                        pairs.push((
                            name,
                            if value.is_empty() { "on" } else { value }.to_string(),
                        ));
                    }
                }
                "submit" | "button" | "file" => {}
                other => pairs.push((name, sanitize(other, value))),
            }
        } else if lower.starts_with("<textarea") {
            let text_end = at + form[at..].find("</textarea>").unwrap();
            let text = decode(&form[at..text_end]);
            let text = text.strip_prefix('\n').unwrap_or(&text);
            pairs.push((name, text.replace("\r\n", "\n").replace('\n', "\r\n")));
            at = text_end;
        } else if lower.starts_with("<select") {
            let select_end = at + form[at..].find("</select>").unwrap();
            let options = &form[at..select_end];
            let mut first = None;
            let mut chosen = None;
            let mut o = 0;
            while let Some(i) = options[o..].find("<option") {
                let p = o + i;
                let c = p + options[p..].find('>').unwrap();
                let oattrs = attributes(&options[p..=c]);
                let value = attr(&oattrs, "value").unwrap_or("").to_string();
                if first.is_none() {
                    first = Some(value.clone());
                }
                if attr(&oattrs, "selected").is_some() {
                    chosen = Some(value);
                }
                o = c + 1;
            }
            pairs.push((name, chosen.or(first).unwrap_or_default()));
            at = select_end;
        }
    }
    pairs
}

fn urlencode(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

async fn get(app: &TestApp, cookies: &str, uri: &str) -> String {
    let page = app
        .request_with_cookies(
            Request::builder().uri(uri).body(Body::empty()).unwrap(),
            cookies,
        )
        .await;
    assert_eq!(page.status(), StatusCode::OK, "GET {uri}");
    body_string(page).await
}

async fn post(
    app: &TestApp,
    cookies: &str,
    uri: &str,
    content_type: &str,
    csrf: Option<&str>,
    body: String,
) -> (StatusCode, String) {
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, content_type);
    if let Some(token) = csrf {
        request = request.header("X-CSRF-Token", token);
    }
    let response = app
        .request_with_cookies(request.body(Body::from(body)).unwrap(), cookies)
        .await;
    let status = response.status();
    (status, body_string(response).await)
}

/// The stored `fields`, as Postgres prints them: the byte for byte form.
async fn stored_text(app: &TestApp, id: Uuid) -> String {
    sqlx::query_scalar("SELECT fields::text FROM item WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await
        .unwrap()
}

/// The newest revision's fields, and how many revisions there are.
async fn newest_revision(app: &TestApp, id: Uuid) -> (String, i64) {
    sqlx::query_as(
        "SELECT r.fields::text, (SELECT count(*) FROM item_revision WHERE item_id = $1) \
         FROM item_revision r WHERE r.item_id = $1 ORDER BY r.created DESC, r.id DESC LIMIT 1",
    )
    .bind(id)
    .fetch_one(&app.db)
    .await
    .unwrap()
}

/// The parts of a page that change on every request, blanked: CSRF tokens,
/// form build ids, CSP nonces.
fn scrub(html: &str) -> String {
    let mut out = html.to_string();
    for marker in [
        "name=\"_token\" value=\"",
        "name=\"_csrf\" value=\"",
        "name=\"_form_build_id\" value=\"",
        "nonce=\"",
        "name=\"csrf-token\" content=\"",
    ] {
        let mut from = 0;
        while let Some(i) = out[from..].find(marker) {
            let s = from + i + marker.len();
            let e = s + out[s..].find('"').unwrap_or(0);
            out.replace_range(s..e, "");
            from = s;
        }
    }
    out
}

/// The display layers for one item: the full page (field formatters and the
/// layout builder), and the API document without its `changed` stamp.
async fn display_layers(app: &TestApp, cookies: &str, id: Uuid) -> (String, Value) {
    let page = scrub(&get(app, cookies, &format!("/item/{id}")).await);
    let mut api: Value =
        serde_json::from_str(&get(app, cookies, &format!("/api/item/{id}")).await).unwrap();
    api.as_object_mut().unwrap().remove("changed");
    (page, api)
}

/// **The goal.** For every field type, every stored shape and every edit path,
/// a save without changes leaves the stored JSON byte for byte as it was, the
/// revision it writes holds the same fields, and the item page (field
/// formatters and the layout builder) and the API render the same.
#[test]
fn an_unchanged_save_is_a_no_op_for_every_field_type_and_path() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let name = common::username("b125rt");
        let cookies = app
            .create_and_login_admin(
                &name,
                "correct-horse-battery-staple",
                &format!("{name}@test.local"),
            )
            .await;
        let uid = common::user_id_of(app, &name).await;
        let target = create(app, uid, TARGET, "B125 target one", json!({})).await;
        let other = create(app, uid, TARGET, "B125 target two", json!({})).await;
        let file = file_row(app, uid).await;

        let mut failures = Vec::new();
        for (label, fields) in cases(target, other, file) {
            for path in ["html", "admin", "json"] {
                let id = create(
                    app,
                    uid,
                    TYPE,
                    &format!("B125 {label} {path}"),
                    fields.clone(),
                )
                .await;
                let before = stored_text(app, id).await;
                let (page_before, api_before) = display_layers(app, &cookies, id).await;

                let edit = format!("/item/{id}/edit");
                let (status, text) = match path {
                    "json" => {
                        let html = get(app, &cookies, &edit).await;
                        let token = browser_submission(&html, &edit)
                            .into_iter()
                            .find(|(k, _)| k == "_token")
                            .unwrap()
                            .1;
                        let api: Value = serde_json::from_str(
                            &get(app, &cookies, &format!("/api/item/{id}")).await,
                        )
                        .unwrap();
                        let body = json!({ "title": api["title"], "fields": api["fields"] });
                        post(
                            app,
                            &cookies,
                            &edit,
                            "application/json",
                            Some(&token),
                            body.to_string(),
                        )
                        .await
                    }
                    _ => {
                        let uri = if path == "admin" {
                            format!("/admin/content/{id}/edit")
                        } else {
                            edit.clone()
                        };
                        let html = get(app, &cookies, &uri).await;
                        assert!(
                            !html.contains("[object"),
                            "{label}/{path}: the form shows an object as text"
                        );
                        let pairs = browser_submission(&html, &uri);
                        post(
                            app,
                            &cookies,
                            &uri,
                            "application/x-www-form-urlencoded",
                            None,
                            urlencode(&pairs),
                        )
                        .await
                    }
                };
                if !(status.is_success() || status.is_redirection()) {
                    let why = text
                        .find("form__errors")
                        .map_or(&text[..text.len().min(600)], |i| {
                            &text[i..text.len().min(i + 600)]
                        });
                    println!("FAIL {label:9} {path:5} save refused ({status}): {why}");
                    failures.push(format!("{label}/{path}: save refused ({status}): {why}"));
                    continue;
                }

                let after = stored_text(app, id).await;
                let (revision, revisions) = newest_revision(app, id).await;
                let (page_after, api_after) = display_layers(app, &cookies, id).await;
                let mut wrong = Vec::new();
                if after != before {
                    let b: Value = serde_json::from_str(&before).unwrap();
                    let a: Value = serde_json::from_str(&after).unwrap();
                    for key in b
                        .as_object()
                        .unwrap()
                        .keys()
                        .chain(a.as_object().unwrap().keys())
                    {
                        if b.get(key) != a.get(key) {
                            wrong.push(format!(
                                "  {key}: {} -> {}",
                                b.get(key).unwrap_or(&Value::Null),
                                a.get(key).unwrap_or(&Value::Null)
                            ));
                        }
                    }
                    wrong.sort();
                    wrong.dedup();
                    if wrong.is_empty() {
                        wrong.push(format!("  bytes differ: {before} -> {after}"));
                    }
                }
                if revision != before {
                    wrong.push(format!(
                        "  newest revision ({revisions} in all) holds other fields: {revision}"
                    ));
                }
                if page_after != page_before {
                    let at = page_before
                        .char_indices()
                        .zip(page_after.chars())
                        .find(|((_, a), b)| a != b)
                        .map_or(0, |((i, _), _)| i);
                    let from = page_before.floor_char_boundary(at.saturating_sub(120));
                    wrong.push(format!(
                        "  the item page renders differently at byte {at}: …{}… became …{}…",
                        &page_before[from..page_before
                            .floor_char_boundary((at + 120).min(page_before.len()))],
                        &page_after[from..page_after
                            .floor_char_boundary((at + 120).min(page_after.len()))]
                    ));
                }
                if api_after != api_before {
                    let keys: Vec<&String> = api_before
                        .as_object()
                        .unwrap()
                        .keys()
                        .filter(|k| api_before.get(k.as_str()) != api_after.get(k.as_str()))
                        .collect();
                    wrong.push(format!(
                        "  the API document differs in {keys:?}: {} -> {}",
                        keys.iter()
                            .map(|k| api_before[k.as_str()].to_string())
                            .collect::<Vec<_>>()
                            .join(", "),
                        keys.iter()
                            .map(|k| api_after[k.as_str()].to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                if wrong.is_empty() {
                    println!(
                        "ok   {label:9} {path:5} stored fields identical ({} bytes), revision identical, page and API identical",
                        after.len()
                    );
                } else {
                    println!("FAIL {label:9} {path:5}\n{}", wrong.join("\n"));
                    failures.push(format!("{label}/{path}:\n{}", wrong.join("\n")));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} round trips changed data:\n{}",
            failures.len(),
            failures.join("\n")
        );
    });
}

/// A CSRF token for a JSON request, from the edit page, as a client gets one.
async fn token_for(app: &TestApp, cookies: &str, edit: &str) -> String {
    let html = get(app, cookies, edit).await;
    browser_submission(&html, edit)
        .into_iter()
        .find(|(k, _)| k == "_token")
        .unwrap()
        .1
}

/// Set one posted pair, replacing every value already posted under `name`.
fn set(pairs: &mut Vec<(String, String)>, name: &str, value: &str) {
    pairs.retain(|(k, _)| k != name);
    pairs.push((name.to_string(), value.to_string()));
}

async fn stored(app: &TestApp, id: Uuid) -> Value {
    serde_json::from_str(&stored_text(app, id).await).unwrap()
}

/// A changed value keeps the shape it was stored in: a long text edited
/// through the admin form keeps its format and its `summary`, a wrapped
/// scalar stays wrapped, a reference object keeps its other keys, and only
/// the field that changed is written.
#[test]
fn a_changed_value_keeps_its_stored_shape_and_format() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let name = common::username("b125chg");
        let cookies = app
            .create_and_login_admin(
                &name,
                "correct-horse-battery-staple",
                &format!("{name}@test.local"),
            )
            .await;
        let uid = common::user_id_of(app, &name).await;
        let target = create(app, uid, TARGET, "B125 change one", json!({})).await;
        let other = create(app, uid, TARGET, "B125 change two", json!({})).await;

        for uri_of in [
            (|id: Uuid| format!("/admin/content/{id}/edit")) as fn(Uuid) -> String,
            |id: Uuid| format!("/item/{id}/edit"),
        ] {
            let id = create(
                app,
                uid,
                TYPE,
                "B125 change",
                json!({
                    "f_long": {"value": "<p>Old</p>", "format": "full_html", "summary": "Kept"},
                    "f_text": {"value": "Barga"},
                    "f_ref": {"target_id": target.to_string(), "target_type": TARGET, "title": "B125 change one"},
                    "f_int": 3,
                }),
            )
            .await;
            let uri = uri_of(id);
            let html = get(app, &cookies, &uri).await;
            assert!(
                html.contains("&lt;p&gt;Old&lt;&#x2F;p&gt;</textarea>"),
                "{uri}: the body is in a textarea"
            );
            assert!(
                html.contains(r#"<option value="full_html" selected>"#),
                "{uri}: its format is selected"
            );
            let mut pairs = browser_submission(&html, &uri);
            set(&mut pairs, "f_long", "<p>New</p>\r\nsecond line");
            set(&mut pairs, "f_text", "Lucca");
            set(&mut pairs, "f_ref", &other.to_string());
            let (status, text) = post(
                app,
                &cookies,
                &uri,
                "application/x-www-form-urlencoded",
                None,
                urlencode(&pairs),
            )
            .await;
            assert!(
                status.is_success() || status.is_redirection(),
                "{uri}: {status} {text}"
            );

            let fields = stored(app, id).await;
            assert_eq!(
                fields["f_long"],
                json!({"value": "<p>New</p>\nsecond line", "format": "full_html", "summary": "Kept"}),
                "{uri}"
            );
            assert_eq!(fields["f_text"], json!({"value": "Lucca"}), "{uri}");
            assert_eq!(
                fields["f_ref"],
                json!({"target_id": other.to_string(), "target_type": TARGET}),
                "{uri}: the new target, without the old target's title"
            );
            assert_eq!(fields["f_int"], json!(3), "{uri}");
            assert!(
                fields.get("f_blocks").is_none(),
                "{uri}: an absent field stays absent: {fields}"
            );
            println!("ok   {uri}: changed values written in their stored shapes: {fields}");
        }
    });
}

/// The JSON edit route reads a value the way the form does: a shape the field
/// type cannot read is a 400 naming the field, a text format the writer may
/// not use is refused, and a value sent back exactly as stored is accepted
/// whatever its shape.
#[test]
fn the_json_route_checks_a_changed_value_by_its_field_type() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let name = common::username("b125json");
        let cookies = app
            .create_and_login_admin(
                &name,
                "correct-horse-battery-staple",
                &format!("{name}@test.local"),
            )
            .await;
        let uid = common::user_id_of(app, &name).await;
        let id = create(
            app,
            uid,
            TYPE,
            "B125 json",
            json!({"f_text": {"odd": "plugin shape"}, "f_int": 1}),
        )
        .await;
        let edit = format!("/item/{id}/edit");

        for (fields, why) in [
            (json!({"f_int": "seven"}), "Integer"),
            (json!({"f_bool": {"a": 1}}), "Flag"),
            (json!({"f_blocks": {"not": "a list"}}), "Blocks"),
            (
                json!({"f_nums": [1, 2, 3, 4]}),
                "Numbers allows at most 3 values",
            ),
            (
                json!({"f_long": {"value": "x", "format": "no_such_format"}}),
                "no_such_format",
            ),
        ] {
            let token = token_for(app, &cookies, &edit).await;
            let body = json!({"fields": fields});
            let (status, text) = post(
                app,
                &cookies,
                &edit,
                "application/json",
                Some(&token),
                body.to_string(),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{fields}: {text}");
            assert!(text.contains(why), "{fields}: the refusal names it: {text}");
            println!("ok   JSON {fields} refused: {text}");
        }
        let before = stored_text(app, id).await;
        assert!(before.contains("plugin shape"), "nothing was written");

        // The stored odd shape, sent back as it is, with a real change beside it.
        let token = token_for(app, &cookies, &edit).await;
        let body = json!({"fields": {"f_text": {"odd": "plugin shape"}, "f_int": {"value": 2}}});
        let (status, text) = post(
            app,
            &cookies,
            &edit,
            "application/json",
            Some(&token),
            body.to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert_eq!(stored(app, id).await["f_int"], json!({"value": 2}));
        println!("ok   JSON with the stored shape sent back unchanged accepted");
    });
}

/// An editor who may not use a long text's format sees it disabled, not as a
/// selector that cannot show its format, and an unchanged save keeps it.
#[test]
fn a_long_text_in_a_format_the_editor_may_not_use_is_kept() {
    run_test(async {
        let app = shared_app().await;
        ensure_types(app).await;
        let admin = common::username("b125fmt");
        app.create_and_login_admin(
            &admin,
            "correct-horse-battery-staple",
            &format!("{admin}@test.local"),
        )
        .await;
        let uid = common::user_id_of(app, &admin).await;
        let id = create(
            app,
            uid,
            TYPE,
            "B125 locked format",
            json!({"f_long": {"value": "<p>Full</p>", "format": "full_html"}, "f_text": "before"}),
        )
        .await;

        let name = common::username("b125ed");
        app.create_test_user(&name, "test-password-123", &format!("{name}@test.local"))
            .await;
        let editor = common::user_id_of(app, &name).await;
        common::grant_via_role(app, editor, &["edit any content", "access content"]).await;
        let cookies = app.login(&name, "test-password-123").await;

        let uri = format!("/item/{id}/edit");
        let html = get(app, &cookies, &uri).await;
        assert!(
            html.contains("a text format you may not use"),
            "the editor is told why"
        );
        let mut pairs = browser_submission(&html, &uri);
        assert!(
            !pairs
                .iter()
                .any(|(k, _)| k == "f_long" || k == "f_long_format"),
            "disabled, not posted"
        );
        set(&mut pairs, "f_text", "after");
        let (status, text) = post(
            app,
            &cookies,
            &uri,
            "application/x-www-form-urlencoded",
            None,
            urlencode(&pairs),
        )
        .await;
        assert!(
            status.is_success() || status.is_redirection(),
            "{status} {text}"
        );
        let fields = stored(app, id).await;
        assert_eq!(
            fields["f_long"],
            json!({"value": "<p>Full</p>", "format": "full_html"})
        );
        assert_eq!(fields["f_text"], json!("after"));
        println!("ok   a full_html body survives an editor's save: {fields}");
    });
}
