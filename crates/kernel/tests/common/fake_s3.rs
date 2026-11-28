//! A minimal in-process S3-compatible HTTP server, standing in for MinIO on a
//! local run with no Docker (B291). CI always runs real MinIO
//! (`scripts/ci-start-minio.sh`) and sets `TROVATO_TEST_S3_ENDPOINT`, so the
//! tests that call [`ensure`] prefer the real store and never start this one
//! there; it exists only so the S3 backed tests still run something real on a
//! workstation or a sandbox without a container runtime.
//!
//! Included by path (`#[path = "common/fake_s3.rs"] mod fake_s3;`) from the
//! tests that need it, so they do not pull in the whole shared harness.
//!
//! Implements just enough of the S3 API for `object_store`'s AWS client: PUT,
//! GET (including a single `Range`), HEAD and DELETE of an object, path style
//! addressing, one bucket. The client still signs every request (SigV4);
//! this server does not check the signature, since it is reachable only from
//! this one process on the loopback address.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path as AxumPath, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

#[derive(Clone, Default)]
struct Store(Arc<Mutex<HashMap<String, Vec<u8>>>>);

type AppState = (Store, String);

fn object_headers(len: usize) -> [(header::HeaderName, String); 4] {
    [
        (header::ETAG, "\"fake-s3-etag\"".to_string()),
        (header::LAST_MODIFIED, chrono::Utc::now().to_rfc2822()),
        (header::CONTENT_LENGTH, len.to_string()),
        (header::ACCEPT_RANGES, "bytes".to_string()),
    ]
}

async fn put_object(
    State((store, expected)): State<AppState>,
    AxumPath((bucket, key)): AxumPath<(String, String)>,
    body: Bytes,
) -> Response {
    if bucket != expected {
        return StatusCode::NOT_FOUND.into_response();
    }
    let len = body.len();
    store.0.lock().unwrap().insert(key, body.to_vec());
    (StatusCode::OK, object_headers(len)).into_response()
}

async fn head_object(
    State((store, expected)): State<AppState>,
    AxumPath((bucket, key)): AxumPath<(String, String)>,
) -> Response {
    if bucket != expected {
        return StatusCode::NOT_FOUND.into_response();
    }
    match store.0.lock().unwrap().get(&key).map(Vec::len) {
        Some(len) => (StatusCode::OK, object_headers(len)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn delete_object(
    State((store, expected)): State<AppState>,
    AxumPath((bucket, key)): AxumPath<(String, String)>,
) -> Response {
    if bucket == expected {
        store.0.lock().unwrap().remove(&key);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `POST /{bucket}?delete`: the S3 Bulk Delete API (`DeleteObjects`), which
/// `object_store`'s `ObjectStore::delete` default implementation uses instead
/// of a plain per-object `DELETE` (real S3 and MinIO both support it; this
/// fake must too, or every `delete()` here would silently 404 and do
/// nothing). The request and response are both a minimal slice of the real
/// XML: just the `<Key>` elements in, an `<Deleted><Key>` for each key out,
/// enough for `object_store`'s own response parsing, no XML parser needed for
/// a payload this simple.
async fn bulk_delete(
    State((store, expected)): State<AppState>,
    AxumPath(bucket): AxumPath<String>,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Response {
    if bucket != expected || !query.is_some_and(|q| q.contains("delete")) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let keys = extract_keys(&String::from_utf8_lossy(&body));
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
    );
    {
        let mut map = store.0.lock().unwrap();
        for key in &keys {
            map.remove(key);
            xml.push_str("<Deleted><Key>");
            xml.push_str(&escape_xml(key));
            xml.push_str("</Key></Deleted>");
        }
    }
    xml.push_str("</DeleteResult>");
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/xml".to_string())],
        xml,
    )
        .into_response()
}

/// The text of every `<Key>...</Key>` element in a Bulk Delete request body,
/// unescaped. Not a general XML parser: this fake only ever receives the one
/// shape `object_store` writes (`<Delete><Object><Key>k</Key></Object>...`).
fn extract_keys(xml: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<Key>") {
        rest = &rest[start + "<Key>".len()..];
        let Some(end) = rest.find("</Key>") else {
            break;
        };
        keys.push(unescape_xml(&rest[..end]));
        rest = &rest[end + "</Key>".len()..];
    }
    keys
}

fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

async fn get_object(
    State((store, expected)): State<AppState>,
    AxumPath((bucket, key)): AxumPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    // Not real SigV4 verification (this stand-in has no secret to check a
    // signature against), but enough to keep the one test that proves a
    // bucket answers nothing to an unsigned request honest: `object_store`
    // always sends `Authorization`, so its absence means a plain,
    // credential-free client, which a private bucket must refuse.
    if !headers.contains_key(header::AUTHORIZATION) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if bucket != expected {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(data) = store.0.lock().unwrap().get(&key).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let total = data.len() as u64;
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(trovato_kernel::file::parse_range_header);
    match range {
        None => (StatusCode::OK, object_headers(data.len()), data).into_response(),
        Some(range) => match trovato_kernel::file::range::resolve_range(Some(range), total) {
            Ok((start, end)) => {
                let slice = data[start as usize..end as usize].to_vec();
                let mut builder = axum::http::Response::builder().status(StatusCode::PARTIAL_CONTENT);
                for (name, value) in object_headers(slice.len()) {
                    builder = builder.header(name, value);
                }
                builder = builder.header(
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{}/{total}", end.saturating_sub(1)),
                );
                builder.body(axum::body::Body::from(slice)).unwrap()
            }
            Err(_) => (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{total}"))],
            )
                .into_response(),
        },
    }
}

/// A long-lived runtime the fake server runs on, kept for the rest of the
/// process: the listener and its accept loop must outlive whichever test
/// (and whichever of that test's own runtimes) first started it.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Runtime::new().expect("failed to build the fake S3 server's runtime")
    })
}

/// Start the fake server (once per process) and return its
/// `(base_url, bucket)`, setting `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`
/// when unset so `object_store` has something to sign with.
///
/// Safe to call from inside another async runtime (a `#[tokio::test]` body,
/// or one already driven by `common::run_test`): the server itself starts on
/// a dedicated OS thread blocking on its own runtime, never the caller's.
pub fn ensure(bucket: &str) -> (String, String) {
    static SERVER: OnceLock<(String, String)> = OnceLock::new();
    SERVER
        .get_or_init(|| {
            if std::env::var_os("AWS_ACCESS_KEY_ID").is_none() {
                // SAFETY: set once, before any other thread in this test
                // binary reads it (this runs inside `OnceLock::get_or_init`,
                // and every caller blocks on the same lock until it returns).
                unsafe { std::env::set_var("AWS_ACCESS_KEY_ID", "fake-s3-test-key") };
            }
            if std::env::var_os("AWS_SECRET_ACCESS_KEY").is_none() {
                // SAFETY: see above.
                unsafe { std::env::set_var("AWS_SECRET_ACCESS_KEY", "fake-s3-test-secret") };
            }
            let owned_bucket = bucket.to_string();
            let for_server = owned_bucket.clone();
            let handle = runtime().handle().clone();
            let base_url = std::thread::spawn(move || handle.block_on(start(for_server)))
                .join()
                .expect("fake S3 server start thread panicked");
            (base_url, owned_bucket)
        })
        .clone()
}

async fn start(bucket: String) -> String {
    let store = Store::default();
    let app = Router::new()
        .route(
            "/{bucket}/{*key}",
            get(get_object)
                .put(put_object)
                .head(head_object)
                .delete(delete_object),
        )
        .route("/{bucket}", post(bulk_delete))
        .with_state((store, bucket));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake S3 listener");
    let addr = listener.local_addr().expect("fake S3 local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}
