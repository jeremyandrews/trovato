//! An in-process, S3-compatible object store for tests: no MinIO binary and
//! no Docker are available in this sandbox (`dl.min.io` returns 410 Gone;
//! there is no `docker` on PATH), so this stands in for "the S3 test server"
//! the backend tests run against.
//!
//! Speaks just enough of the S3 REST API for
//! `trovato_kernel::file::S3Storage`: path-style PUT/GET/HEAD/DELETE, and
//! query-string (presigned) GET. It verifies SigV4 itself, independently
//! reconstructing the expected signature from the request it actually
//! received (method, path, query, headers, body) rather than trusting the
//! client — the same thing a real S3-compatible store does, and the reason
//! a bad or missing signature gets a real 403 here instead of the mock just
//! always accepting whatever arrives.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{OriginalUri, Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use trovato_kernel::file::sigv4::{self, Credentials, SigningRequest};

/// Fixed test credentials, shared between the client ([`S3Settings`]) and
/// this server's own verification.
pub const ACCESS_KEY_ID: &str = "mockAccessKeyId";
pub const SECRET_ACCESS_KEY: &str = "mockSecretAccessKey1234567890";
pub const REGION: &str = "us-east-1";
pub const BUCKET: &str = "test-bucket";

fn credentials() -> Credentials {
    Credentials {
        access_key_id: ACCESS_KEY_ID.to_string(),
        secret_access_key: SECRET_ACCESS_KEY.to_string(),
    }
}

#[derive(Default)]
struct Store {
    objects: HashMap<String, Vec<u8>>,
}

/// A running mock S3 server. Dropping this does not stop it (the accept
/// task is detached, like `SmtpSink`'s); tests are short-lived processes, so
/// that is the same tradeoff the rest of this test suite already makes.
pub struct MockS3 {
    pub port: u16,
    store: Arc<Mutex<Store>>,
}

impl MockS3 {
    /// Bind an ephemeral loopback port and start accepting.
    pub async fn start() -> Self {
        let store = Arc::new(Mutex::new(Store::default()));
        let app = axum::Router::new()
            .fallback(any(handle))
            .with_state(store.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a port for the mock S3 server");
        let port = listener
            .local_addr()
            .expect("read the mock S3 server's local address")
            .port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { port, store }
    }

    pub fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The bytes stored at `key` (without the bucket), bypassing HTTP and
    /// signing entirely — for a test to assert on server-side state
    /// directly, same as `SmtpSink::messages`.
    pub fn object(&self, key: &str) -> Option<Vec<u8>> {
        self.store
            .lock()
            .expect("mock S3 store lock")
            .objects
            .get(key)
            .cloned()
    }

    pub fn object_count(&self) -> usize {
        self.store.lock().expect("mock S3 store lock").objects.len()
    }
}

/// The `/{bucket}/{key...}` split this mock requires (path-style only — this
/// is a test double, not a second backend to maintain in two styles).
fn bucket_and_key(path: &str) -> Option<(&str, &str)> {
    let path = path.strip_prefix('/')?;
    let (bucket, key) = path.split_once('/')?;
    Some((bucket, key))
}

async fn handle(
    State(store): State<Arc<Mutex<Store>>>,
    OriginalUri(uri): OriginalUri,
    method: Method,
    headers: HeaderMap,
    request: Request,
) -> Response {
    let path = uri.path().to_string();
    let query: Vec<(String, String)> = uri
        .query()
        .map(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default();

    let Ok(body) = axum::body::to_bytes(request.into_body(), 32 * 1024 * 1024).await else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    if let Err(status) = verify_signature(&method, &path, &query, &headers, &body) {
        return status.into_response();
    }

    let Some((bucket, key)) = bucket_and_key(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if bucket != BUCKET {
        return StatusCode::NOT_FOUND.into_response();
    }

    let mut store = store.lock().expect("mock S3 store lock");
    match method {
        Method::PUT => {
            store.objects.insert(key.to_string(), body.to_vec());
            StatusCode::OK.into_response()
        }
        Method::GET => match store.objects.get(key) {
            Some(data) => (StatusCode::OK, Bytes::from(data.clone())).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
        Method::HEAD => match store.objects.get(key) {
            Some(data) => (
                StatusCode::OK,
                [(axum::http::header::CONTENT_LENGTH, data.len().to_string())],
            )
                .into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
        Method::DELETE => {
            store.objects.remove(key);
            StatusCode::NO_CONTENT.into_response()
        }
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

/// Recompute the signature this request should carry (header auth or
/// presigned query auth), and refuse (403) if it does not match what
/// arrived, or a required piece is missing.
fn verify_signature(
    method: &Method,
    path: &str,
    query: &[(String, String)],
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), StatusCode> {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::FORBIDDEN)?;

    if let Some((_, signature)) = query.iter().find(|(k, _)| k == "X-Amz-Signature") {
        // Presigned query auth: rebuild the canonical query (every param
        // except the signature itself) and recompute.
        let date = query
            .iter()
            .find(|(k, _)| k == "X-Amz-Date")
            .map(|(_, v)| v.as_str())
            .ok_or(StatusCode::FORBIDDEN)?;
        let expires = query
            .iter()
            .find(|(k, _)| k == "X-Amz-Expires")
            .and_then(|(_, v)| v.parse::<u32>().ok())
            .ok_or(StatusCode::FORBIDDEN)?;
        let when = chrono::NaiveDateTime::parse_from_str(date, "%Y%m%dT%H%M%SZ")
            .map_err(|_| StatusCode::FORBIDDEN)?
            .and_utc();
        let expected_query =
            sigv4::presign_query(host, path, &credentials(), REGION, "s3", when, expires);
        let expected_signature = expected_query
            .rsplit("X-Amz-Signature=")
            .next()
            .ok_or(StatusCode::FORBIDDEN)?;
        if expected_signature == signature {
            Ok(())
        } else {
            Err(StatusCode::FORBIDDEN)
        }
    } else {
        // Header auth.
        let authorization = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(StatusCode::FORBIDDEN)?;
        let amz_date = headers
            .get("x-amz-date")
            .and_then(|v| v.to_str().ok())
            .ok_or(StatusCode::FORBIDDEN)?;
        let content_sha256 = headers
            .get("x-amz-content-sha256")
            .and_then(|v| v.to_str().ok())
            .ok_or(StatusCode::FORBIDDEN)?;
        if content_sha256 != sigv4::sha256_hex(body) {
            return Err(StatusCode::FORBIDDEN);
        }
        let when = chrono::NaiveDateTime::parse_from_str(amz_date, "%Y%m%dT%H%M%SZ")
            .map_err(|_| StatusCode::FORBIDDEN)?
            .and_utc();
        let req = SigningRequest {
            method: method.as_str(),
            host,
            path,
            query: &[],
            payload_hash: content_sha256,
        };
        let expected = sigv4::sign_request(
            &req,
            &credentials(),
            REGION,
            "s3",
            when,
            &[
                ("x-amz-date", amz_date),
                ("x-amz-content-sha256", content_sha256),
            ],
        );
        if expected == authorization {
            Ok(())
        } else {
            Err(StatusCode::FORBIDDEN)
        }
    }
}
