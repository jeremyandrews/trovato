//! The kernel's JSON API convention: one versioned prefix, one document.
//!
//! Every kernel JSON endpoint is declared through [`ApiRoutes`], which takes
//! the handler and its OpenAPI description together. The same value mounts
//! the route under [`PREFIX`] (`/api/v1`) with the `x-api-version` header, and
//! lists it in `/api/v1/openapi.json`, so the document is generated from the
//! router rather than kept beside it.
//!
//! A route that was served before the prefix existed keeps its old
//! unversioned path, `/api/...`, as an alias ([`Doc::alias`]): the same
//! handler, answered with a `Deprecation` header and marked deprecated in the
//! document. The aliases are removed at 2.0.
//!
//! Plugin routes (`api` menu entries) are not declared here and keep their
//! paths; [`super::route_metadata`] lists them beside these.

use axum::{
    Router,
    body::Body,
    extract::State,
    handler::Handler,
    http::{HeaderValue, Method, Request},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{MethodFilter, MethodRouter, on},
};

use super::route_metadata::{ParamMeta, RouteMetadata};
use crate::state::AppState;

/// The prefix every kernel JSON endpoint is served under.
pub const PREFIX: &str = "/api/v1";

/// The unversioned prefix the deprecated aliases keep.
pub const LEGACY_PREFIX: &str = "/api";

/// The OpenAPI description of one route, declared beside it.
#[derive(Debug, Clone)]
pub struct Doc {
    summary: String,
    description: Option<String>,
    query: Vec<ParamMeta>,
    response_type: &'static str,
    alias: bool,
}

/// Start the description of a route with its one-line summary.
pub fn doc(summary: &str) -> Doc {
    Doc {
        summary: summary.to_string(),
        description: None,
        query: Vec::new(),
        response_type: "application/json",
        alias: false,
    }
}

impl Doc {
    /// An optional query parameter.
    pub fn query(mut self, name: &str, description: &str) -> Self {
        self.query.push(param(name, "query", false, description));
        self
    }

    /// A required query parameter.
    pub fn required(mut self, name: &str, description: &str) -> Self {
        self.query.push(param(name, "query", true, description));
        self
    }

    /// The four pagination parameters every list takes
    /// ([`crate::pagination::PageParams`]).
    pub fn paged(self, default: i64, max: i64) -> Self {
        self.query("page", "Page number, from 1 (default 1)")
            .query(
                "per_page",
                &format!("Entries per page (default {default}, max {max})"),
            )
            .query("limit", "Alias of per_page")
            .query("offset", "Entries to skip; alias of page")
    }

    /// A longer description.
    pub fn describe(mut self, description: &str) -> Self {
        self.description = Some(description.to_string());
        self
    }

    /// The route answers with a server-sent event stream.
    pub fn stream(mut self) -> Self {
        self.response_type = "text/event-stream";
        self
    }

    /// Keep the route's pre-`/api/v1` path, `/api` + its path, as a
    /// deprecated alias.
    pub fn alias(mut self) -> Self {
        self.alias = true;
        self
    }
}

fn param(name: &str, location: &str, required: bool, description: &str) -> ParamMeta {
    ParamMeta {
        name: name.to_string(),
        location: location.to_string(),
        required,
        description: description.to_string(),
    }
}

/// A set of kernel JSON routes and their descriptions.
#[must_use]
pub struct ApiRoutes {
    tag: &'static str,
    plugin: Option<&'static str>,
    v1: Router<AppState>,
    legacy: Router<AppState>,
    docs: Vec<RouteMetadata>,
    has_legacy: bool,
    children: Vec<ApiRoutes>,
}

impl ApiRoutes {
    /// An empty set whose routes are grouped under `tag` in the document.
    pub fn new(tag: &'static str) -> Self {
        Self {
            tag,
            plugin: None,
            v1: Router::new(),
            legacy: Router::new(),
            docs: Vec::new(),
            has_legacy: false,
            children: Vec::new(),
        }
    }

    /// `GET` `path` (relative to [`PREFIX`]).
    pub fn get<H, T>(self, path: &str, handler: H, doc: Doc) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.route(Method::GET, path, on(MethodFilter::GET, handler), doc)
    }

    /// `POST` `path` (relative to [`PREFIX`]).
    pub fn post<H, T>(self, path: &str, handler: H, doc: Doc) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.route(Method::POST, path, on(MethodFilter::POST, handler), doc)
    }

    /// `PUT` `path` (relative to [`PREFIX`]).
    pub fn put<H, T>(self, path: &str, handler: H, doc: Doc) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.route(Method::PUT, path, on(MethodFilter::PUT, handler), doc)
    }

    /// `DELETE` `path` (relative to [`PREFIX`]).
    pub fn delete<H, T>(self, path: &str, handler: H, doc: Doc) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.route(Method::DELETE, path, on(MethodFilter::DELETE, handler), doc)
    }

    /// A route whose method router is built by the caller, for one that
    /// carries a layer of its own (a body limit). `method` must be the one
    /// `route` answers; the served-paths test requests every listed path with
    /// its listed method.
    pub fn route(
        mut self,
        method: Method,
        path: &str,
        route: MethodRouter<AppState>,
        doc: Doc,
    ) -> Self {
        assert!(
            path.starts_with('/') && !path.starts_with(PREFIX),
            "API paths are declared relative to {PREFIX}: {path}"
        );
        let path_params: Vec<ParamMeta> = path
            .split('/')
            .filter_map(|seg| seg.strip_prefix('{')?.strip_suffix('}'))
            .map(|name| param(name, "path", true, ""))
            .collect();
        let parameters: Vec<ParamMeta> = path_params.into_iter().chain(doc.query).collect();
        let versioned = format!("{PREFIX}{path}");

        if doc.alias {
            let legacy = format!("{LEGACY_PREFIX}{path}");
            self.legacy = self.legacy.route(&legacy, route.clone());
            self.has_legacy = true;
            self.docs.push(RouteMetadata {
                method: method.to_string(),
                path: legacy,
                summary: doc.summary.clone(),
                parameters: parameters.clone(),
                response_type: doc.response_type.to_string(),
                tags: vec![self.tag.to_string()],
                deprecated: true,
                description: Some(format!(
                    "Deprecated alias of `{versioned}`, removed in 2.0."
                )),
            });
        }
        self.v1 = self.v1.route(path, route);
        self.docs.push(RouteMetadata {
            method: method.to_string(),
            path: versioned,
            summary: doc.summary,
            parameters,
            response_type: doc.response_type.to_string(),
            tags: vec![self.tag.to_string()],
            deprecated: false,
            description: doc.description,
        });
        self
    }

    /// Serve these routes only while `plugin` is enabled: 404 otherwise, as
    /// [`super::gated_plugin_routes`] answers for the plugin's other routes.
    pub fn plugin(mut self, plugin: &'static str) -> Self {
        self.plugin = Some(plugin);
        let note = format!("Served while the `{plugin}` plugin is enabled.");
        for doc in &mut self.docs {
            doc.description = Some(match doc.description.take() {
                Some(text) => format!("{text} {note}"),
                None => note.clone(),
            });
        }
        self
    }

    /// Add another set's routes. Its tag and plugin gate stay with its own
    /// routes.
    pub fn merge(mut self, other: ApiRoutes) -> Self {
        self.children.push(other);
        self
    }

    /// The routes' OpenAPI descriptions: each versioned path and any alias.
    pub fn docs(&self) -> Vec<RouteMetadata> {
        let mut docs = self.docs.clone();
        for child in &self.children {
            docs.extend(child.docs());
        }
        docs
    }

    /// The router: every route under [`PREFIX`] with the version header, and
    /// the aliases at their old paths with the `Deprecation` header too, each
    /// behind its set's plugin gate.
    pub fn into_router(self, state: &AppState) -> Router<AppState> {
        let (v1, legacy) = self.routers(state);
        let mut router =
            Router::new().nest(PREFIX, v1.layer(axum::middleware::from_fn(api_version)));
        if let Some(legacy) = legacy {
            router = router.merge(legacy.layer(axum::middleware::from_fn(deprecated_alias)));
        }
        router
    }

    /// The versioned routes (relative to [`PREFIX`]) and the aliases, gated.
    fn routers(self, state: &AppState) -> (Router<AppState>, Option<Router<AppState>>) {
        let plugin = self.plugin;
        let gate = |router: Router<AppState>| match plugin {
            Some(plugin) => router.route_layer(axum::middleware::from_fn_with_state(
                (state.clone(), plugin),
                plugin_gate,
            )),
            None => router,
        };
        let mut v1 = if self.docs.is_empty() {
            self.v1
        } else {
            gate(self.v1)
        };
        let mut legacy = self.has_legacy.then(|| gate(self.legacy));
        for child in self.children {
            let (child_v1, child_legacy) = child.routers(state);
            v1 = v1.merge(child_v1);
            legacy = match (legacy, child_legacy) {
                (Some(a), Some(b)) => Some(a.merge(b)),
                (a, b) => a.or(b),
            };
        }
        (v1, legacy)
    }
}

/// 404 unless `plugin` is enabled.
async fn plugin_gate(
    State((state, plugin)): State<(AppState, &'static str)>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if state.is_plugin_enabled(plugin) {
        next.run(req).await
    } else {
        crate::error::AppError::with_status(
            axum::http::StatusCode::NOT_FOUND,
            format!("Plugin '{plugin}' is not enabled."),
        )
        .into_response()
    }
}

/// Mark every versioned answer with the API version.
async fn api_version(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert("x-api-version", HeaderValue::from_static("1"));
    response
}

/// Mark an alias's answer deprecated (RFC 9745), and with the version it
/// answers as.
async fn deprecated_alias(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("x-api-version", HeaderValue::from_static("1"));
    headers.insert("deprecation", HeaderValue::from_static("true"));
    response
}
