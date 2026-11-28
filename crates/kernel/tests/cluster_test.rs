#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Two kernels behind a round robin load balancer.
//!
//! These tests talk HTTP to the proxy of `docker-compose.cluster.yml` and run
//! only when `TROVATO_CLUSTER_URL` names it; without it every test returns at
//! once, so `cargo test --all` is unchanged on a machine with no cluster.
//!
//! ```text
//! docker compose -f docker-compose.cluster.yml up -d --build
//! TROVATO_CLUSTER_URL=http://localhost:8095 cargo test -p trovato-kernel --test cluster_test
//! ```
//!
//! The proxy pins a request carrying `X-Trovato-Node: a` or `b` to that node,
//! so "write on A, read on B" is deterministic. "Current on B" means within one
//! second of the write's response, polled, not within a cache TTL.
//!
//! A few scenarios drive the harness itself (restart Redis, restart a kernel,
//! run a third container, boot two kernels on a fresh database), so they shell
//! out to `docker compose` with the same compose file. The scenarios share one
//! site and some restart its servers, so every test holds one lock and they
//! run one at a time whatever `--test-threads` says.
//!
//! Optional environment:
//! - `TROVATO_CLUSTER_DATABASE_URL`: the cluster's Postgres as the host sees
//!   it, used to look up ids and to make the fresh database of scenario 12
//!   (default `postgres://trovato:trovato@localhost:56432/trovato`).
//! - `TROVATO_CLUSTER_COMPOSE_FILE`: the compose file (default: the one at the
//!   repository root).

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sqlx::PgPool;

/// One scenario at a time: they share a site, and some restart its servers.
static SERIAL: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

const ADMIN_NAME: &str = "clusteradmin";
const ADMIN_PASSWORD: &str = "cluster-admin-password";
const CRON_KEY: &str = "cluster-cron-key";

/// How long a write may take to be current on the other node.
const CURRENT_WITHIN: Duration = Duration::from_secs(1);

/// Node pin for a request; `None` lets the proxy pick round robin.
#[derive(Clone, Copy, Debug)]
enum On {
    A,
    B,
    Any,
}

impl On {
    fn header(self) -> Option<&'static str> {
        match self {
            On::A => Some("a"),
            On::B => Some("b"),
            On::Any => None,
        }
    }
}

/// A response, read whole.
struct Resp {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("expected JSON ({e}), got {}: {}", self.status, self.body))
    }

    fn location(&self) -> String {
        self.headers
            .get("location")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    }

    fn served_by(&self) -> String {
        self.headers
            .get("x-trovato-served-by")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string()
    }
}

/// A browser-ish client: its own cookie jar, no redirect following.
///
/// The session cookie is `Secure`, which a cookie store would refuse to send
/// over plain HTTP to a non-loopback host, so the jar is kept by hand.
struct Client {
    base: String,
    http: reqwest::Client,
    cookies: std::sync::Mutex<BTreeMap<String, String>>,
}

impl Client {
    fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(60))
                .build()
                .unwrap(),
            cookies: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    fn cookie_header(&self) -> String {
        self.cookies
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn store_cookies(&self, headers: &HeaderMap) {
        let mut jar = self.cookies.lock().unwrap();
        for value in headers.get_all("set-cookie") {
            let Ok(value) = value.to_str() else { continue };
            let mut parts = value.split(';');
            let Some((name, val)) = parts.next().and_then(|p| p.trim().split_once('=')) else {
                continue;
            };
            let expired = parts.any(|p| {
                let p = p.trim().to_ascii_lowercase();
                p == "max-age=0" || p.starts_with("expires=thu, 01 jan 1970")
            });
            if expired || val.is_empty() {
                jar.remove(name);
            } else {
                jar.insert(name.to_string(), val.to_string());
            }
        }
    }

    async fn send(
        &self,
        on: On,
        method: Method,
        path: &str,
        headers: &[(&str, String)],
        body: Option<(String, Vec<u8>)>,
    ) -> Resp {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("cookie", self.cookie_header());
        if let Some(node) = on.header() {
            request = request.header("x-trovato-node", node);
        }
        for (name, value) in headers {
            request = request.header(*name, HeaderValue::from_str(value).unwrap());
        }
        if let Some((content_type, bytes)) = body {
            request = request.header("content-type", content_type).body(bytes);
        }
        let response = request
            .send()
            .await
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        let status = response.status();
        let headers = response.headers().clone();
        self.store_cookies(&headers);
        let body = response.text().await.unwrap_or_default();
        Resp {
            status,
            headers,
            body,
        }
    }

    async fn get(&self, on: On, path: &str) -> Resp {
        self.send(on, Method::GET, path, &[], None).await
    }

    async fn get_html(&self, on: On, path: &str) -> Resp {
        self.send(
            on,
            Method::GET,
            path,
            &[("accept", "text/html".to_string())],
            None,
        )
        .await
    }

    async fn post_form(&self, on: On, path: &str, fields: &[(&str, String)]) -> Resp {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        self.send(
            on,
            Method::POST,
            path,
            &[],
            Some((
                "application/x-www-form-urlencoded".to_string(),
                body.into_bytes(),
            )),
        )
        .await
    }

    async fn send_json(
        &self,
        on: On,
        method: Method,
        path: &str,
        value: &Value,
        csrf: &str,
    ) -> Resp {
        self.send(
            on,
            method,
            path,
            &[("x-csrf-token", csrf.to_string())],
            Some((
                "application/json".to_string(),
                value.to_string().into_bytes(),
            )),
        )
        .await
    }

    /// A fresh CSRF token for this session, minted by `on`.
    async fn csrf(&self, on: On) -> String {
        let page = self.get(on, "/user/profile").await;
        csrf_in(&page.body).unwrap_or_else(|| {
            panic!(
                "no CSRF token on /user/profile ({}): {}",
                page.status, page.body
            )
        })
    }

    /// A form post carrying a fresh `_token` (minted on the same node).
    async fn submit(&self, on: On, path: &str, fields: &[(&str, String)]) -> Resp {
        let token = self.csrf(on).await;
        let mut all = vec![("_token", token), ("_form_build_id", "x".to_string())];
        all.extend(fields.iter().cloned());
        self.post_form(on, path, &all).await
    }

    async fn login(&self, on: On, name: &str, password: &str) -> Resp {
        self.send(
            on,
            Method::POST,
            "/user/login/json",
            &[],
            Some((
                "application/json".to_string(),
                json!({"username": name, "password": password})
                    .to_string()
                    .into_bytes(),
            )),
        )
        .await
    }
}

fn csrf_in(html: &str) -> Option<String> {
    static META: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?:name="csrf-token" content="|name="_token" value=")([0-9a-f]{64})""#)
            .unwrap()
    });
    META.captures(html).map(|c| c[1].to_string())
}

/// The `_token` and `_form_build_id` of the pipeline form `form_id` on
/// `html`: what a browser posts back with that form's fields.
fn pipeline_fields(html: &str, form_id: &str) -> Vec<(&'static str, String)> {
    let at = html
        .find(&format!(r#"<form id="{form_id}""#))
        .unwrap_or_else(|| panic!("no form {form_id} on the page: {html}"));
    let form = &html[at..];
    let value = |name: &str| {
        let re = Regex::new(&format!(r#"name="{name}" value="([^"]*)""#)).unwrap();
        re.captures(form)
            .map(|c| c[1].to_string())
            .unwrap_or_else(|| panic!("form {form_id} has no {name}"))
    };
    vec![
        ("_token", value("_token")),
        ("_form_build_id", value("_form_build_id")),
    ]
}

/// A short unique suffix for names this run creates.
fn unique() -> String {
    uuid::Uuid::now_v7().simple().to_string()[20..].to_string()
}

/// Poll `check` until it holds or `within` passes; true if it held.
async fn eventually<F, Fut>(within: Duration, mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + within;
    loop {
        if check().await {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// =============================================================================
// The harness
// =============================================================================

struct Cluster {
    url: String,
    db: PgPool,
    admin: Client,
}

/// The cluster, set up and with an administrator signed in, or `None` (skip)
/// when `TROVATO_CLUSTER_URL` is not set.
async fn cluster() -> Option<Cluster> {
    let url = std::env::var("TROVATO_CLUSTER_URL").ok()?;
    let db_url = std::env::var("TROVATO_CLUSTER_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://trovato:trovato@localhost:56432/trovato".to_string());
    wait_healthy(&url, On::A).await;
    wait_healthy(&url, On::B).await;
    wait_in_rotation(&url).await;
    let db = PgPool::connect(&db_url).await.expect("cluster database");
    let admin = Client::new(&url);
    install(&admin).await;
    let login = admin.login(On::Any, ADMIN_NAME, ADMIN_PASSWORD).await;
    assert_eq!(login.status, StatusCode::OK, "admin login: {}", login.body);
    Some(Cluster { url, db, admin })
}

/// Run the installer once; later runs find the site installed.
async fn install(client: &Client) {
    let front = client.get(On::Any, "/").await;
    if front.location() != "/install" {
        return;
    }
    let form = client.get(On::Any, "/install/admin").await;
    let admin = client
        .post_form(
            On::Any,
            "/install/admin",
            &[
                ("_token", form_token(&form.body)),
                ("username", ADMIN_NAME.to_string()),
                ("email", "admin@cluster.test".to_string()),
                ("password", ADMIN_PASSWORD.to_string()),
                ("password_confirm", ADMIN_PASSWORD.to_string()),
            ],
        )
        .await;
    assert!(
        admin.status.is_redirection(),
        "install admin: {}",
        admin.body
    );
    let form = client.get(On::Any, "/install/site").await;
    let site = client
        .post_form(
            On::Any,
            "/install/site",
            &[
                ("_token", form_token(&form.body)),
                ("site_name", "Cluster".to_string()),
            ],
        )
        .await;
    assert!(site.status.is_redirection(), "install site: {}", site.body);
}

/// The `_token` an installer form carries, or nothing if it carries none.
fn form_token(html: &str) -> String {
    let marker = "name=\"_token\" value=\"";
    html.find(marker)
        .map(|at| &html[at + marker.len()..])
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_default()
        .to_string()
}

/// Wait until the round robin backend (`On::Any`) has both nodes in rotation.
///
/// A healthy node is not yet a node the proxy balances to. The pinned
/// backends (`On::A`, `On::B`) forward at once, but the round robin one adds a
/// server only after the proxy's own health checks pass, on its own clock, a
/// few seconds after the node answers. Until then an `On::Any` request gets
/// the proxy's 503. Done when an unpinned request has been answered by each
/// node.
async fn wait_in_rotation(url: &str) {
    let client = Client::new(url);
    let seen = std::sync::Mutex::new(BTreeSet::new());
    let up = eventually(Duration::from_secs(180), || async {
        match client.http.get(format!("{url}/health")).send().await {
            Ok(r) if r.status() == StatusCode::OK => {
                if let Some(node) = r
                    .headers()
                    .get("x-trovato-served-by")
                    .and_then(|v| v.to_str().ok())
                {
                    seen.lock().unwrap().insert(node.to_string());
                }
                seen.lock().unwrap().len() == 2
            }
            _ => false,
        }
    })
    .await;
    assert!(up, "the proxy did not put both nodes in rotation");
}

async fn wait_healthy(url: &str, on: On) {
    let client = Client::new(url);
    let up = eventually(Duration::from_secs(180), || async {
        let mut request = client.http.get(format!("{url}/health"));
        if let Some(node) = on.header() {
            request = request.header("x-trovato-node", node);
        }
        matches!(request.send().await, Ok(r) if r.status() == StatusCode::OK)
    })
    .await;
    assert!(up, "node {on:?} did not become healthy");
}

fn compose_file() -> String {
    std::env::var("TROVATO_CLUSTER_COMPOSE_FILE").unwrap_or_else(|_| {
        format!(
            "{}/../../docker-compose.cluster.yml",
            env!("CARGO_MANIFEST_DIR")
        )
    })
}

/// Run `docker compose -f <cluster file> <args>`; panics on failure.
fn compose(args: &[&str]) -> String {
    let output = Command::new("docker")
        .arg("compose")
        .arg("-f")
        .arg(compose_file())
        .args(args)
        .output()
        .expect("docker compose");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "docker compose {args:?} failed:\n{stdout}\n{stderr}"
    );
    format!("{stdout}{stderr}")
}

/// Restart one kernel and wait for it to answer again.
async fn restart_node(url: &str, service: &str, on: On) {
    compose(&["restart", service]);
    wait_healthy(url, on).await;
    wait_in_rotation(url).await;
}

/// Make sure a plugin is enabled on both nodes, restarting them if it was not.
async fn ensure_enabled(c: &Cluster, plugin: &str) {
    let status: Option<i16> =
        sqlx::query_scalar("SELECT status FROM plugin_status WHERE name = $1")
            .bind(plugin)
            .fetch_optional(&c.db)
            .await
            .unwrap();
    if status == Some(1) {
        return;
    }
    compose(&[
        "exec",
        "-T",
        "node-a",
        "./trovato",
        "plugin",
        "enable",
        plugin,
    ]);
    restart_node(&c.url, "node-a", On::A).await;
    restart_node(&c.url, "node-b", On::B).await;
}

async fn user_id(db: &PgPool, name: &str) -> uuid::Uuid {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(name)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn role_id(db: &PgPool, name: &str) -> uuid::Uuid {
    sqlx::query_scalar("SELECT id FROM roles WHERE name = $1")
        .bind(name)
        .fetch_one(db)
        .await
        .unwrap()
}

/// Create a user through the admin screen on node A; returns the id.
async fn create_user(
    c: &Cluster,
    name: &str,
    password: &str,
    extra: &[(&str, String)],
) -> uuid::Uuid {
    let mut fields = vec![
        ("name", name.to_string()),
        ("mail", format!("{name}@cluster.test")),
        ("password", password.to_string()),
    ];
    fields.extend(extra.iter().cloned());
    let created = c.admin.submit(On::A, "/admin/people/add", &fields).await;
    assert!(
        created.status.is_redirection(),
        "create user: {}",
        created.body
    );
    user_id(&c.db, name).await
}

/// Create a role holding `permission`, keeping every other role's grant of it.
async fn create_role_with(c: &Cluster, name: &str, permission: &str) -> uuid::Uuid {
    let created = c
        .admin
        .submit(
            On::A,
            "/admin/people/roles/add",
            &[("name", name.to_string())],
        )
        .await;
    assert!(
        created.status.is_redirection(),
        "create role: {}",
        created.body
    );
    let role = role_id(&c.db, name).await;

    let grid = c.admin.get(On::A, "/admin/people/permissions").await;
    let index_re = Regex::new(&format!(
        r#"name="permname_(\d+)" value="{}""#,
        regex::escape(permission)
    ))
    .unwrap();
    let index = index_re.captures(&grid.body).expect("permission row")[1].to_string();
    let checked_re = Regex::new(&format!(
        r#"name="perm_{index}_([0-9a-f-]{{36}})"\s*value="1"\s*checked"#
    ))
    .unwrap();
    let mut fields = vec![("permname_0", permission.to_string())];
    let holders: Vec<String> = checked_re
        .captures_iter(&grid.body)
        .map(|c| c[1].to_string())
        .chain(std::iter::once(role.to_string()))
        .collect();
    let keys: Vec<String> = holders.iter().map(|id| format!("perm_0_{id}")).collect();
    for key in &keys {
        fields.push((key.as_str(), "1".to_string()));
    }
    let saved = c
        .admin
        .submit(On::A, "/admin/people/permissions", &fields)
        .await;
    assert!(saved.status.is_redirection(), "grant: {}", saved.body);
    role
}

/// Create a `page` item as the administrator on `on`; returns its id.
async fn create_item(c: &Cluster, on: On, title: &str) -> String {
    let csrf = c.admin.csrf(on).await;
    let created = c
        .admin
        .send_json(
            on,
            Method::POST,
            "/item/add/page",
            &json!({"title": title, "status": 1, "fields": {}}),
            &csrf,
        )
        .await;
    assert_eq!(
        created.status,
        StatusCode::OK,
        "create item: {}",
        created.body
    );
    created.json()["id"].as_str().unwrap().to_string()
}

async fn item_title(c: &Cluster, on: On, id: &str) -> String {
    let item = c.admin.get(on, &format!("/api/item/{id}")).await;
    item.json()["title"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

async fn edit_item_title(c: &Cluster, on: On, id: &str, title: &str) {
    let csrf = c.admin.csrf(on).await;
    let edited = c
        .admin
        .send_json(
            on,
            Method::POST,
            &format!("/item/{id}/edit"),
            &json!({"title": title}),
            &csrf,
        )
        .await;
    assert_eq!(edited.status, StatusCode::OK, "edit item: {}", edited.body);
}

/// Edit an item on A; it must be current on B. B reads it first, so a cache
/// on B holds the old title.
async fn assert_item_edit_reaches_b(c: &Cluster) {
    let id = create_item(c, On::A, &format!("Before {}", unique())).await;
    let _ = item_title(c, On::B, &id).await;
    let after = format!("After {}", unique());
    edit_item_title(c, On::A, &id, &after).await;
    let current = eventually(CURRENT_WITHIN, || async {
        item_title(c, On::B, &id).await == after
    })
    .await;
    assert!(
        current,
        "an item edited on A was not current on B within 1 s"
    );
}

// =============================================================================
// Scenarios
// =============================================================================

/// 1. Revoke a role on A; the next request on B is 403.
#[tokio::test]
async fn s01_role_revoked_on_a_refuses_on_b() {
    let _serial = SERIAL.lock().await;
    let Some(c) = cluster().await else { return };
    let tag = unique();
    let role = create_role_with(
        &c,
        &format!("cluster role {tag}"),
        "access administration pages",
    )
    .await;
    let name = format!("editor_{tag}");
    let password = "cluster-user-password";
    let uid = create_user(
        &c,
        &name,
        password,
        &[(&format!("role_{role}"), "1".to_string())],
    )
    .await;

    let user = Client::new(&c.url);
    assert_eq!(
        user.login(On::Any, &name, password).await.status,
        StatusCode::OK
    );
    assert_eq!(
        user.get(On::B, "/admin").await.status,
        StatusCode::OK,
        "granted on B"
    );
    assert_eq!(
        user.get(On::A, "/admin").await.status,
        StatusCode::OK,
        "granted on A"
    );

    // Every field resent except the role: the role is revoked.
    let revoked = c
        .admin
        .submit(
            On::A,
            &format!("/admin/people/{uid}/edit"),
            &[
                ("name", name.clone()),
                ("mail", format!("{name}@cluster.test")),
                ("status", "1".to_string()),
            ],
        )
        .await;
    assert!(revoked.status.is_redirection(), "revoke: {}", revoked.body);

    let refused = eventually(CURRENT_WITHIN, || async {
        user.get(On::B, "/admin").await.status == StatusCode::FORBIDDEN
    })
    .await;
    assert!(refused, "a role revoked on A still granted on B");
}

/// 2. Block a user on A; B refuses the session.
#[tokio::test]
async fn s02_user_blocked_on_a_is_refused_on_b() {
    let _serial = SERIAL.lock().await;
    let Some(c) = cluster().await else { return };
    let name = format!("blocked_{}", unique());
    let password = "cluster-user-password";
    let uid = create_user(&c, &name, password, &[]).await;

    let user = Client::new(&c.url);
    assert_eq!(
        user.login(On::Any, &name, password).await.status,
        StatusCode::OK
    );
    assert_eq!(
        user.get(On::B, "/user/profile").await.status,
        StatusCode::OK
    );

    // No `status` field: the account is blocked.
    let blocked = c
        .admin
        .submit(
            On::A,
            &format!("/admin/people/{uid}/edit"),
            &[
                ("name", name.clone()),
                ("mail", format!("{name}@cluster.test")),
            ],
        )
        .await;
    assert!(blocked.status.is_redirection(), "block: {}", blocked.body);

    let refused = eventually(CURRENT_WITHIN, || async {
        let page = user.get(On::B, "/user/profile").await;
        // Signed out by the block: refused (the access-denied page, `403`, to
        // this browser), where an active session is shown its profile.
        page.status == StatusCode::FORBIDDEN || page.status == StatusCode::UNAUTHORIZED
    })
    .await;
    assert!(refused, "a user blocked on A kept a session on B");
    let again = Client::new(&c.url);
    assert_eq!(
        again.login(On::B, &name, password).await.status,
        StatusCode::UNAUTHORIZED,
        "a user blocked on A could sign in on B"
    );
}

/// 3. Log in on A and browse on B; render a form on A and post it to B; start
/// a passkey challenge on A and verify it on B.
#[tokio::test]
async fn s03_session_form_and_passkey_cross_nodes() {
    let _serial = SERIAL.lock().await;
    let Some(c) = cluster().await else { return };

    let user = Client::new(&c.url);
    assert_eq!(
        user.login(On::A, ADMIN_NAME, ADMIN_PASSWORD).await.status,
        StatusCode::OK
    );
    let page = user.get(On::B, "/admin").await;
    assert_eq!(
        page.status,
        StatusCode::OK,
        "a session opened on A works on B"
    );
    assert_eq!(page.served_by(), "b");

    let form = user.get(On::A, "/admin/people/roles/add").await;
    let token = csrf_in(&form.body).expect("form token");
    let role = format!("cross node {}", unique());
    let posted = user
        .post_form(
            On::B,
            "/admin/people/roles/add",
            &[
                ("_token", token),
                ("_form_build_id", "x".to_string()),
                ("name", role.clone()),
            ],
        )
        .await;
    assert!(
        posted.status.is_redirection(),
        "a form rendered on A posts to B: {} {}",
        posted.status,
        posted.body
    );
    let _ = role_id(&c.db, &role).await;

    let csrf = user.csrf(On::A).await;
    let start = user
        .send_json(
            On::A,
            Method::POST,
            "/user/webauthn/register/start",
            &json!({}),
            &csrf,
        )
        .await;
    assert_eq!(
        start.status,
        StatusCode::OK,
        "passkey start: {}",
        start.body
    );
    let finish_token = start.json()["csrf_token"].as_str().unwrap().to_string();
    let bogus = json!({
        "credential": {
            "id": "AAAA",
            "rawId": "AAAA",
            "response": {"attestationObject": "AAAA", "clientDataJSON": "AAAA"},
            "type": "public-key",
            "extensions": {}
        }
    });
    let finish = user
        .send_json(
            On::B,
            Method::POST,
            "/user/webauthn/register/finish",
            &bogus,
            &finish_token,
        )
        .await;
    assert!(
        finish.body.contains("Registration could not be verified"),
        "B must find the challenge A started and reject only the credential: {} {}",
        finish.status,
        finish.body
    );
}

/// 4. Edit an item, add a field to a type, rename a term, edit a gather query,
/// add a redirect and import a locale string on A; each is current on B.
#[tokio::test]
async fn s04_content_writes_on_a_are_current_on_b() {
    let _serial = SERIAL.lock().await;
    let Some(c) = cluster().await else { return };
    ensure_enabled(&c, "trovato_categories").await;
    let tag = unique();

    // Item.
    assert_item_edit_reaches_b(&c).await;

    // Field on a type.
    let fields_page = "/admin/structure/types/page/fields";
    let field = format!("cf_{tag}");
    let _ = c.admin.get(On::B, fields_page).await;
    // The add-field form is a pipeline form: post its own token and build id.
    let form = c.admin.get(On::A, fields_page).await;
    let token = csrf_in(&form.body).expect("form token");
    let build_id = Regex::new(r#"name="_form_build_id" value="([^"]+)""#)
        .unwrap()
        .captures(&form.body)
        .map(|c| c[1].to_string())
        .expect("form build id");
    let added = c
        .admin
        .post_form(
            On::A,
            "/admin/structure/types/page/fields/add",
            &[
                ("_token", token),
                ("_form_build_id", build_id),
                ("label", format!("Cluster {tag}")),
                ("name", field.clone()),
                ("field_type", "text".to_string()),
            ],
        )
        .await;
    assert!(added.status.is_redirection(), "add field: {}", added.body);
    let current = eventually(CURRENT_WITHIN, || async {
        c.admin.get(On::B, fields_page).await.body.contains(&field)
    })
    .await;
    assert!(
        current,
        "a field added on A was not current on B within 1 s"
    );

    // Term.
    let csrf = c.admin.csrf(On::A).await;
    let _ = c
        .admin
        .send_json(
            On::A,
            Method::POST,
            "/api/category",
            &json!({"id": "cluster", "label": "Cluster"}),
            &csrf,
        )
        .await;
    let csrf = c.admin.csrf(On::A).await;
    let term = c
        .admin
        .send_json(
            On::A,
            Method::POST,
            "/api/tag",
            &json!({"category_id": "cluster", "label": format!("Old {tag}")}),
            &csrf,
        )
        .await;
    assert_eq!(
        term.status,
        StatusCode::CREATED,
        "create term: {}",
        term.body
    );
    let term_id = term.json()["id"].as_str().unwrap().to_string();
    let _ = c.admin.get(On::B, &format!("/api/tag/{term_id}")).await;
    let renamed = format!("New {tag}");
    let csrf = c.admin.csrf(On::A).await;
    let put = c
        .admin
        .send_json(
            On::A,
            Method::PUT,
            &format!("/api/tag/{term_id}"),
            &json!({"label": renamed}),
            &csrf,
        )
        .await;
    assert_eq!(put.status, StatusCode::OK, "rename term: {}", put.body);
    let current = eventually(CURRENT_WITHIN, || async {
        let t = c.admin.get(On::B, &format!("/api/tag/{term_id}")).await;
        t.status == StatusCode::OK && t.json()["label"] == json!(renamed)
    })
    .await;
    assert!(
        current,
        "a term renamed on A was not current on B within 1 s"
    );

    // Gather query.
    let query = format!("cluster_q_{tag}");
    let gather_fields = |label: String, display: Value| {
        vec![
            ("query_id", query.clone()),
            ("label", label),
            ("description", String::new()),
            (
                "definition_json",
                json!({"base_table": "item", "item_type": "page"}).to_string(),
            ),
            ("display_json", display.to_string()),
        ]
    };
    // The query editor is a pipeline form: post what its page built.
    let editor = c.admin.get_html(On::A, "/admin/gather/create").await;
    let mut fields = pipeline_fields(&editor.body, "gather-form");
    fields.extend(gather_fields(format!("Old {tag}"), json!({})));
    let created = c
        .admin
        .post_form(On::A, "/admin/gather/create", &fields)
        .await;
    assert!(
        created.status.is_redirection(),
        "create gather: {}",
        created.body
    );
    // A newly created query reaches B on its own reload; wait for that, then
    // the edit is what is measured.
    let present = eventually(Duration::from_secs(90), || async {
        c.admin
            .get(On::B, &format!("/api/query/{query}"))
            .await
            .status
            == StatusCode::OK
    })
    .await;
    assert!(present, "a gather query created on A never reached B");
    let _ = c.admin.get(On::B, &format!("/gather/{query}")).await;
    let new_label = format!("New {tag}");
    let target = format!("/cluster-target-{tag}");
    let editor = c
        .admin
        .get_html(On::A, &format!("/admin/gather/{query}/edit"))
        .await;
    let mut fields = pipeline_fields(&editor.body, "gather-form");
    fields.extend(gather_fields(
        new_label.clone(),
        json!({"canonical_url": target}),
    ));
    let saved = c
        .admin
        .post_form(On::A, &format!("/admin/gather/{query}/save"), &fields)
        .await;
    assert!(saved.status.is_redirection(), "save gather: {}", saved.body);
    let current = eventually(CURRENT_WITHIN, || async {
        c.admin
            .get(On::B, &format!("/api/query/{query}"))
            .await
            .json()["label"]
            == json!(new_label)
    })
    .await;
    assert!(
        current,
        "a gather query edited on A was not current on B within 1 s"
    );

    // Redirect: the canonical URL just saved on A adds one.
    let current = eventually(CURRENT_WITHIN, || async {
        let r = c.admin.get(On::B, &format!("/gather/{query}")).await;
        r.status == StatusCode::MOVED_PERMANENTLY && r.location() == target
    })
    .await;
    assert!(
        current,
        "a redirect added on A was not current on B within 1 s"
    );

    // Locale string, imported on A. The harness's 404 template translates
    // "Cluster locale probe" (docker/cluster/templates/error--404.html).
    let probe = "/cluster-no-such-page";
    let _ = c.admin.get_html(On::B, probe).await;
    let translation = format!("Probe {tag}");
    let yaml = format!(
        "id: 0193a5a0-0095-7000-8000-000000000001\nsource: Cluster locale probe\ntranslation: {translation}\nlanguage: en\ncontext: ''\n"
    );
    let script = format!(
        "rm -rf /tmp/locale && mkdir /tmp/locale && cat > /tmp/locale/locale_string.0193a5a0-0095-7000-8000-000000000001.yml <<'EOF'\n{yaml}EOF\n./trovato config import /tmp/locale"
    );
    compose(&["exec", "-T", "node-a", "sh", "-c", &script]);
    let current = eventually(CURRENT_WITHIN, || async {
        c.admin
            .get_html(On::B, probe)
            .await
            .body
            .contains(&translation)
    })
    .await;
    assert!(
        current,
        "a locale string imported on A was not current on B within 1 s"
    );
}

/// 5. Invalidate a cached value on A; B misses.
///
/// No shipped plugin uses the cache host functions, so this goes through the
/// kernel's own user of the same cache layer: the search settings, cached
/// under one key and invalidated when the settings form is saved.
#[tokio::test]
async fn s05_cache_invalidated_on_a_misses_on_b() {
    let _serial = SERIAL.lock().await;
    let Some(c) = cluster().await else { return };

    async fn followups(c: &Cluster, on: On) -> Value {
        let page = c.admin.get(on, "/search").await;
        let open = r#"<script id="scolta-config" type="application/json">"#;
        let start = page.body.find(open).expect("config block") + open.len();
        let end = page.body[start..].find("</script>").unwrap();
        let config: Value = serde_json::from_str(page.body[start..start + end].trim()).unwrap();
        config["scoring"]["AI_MAX_FOLLOWUPS"].clone()
    }

    let before = followups(&c, On::B).await;
    let _ = followups(&c, On::B).await;
    let next = if before == json!(4) { 5 } else { 4 };
    let saved = c
        .admin
        .submit(
            On::A,
            "/admin/config/search",
            &[
                ("expand", "on".to_string()),
                ("summarize", "on".to_string()),
                ("followup", "on".to_string()),
                ("analytics_enabled", "on".to_string()),
                ("max_followups", next.to_string()),
            ],
        )
        .await;
    assert!(
        saved.status.is_redirection(),
        "save settings: {}",
        saved.body
    );
    let current = eventually(CURRENT_WITHIN, || async {
        followups(&c, On::B).await == json!(next)
    })
    .await;
    assert!(
        current,
        "a cache entry invalidated on A was still served on B"
    );
}

/// A small real PNG.
fn png() -> Vec<u8> {
    let image = image::RgbImage::from_pixel(32, 32, image::Rgb([200, 30, 30]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

/// 6. Upload on A; the file and an image derivative are served from B.
#[tokio::test]
async fn s06_upload_on_a_is_served_from_b() {
    let _serial = SERIAL.lock().await;
    let Some(c) = cluster().await else { return };
    ensure_enabled(&c, "trovato_image_styles").await;

    let boundary = "clusterboundary7MA4YWxkTrZu0gW";
    let name = format!("cluster-{}.png", unique());
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: image/png\r\n\r\n"
    )
    .into_bytes();
    body.extend(png());
    body.extend(format!("\r\n--{boundary}--\r\n").into_bytes());
    let csrf = c.admin.csrf(On::A).await;
    let uploaded = c
        .admin
        .send(
            On::A,
            Method::POST,
            "/file/upload",
            &[("x-csrf-token", csrf)],
            Some((format!("multipart/form-data; boundary={boundary}"), body)),
        )
        .await;
    assert_eq!(uploaded.status, StatusCode::OK, "upload: {}", uploaded.body);
    let url = uploaded.json()["file"]["url"].as_str().unwrap().to_string();

    let file = c.admin.get(On::B, &url).await;
    assert_eq!(
        file.status,
        StatusCode::OK,
        "the file uploaded on A is served from B"
    );
    assert_eq!(file.served_by(), "b");

    let relative = url.trim_start_matches("/files/");
    let derivative = c
        .admin
        .get(On::B, &format!("/files/styles/w16/{relative}"))
        .await;
    assert_eq!(
        derivative.status,
        StatusCode::OK,
        "an image derivative is served from B: {}",
        derivative.body
    );
    // Made by B, served by A from the shared volume.
    let again = c
        .admin
        .get(On::A, &format!("/files/styles/w16/{relative}"))
        .await;
    assert_eq!(
        again.status,
        StatusCode::OK,
        "the derivative made on B is served from A"
    );
}

/// The meta file each language's entry names. A publish that adds a page
/// always names a new one; `page_count` would not do, since a new item may
/// take a deleted item's slot and leave the count as it was.
async fn published_metas(c: &Cluster, on: On) -> Option<Vec<String>> {
    let entry = c
        .admin
        .get(on, "/static/pagefind/pagefind-entry.json")
        .await;
    if entry.status != StatusCode::OK {
        return None;
    }
    let value: Value = serde_json::from_str(&entry.body).ok()?;
    let mut metas: Vec<String> = value["languages"]
        .as_object()?
        .values()
        .filter_map(|l| l["hash"].as_str().map(str::to_string))
        .collect();
    metas.sort();
    Some(metas)
}

/// 7. Pagefind rebuild on whichever node wins cron; search on the other node
/// finds the new item.
#[tokio::test]
async fn s07_pagefind_built_by_one_node_is_served_by_the_other() {
    let _serial = SERIAL.lock().await;
    let Some(c) = cluster().await else { return };
    ensure_enabled(&c, "trovato_search").await;

    // Settle: an index of everything so far.
    for _ in 0..2 {
        let _ = c
            .admin
            .post_form(On::Any, &format!("/cron/{CRON_KEY}"), &[])
            .await;
    }
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let before_a = published_metas(&c, On::A).await;
    let before_b = published_metas(&c, On::B).await;

    let _ = create_item(&c, On::A, &format!("Searchable {}", unique())).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;

    let mut builder = None;
    for _ in 0..5 {
        let run = c
            .admin
            .post_form(On::Any, &format!("/cron/{CRON_KEY}"), &[])
            .await;
        let value = run.json();
        let built = value["tasks"]
            .as_array()
            .is_some_and(|t| t.iter().any(|t| t == "pagefind_rebuild"));
        if built {
            builder = Some(run.served_by());
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let builder = builder.expect("no cron run rebuilt the Pagefind index");
    let (other, before) = if builder == "a" {
        (On::B, before_b)
    } else {
        (On::A, before_a)
    };
    let after = published_metas(&c, other).await;
    assert!(
        after.is_some() && after != before,
        "node {builder} built the index; the other node serves metas {after:?} (was {before:?})"
    );
}

/// 8. Fire cron on both nodes at once; exactly one run is recorded.
#[tokio::test]
async fn s08_concurrent_cron_runs_once() {
    let _serial = SERIAL.lock().await;
    let Some(c) = cluster().await else { return };
    let path = format!("/cron/{CRON_KEY}");
    for _ in 0..3 {
        let (a1, b1, a2, b2) = tokio::join!(
            c.admin.post_form(On::A, &path, &[]),
            c.admin.post_form(On::B, &path, &[]),
            c.admin.post_form(On::A, &path, &[]),
            c.admin.post_form(On::B, &path, &[]),
        );
        let statuses: Vec<String> = [a1, b1, a2, b2]
            .iter()
            .map(|r| r.json()["status"].as_str().unwrap_or_default().to_string())
            .collect();
        let completed = statuses.iter().filter(|s| *s == "completed").count();
        assert_eq!(completed, 1, "exactly one run, got {statuses:?}");
        assert!(
            statuses.iter().all(|s| s == "completed" || s == "skipped"),
            "{statuses:?}"
        );
    }
}

/// 9. Chat requests alternating A and B are refused at N, not 2N.
#[tokio::test]
async fn s09_chat_limit_is_shared() {
    let _serial = SERIAL.lock().await;
    let Some(c) = cluster().await else { return };
    let limit = 3;
    let saved = c
        .admin
        .submit(
            On::A,
            "/admin/system/ai-chat",
            &[
                ("system_prompt", "You are a test.".to_string()),
                ("rag_max_results", "5".to_string()),
                ("rag_min_score", "0.3".to_string()),
                ("max_history_turns", "5".to_string()),
                ("rate_limit_per_hour", limit.to_string()),
                ("max_tokens", "256".to_string()),
            ],
        )
        .await;
    assert!(
        saved.status.is_redirection(),
        "save chat config: {}",
        saved.body
    );

    // A new administrator each run, so earlier runs' counts do not carry over.
    let name = format!("chat_{}", unique());
    let password = "cluster-user-password";
    create_user(&c, &name, password, &[("is_admin", "1".to_string())]).await;
    let user = Client::new(&c.url);
    assert_eq!(
        user.login(On::Any, &name, password).await.status,
        StatusCode::OK
    );

    let mut statuses = Vec::new();
    for i in 0..=limit {
        let on = if i % 2 == 0 { On::A } else { On::B };
        let csrf = user.csrf(on).await;
        let reply = user
            .send_json(
                on,
                Method::POST,
                "/api/v1/chat",
                &json!({"message": "hello"}),
                &csrf,
            )
            .await;
        statuses.push(reply.status);
    }
    assert!(
        statuses[..limit]
            .iter()
            .all(|s| *s != StatusCode::TOO_MANY_REQUESTS),
        "refused before the limit: {statuses:?}"
    );
    assert_eq!(
        statuses[limit],
        StatusCode::TOO_MANY_REQUESTS,
        "request {} across both nodes must be refused: {statuses:?}",
        limit + 1
    );
}

/// 10. Config import run from a third container; both nodes current, no
/// restart.
#[tokio::test]
async fn s10_config_import_from_a_third_container() {
    let _serial = SERIAL.lock().await;
    let Some(c) = cluster().await else { return };
    let label = format!("Page {}", unique());
    let _ = c.admin.get(On::A, "/admin/structure/types").await;
    let _ = c.admin.get(On::B, "/admin/structure/types").await;

    let script = format!(
        "./trovato config export /tmp/export >/dev/null && mkdir /tmp/import && \
         sed 's/^label: .*/label: {label}/' /tmp/export/item_type.page.yml > /tmp/import/item_type.page.yml && \
         ./trovato config import /tmp/import"
    );
    let output = compose(&["run", "--rm", "--no-deps", "node-a", "sh", "-c", &script]);
    assert!(
        !output.contains("Restart the server"),
        "an import must not ask for a restart: {output}"
    );
    for on in [On::A, On::B] {
        let current = eventually(CURRENT_WITHIN, || async {
            c.admin
                .get(on, "/admin/structure/types")
                .await
                .body
                .contains(&label)
        })
        .await;
        assert!(
            current,
            "node {on:?} did not show the imported label within 1 s"
        );
    }
}

/// 11. Restart Redis mid test; a write on A still reaches B after reconnect.
#[tokio::test]
async fn s11_redis_restart() {
    let _serial = SERIAL.lock().await;
    let Some(c) = cluster().await else { return };
    assert_item_edit_reaches_b(&c).await;

    compose(&["restart", "redis"]);
    wait_healthy(&c.url, On::A).await;
    wait_healthy(&c.url, On::B).await;
    // Sessions lived in Redis; sign in again once it answers.
    let admin = Client::new(&c.url);
    let signed_in = eventually(Duration::from_secs(30), || async {
        admin
            .login(On::Any, ADMIN_NAME, ADMIN_PASSWORD)
            .await
            .status
            == StatusCode::OK
    })
    .await;
    assert!(signed_in, "could not sign in after Redis restarted");
    // Give both nodes' pollers a moment to see the new (reset) Redis.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let c = Cluster { admin, ..c };
    assert_item_edit_reaches_b(&c).await;
}

/// 12. Both nodes boot together on a fresh database with a pending
/// `tap_install`; it runs once.
///
/// Two extra kernels from the same image boot at once against a new database;
/// every enabled plugin is pending there. Each kernel logs one line per
/// `tap_install` it dispatched, so the two logs together must name each plugin
/// once.
///
/// Each kernel claims a plugin's row before dispatching it, so only one of them
/// runs it, however the two boots interleave.
#[tokio::test]
async fn s12_fresh_boot_runs_tap_install_once() {
    let _serial = SERIAL.lock().await;
    let Some(c) = cluster().await else { return };
    let database = format!("fresh_{}", unique());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&c.db)
        .await
        .unwrap();
    let url = format!("postgres://trovato:trovato@postgres:5432/{database}");
    let names = [format!("{database}_1"), format!("{database}_2")];
    let start = |name: &str| {
        let url = url.clone();
        let name = name.to_string();
        std::thread::spawn(move || {
            compose(&[
                "run",
                "-d",
                "--no-deps",
                "--name",
                &name,
                "-e",
                &format!("DATABASE_URL={url}"),
                "node-a",
            ])
        })
    };
    let handles: Vec<_> = names.iter().map(|n| start(n)).collect();
    for h in handles {
        h.join().unwrap();
    }

    let fresh_url = std::env::var("TROVATO_CLUSTER_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://trovato:trovato@localhost:56432/trovato".to_string());
    let fresh_url = format!("{}/{database}", fresh_url.rsplit_once('/').unwrap().0);
    let fresh = PgPool::connect(&fresh_url).await.unwrap();
    let done = eventually(Duration::from_secs(120), || async {
        let pending: Option<i64> = sqlx::query_scalar(
            "SELECT COUNT(*) FROM plugin_status WHERE status = 1 AND tap_install_called = FALSE",
        )
        .fetch_one(&fresh)
        .await
        .ok();
        let total: Option<i64> =
            sqlx::query_scalar("SELECT COUNT(*) FROM plugin_status WHERE status = 1")
                .fetch_one(&fresh)
                .await
                .ok();
        pending == Some(0) && total.unwrap_or(0) > 0
    })
    .await;
    let enabled: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM plugin_status WHERE status = 1")
        .fetch_one(&fresh)
        .await
        .unwrap_or(0);

    let dispatch =
        Regex::new(r"tap_install (?:dispatched|not implemented or failed)[^\n]*plugin\S*?=(\w+)")
            .unwrap();
    let ansi = Regex::new(r"\x1b\[[0-9;]*m").unwrap();
    let read_logs = || {
        let mut logs = String::new();
        for name in &names {
            let out = Command::new("docker")
                .args(["logs", name])
                .output()
                .unwrap();
            logs.push_str(&String::from_utf8_lossy(&out.stdout));
            logs.push_str(&String::from_utf8_lossy(&out.stderr));
        }
        let plain = ansi.replace_all(&logs, "").to_string();
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for cap in dispatch.captures_iter(&plain) {
            *counts.entry(cap[1].to_string()).or_default() += 1;
        }
        (plain, counts)
    };
    // Every row is claimed before its dispatch finishes; wait until a dispatch
    // line names each enabled plugin, then a moment more for any duplicate.
    eventually(Duration::from_secs(120), || async {
        read_logs().1.len() as i64 >= enabled
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (plain, counts) = read_logs();
    for name in &names {
        let _ = Command::new("docker").args(["rm", "-f", name]).output();
    }
    fresh.close().await;
    let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {database} WITH (FORCE)"
    )))
    .execute(&c.db)
    .await;
    assert!(
        done,
        "tap_install never finished on the fresh database:\n{plain}"
    );

    let twice: Vec<_> = counts.iter().filter(|(_, n)| **n > 1).collect();
    assert!(
        twice.is_empty(),
        "tap_install ran more than once: {twice:?}"
    );
    assert!(
        !counts.is_empty(),
        "no tap_install was dispatched:\n{plain}"
    );
}

/// 13. Disable a plugin on A; both nodes keep dispatching it until restarted,
/// and after a rolling restart neither does.
#[tokio::test]
async fn s13_plugin_disable_waits_for_restart() {
    let _serial = SERIAL.lock().await;
    let Some(c) = cluster().await else { return };
    ensure_enabled(&c, "trovato_categories").await;
    let probe = "/api/categories";
    for on in [On::A, On::B] {
        assert_eq!(c.admin.get(on, probe).await.status, StatusCode::OK);
    }

    let page = c.admin.get(On::A, "/admin/plugins").await;
    let token = csrf_in(&page.body).expect("plugin page token");
    let toggled = c
        .admin
        .post_form(
            On::A,
            "/admin/plugins/toggle",
            &[
                ("_token", token),
                ("plugin_name", "trovato_categories".to_string()),
                ("action", "disable".to_string()),
            ],
        )
        .await;
    assert!(toggled.status.is_redirection(), "disable: {}", toggled.body);

    let anon = Client::new(&c.url);
    for on in [On::A, On::B] {
        assert_eq!(
            anon.get(on, probe).await.status,
            StatusCode::OK,
            "node {on:?} stopped dispatching before a restart"
        );
    }

    restart_node(&c.url, "node-a", On::A).await;
    restart_node(&c.url, "node-b", On::B).await;
    for on in [On::A, On::B] {
        assert_eq!(
            anon.get(on, probe).await.status,
            StatusCode::NOT_FOUND,
            "node {on:?} still dispatches after the rolling restart"
        );
    }

    // Put it back for the other scenarios.
    compose(&[
        "exec",
        "-T",
        "node-a",
        "./trovato",
        "plugin",
        "enable",
        "trovato_categories",
    ]);
    restart_node(&c.url, "node-a", On::A).await;
    restart_node(&c.url, "node-b", On::B).await;
}
