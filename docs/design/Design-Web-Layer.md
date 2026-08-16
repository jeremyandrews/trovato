# Trovato Design: Web Layer & Sessions

*Sections 1-2 of the v2.1 Design Document*

---

## 1. The Web Layer (Axum) & Observability

### Why Axum

Axum is built on Tokio and Tower. Tower's middleware model maps well to Drupal's request lifecycle (bootstrap → init → route → handler → response). Axum's extractors give us type-safe access to request data. It's the most actively maintained async web framework in Rust.

We wrap the entire application in a custom profiling middleware ("Gander") to catch bottlenecks early.

### Application State

Every Axum handler gets access to shared state via an `Arc<AppState>`. Note that `PluginRegistry` holds *compiled* plugins (which are `Send + Sync`), not active instances. Per-request instances are managed by `RequestState` (see Section 3).

```rust
use std::sync::Arc;
use dashmap::DashMap;
use sqlx::PgPool;
use wasmtime::Engine;

pub struct AppState {
    pub db: PgPool,
    pub redis: redis::Client,
    pub wasm_engine: Engine,
    pub plugin_registry: PluginRegistry,  // Holds CompiledPlugin, not Stores
    pub tap_registry: TapRegistry,
    pub menu_registry: MenuRegistry,
    pub permissions: DashMap<(Uuid, String), bool>,
    pub theme_engine: ThemeEngine,
    pub cache: CacheLayer,
    pub file_storage: Box<dyn FileStorage>,
}
```

`DashMap` is a concurrent HashMap. We use it for permission lookups because these are read-heavy and rarely written (only when an admin changes permissions or a plugin is enabled/disabled).

### Profiling Middleware

```rust
use axum::{extract::State, response::Response, middleware::Next};
use std::time::Instant;

pub async fn profiling_middleware(
    State(_state): State<Arc<AppState>>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    let start = Instant::now();

    // Initialize request-scoped profile in extensions
    request.extensions_mut().insert(RequestProfile::new());

    let response = next.run(request).await;

    let duration = start.elapsed();
    tracing::info!(
        target: "gander",
        path = %request.uri().path(),
        duration_ms = %duration.as_millis(),
        "Request Complete"
    );
    response
}
```

The profiler logs detailed JSON traces for slow requests, including breakdown of DB query duration, WASM tap invocation duration, and template rendering duration. This data is structured for ingestion by tools like Jaeger or Honeycomb.

### Route Setup

Drupal 6 had two kinds of routes: system routes (login, admin pages) and content routes (anything handled by a plugin's `tap_menu`). We replicate this with static routes plus a fallback handler:

```rust
use axum::{Router, routing::{get, post}};

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/user/login", get(user_login_form).post(user_login_submit))
        .route("/user/logout", get(user_logout))
        .route("/user/{id}", get(user_profile))
        .route("/admin/plugins", get(admin_plugins).post(admin_plugins_submit))
        .route("/admin/content/types", get(admin_content_types))
        .route("/item/add/{type}", get(item_add_form).post(item_add_submit))
        .route("/item/{id}", get(item_view))
        .route("/item/{id}/edit", get(item_edit_form).post(item_edit_submit))
        .route("/item/{id}/revisions", get(item_revisions))
        .route("/file/upload", post(file_upload))
        .route("/search", get(search_page))
        .route("/health", get(health_check))
        .route("/metrics", get(metrics_endpoint))
        .fallback(dynamic_route_handler)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(), profiling_middleware
        ))
        .with_state(state)
}
```

### The Dynamic Route Handler

This is the equivalent of Drupal's menu system. When a request doesn't match any static route, we look it up in a registry populated by plugins:

```rust
use axum::extract::{Path, State};
use axum::response::Response;

async fn dynamic_route_handler(
    Path(path): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    if let Some(menu_item) = state.menu_registry.resolve(&path) {
        if !check_access(&state, &menu_item.permission).await {
            return Response::builder()
                .status(403)
                .body("Access denied".into())
                .unwrap();
        }

        let result = state.plugin_registry
            .invoke(&menu_item.plugin, &menu_item.callback, &path)
            .await;

        match result {
            Ok(html) => Response::builder()
                .status(200)
                .header("content-type", "text/html")
                .body(html.into())
                .unwrap(),
            Err(e) => {
                tracing::error!("Plugin error: {e}");
                Response::builder()
                    .status(500)
                    .body("Internal error".into())
                    .unwrap()
            }
        }
    } else {
        Response::builder()
            .status(404)
            .body("Not found".into())
            .unwrap()
    }
}
```

### The Menu Registry

In Drupal 6, `tap_menu` returned an array of path patterns mapped to callback functions and access arguments. We store this in memory, rebuilt on cache clear or plugin enable/disable:

```rust
use std::collections::HashMap;

pub struct MenuItem {
    pub path_pattern: String,
    pub plugin: String,
    pub callback: String,
    pub permission: String,
    pub title: String,
    pub parent: Option<String>,  // path of parent menu item; enables breadcrumb generation
}

pub struct MenuRegistry {
    exact: HashMap<String, MenuItem>,
    patterns: Vec<(Vec<PathSegment>, MenuItem)>,
}

enum PathSegment {
    Literal(String),
    Wildcard(String),
}

impl MenuRegistry {
    pub fn resolve(&self, path: &str) -> Option<&MenuItem> {
        if let Some(item) = self.exact.get(path) {
            return Some(item);
        }

        let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        for (pattern, item) in &self.patterns {
            if pattern.len() == segments.len() {
                let matches = pattern.iter().zip(segments.iter()).all(|(p, s)| {
                    match p {
                        PathSegment::Literal(lit) => lit == s,
                        PathSegment::Wildcard(_) => true,
                    }
                });
                if matches {
                    return Some(item);
                }
            }
        }
        None
    }

    pub fn breadcrumb(&self, path: &str) -> Vec<BreadcrumbItem> {
        let mut crumbs = Vec::new();
        let mut current = self.resolve(path);

        while let Some(item) = current {
            crumbs.push(BreadcrumbItem {
                title: item.title.clone(),
                path: item.path_pattern.clone(),
            });
            current = item.parent.as_ref()
                .and_then(|p| self.resolve(p));
        }

        crumbs.reverse();
        crumbs
    }
}

pub struct BreadcrumbItem {
    pub title: String,
    pub path: String,
}

**Stage-aware menu resolution (Phase 3+):** Menu items registered by plugins via `tap_menu` are currently static (rebuilt on plugin enable/disable). For stage support, menu items created in a non-live stage need a similar override mechanism to items. This is deferred to when menu items become first-class stage-able entities with their own revision table. For MVP, menus are globally visible regardless of stage.
```

### Breadcrumb Generation

The `MenuRegistry` supports breadcrumb generation by walking up the parent chain. A breadcrumb walks from the root of the menu hierarchy to the current page:

```rust
let breadcrumbs = menu_registry.breadcrumb("/blog/tech/rust");
// Returns: [BreadcrumbItem("Home", "/"), BreadcrumbItem("Blog", "/blog"), BreadcrumbItem("Tech", "/blog/tech"), BreadcrumbItem("Rust", "/blog/tech/rust")]
```

Note: Category-aware breadcrumbs (item → term → vocabulary) are content-aware logic and belong in the item plugin or a dedicated breadcrumb plugin, not in the framework. Such breadcrumbs query the item's category field, walk the term hierarchy, and prepend those items to the menu-based breadcrumbs.

---

## 2. Sessions and Authentication

### Session Architecture

Drupal 6 stored sessions in a database table. We use Redis instead — sessions are ephemeral and Redis handles TTL expiration natively.

```rust
use tower_sessions::{SessionManagerLayer, Expiry, cookie::SameSite};
use tower_sessions_redis_store::RedisStore;
use time::Duration;

pub async fn session_layer(redis_url: &str) -> SessionManagerLayer<RedisStore> {
    let client = redis::Client::open(redis_url).unwrap();
    let conn = client.get_multiplexed_async_connection().await.unwrap();
    let store = RedisStore::new(conn);

    SessionManagerLayer::new(store)
        .with_expiry(Expiry::OnInactivity(Duration::hours(24)))
        .with_secure(true)
        .with_http_only(true)
        .with_same_site(SameSite::Strict)
}
```

The session cookie is HttpOnly (no JavaScript access), Secure (HTTPS only), and SameSite=Strict. This eliminates most session hijacking vectors that plagued Drupal 6.

### Stage Awareness

The user's session also tracks their active **Stage**. If a user is in the "Spring Campaign" stage, all item loads must reflect that state.

```rust
pub async fn get_active_stage(
    session: &tower_sessions::Session,
) -> Option<String> {
    session.get::<String>("active_stage").await.ok().flatten()
}
```

### Stage Switching

Users switch stages via an admin toolbar or API endpoint. The switch updates the session:

```rust
pub async fn set_active_stage(
    session: &tower_sessions::Session,
    stage_id: &str,
) {
    session.insert("active_stage", stage_id.to_string()).await.ok();
}
```

**Preview URL scheme:** When a user is in a non-live stage, they see stage content at normal URLs. The stage context comes from the session, not the URL. This means `/item/{id}` shows the stage version automatically — no special preview URLs needed. Sharing a URL with someone not in the stage shows the live version.

For shareable preview links (e.g., sending to a reviewer), use a query parameter: `/item/{id}?stage=spring_campaign&preview_token=abc123`. The preview token is a time-limited HMAC that grants read-only stage access without a session.

### The User Record

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub name: String,
    #[serde(skip_serializing)]
    pub pass: String,
    pub mail: Option<String>,
    pub is_admin: bool,
    pub created: i64,
    pub access: i64,
    pub login: i64,
    pub status: i16,
    pub timezone: Option<String>,
    pub language: Option<String>,
    pub data: serde_json::Value,
}

impl User {
    pub fn is_anonymous(&self) -> bool { self.id == Uuid::nil() }
    pub fn is_superuser(&self) -> bool { self.is_admin }
}
```

In Drupal 6, User 1 bypasses all permission checks. We replace this magic number with an explicit `is_admin` boolean column. The anonymous (not-logged-in) user is represented by `Uuid::nil()`. The `is_admin` flag is set during initial installation and is useful for setup and disaster recovery.

### Authentication Flow

```rust
use argon2::{Argon2, PasswordHash, PasswordVerifier, PasswordHasher};
use argon2::password_hash::SaltString;
use rand::rngs::OsRng;

pub async fn authenticate(
    db: &PgPool, username: &str, password: &str,
) -> Result<User, AuthError> {
    let user: Option<User> = sqlx::query_as(
        "SELECT * FROM users WHERE name = $1 AND status = 1"
    )
    .bind(username)
    .fetch_optional(db)
    .await?;

    let user = user.ok_or(AuthError::InvalidCredentials)?;

    let parsed_hash = PasswordHash::new(&user.pass)
        .map_err(|_| AuthError::InvalidCredentials)?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .map_err(|_| AuthError::InvalidCredentials)?;

    sqlx::query("UPDATE users SET login = $1, access = $1 WHERE id = $2")
        .bind(chrono::Utc::now().timestamp())
        .bind(user.id)
        .execute(db)
        .await?;

    Ok(user)
}

pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default().hash_password(password.as_bytes(), &salt)?;
    Ok(hash.to_string())
}
```

### Permission System

```sql
CREATE TABLE role (
    id UUID PRIMARY KEY,
    name VARCHAR(64) NOT NULL UNIQUE
);
INSERT INTO role (id, name) VALUES ('00000000-0000-0000-0000-000000000001', 'anonymous user');
INSERT INTO role (id, name) VALUES ('00000000-0000-0000-0000-000000000002', 'authenticated user');

CREATE TABLE role_permission (
    role_id UUID NOT NULL REFERENCES role(id),
    permission VARCHAR(128) NOT NULL,
    PRIMARY KEY (role_id, permission)
);

CREATE TABLE users_roles (
    user_id UUID NOT NULL REFERENCES users(id),
    role_id UUID NOT NULL REFERENCES role(id),
    PRIMARY KEY (user_id, role_id)
);
```

Permissions are loaded into a `DashMap` at startup for fast lookups:

```rust
/// Well-known role UUIDs (set during installation)
const ANONYMOUS_ROLE_ID: &str = "00000000-0000-0000-0000-000000000001";
const AUTHENTICATED_ROLE_ID: &str = "00000000-0000-0000-0000-000000000002";

pub async fn user_has_permission(
    state: &AppState, user: &User, permission: &str,
) -> bool {
    if user.is_superuser() { return true; }

    let anon_id: Uuid = ANONYMOUS_ROLE_ID.parse().unwrap();
    let auth_id: Uuid = AUTHENTICATED_ROLE_ID.parse().unwrap();

    if user.is_anonymous() {
        return state.permissions.get(&(anon_id, permission.to_string()))
            .map(|v| *v).unwrap_or(false);
    }

    if state.permissions.get(&(auth_id, permission.to_string()))
        .map(|v| *v).unwrap_or(false)
    { return true; }

    let roles: Vec<Uuid> = sqlx::query_scalar(
        "SELECT role_id FROM users_roles WHERE user_id = $1"
    ).bind(user.id).fetch_all(&state.db).await.unwrap_or_default();

    roles.iter().any(|role_id| {
        state.permissions.get(&(*role_id, permission.to_string()))
            .map(|v| *v).unwrap_or(false)
    })
}
```

---

