//! Health probes (`docs/design/upgrades-and-probes.md` §5).
//!
//! - `/health/live`: the process answers. Checks nothing, so a database blip
//!   never makes Kubernetes restart a pod.
//! - `/health/ready`: Postgres and Redis answer and the server is not draining.
//!   The full application only serves once the schema it needs is there and
//!   its plugins are loaded; before that, [`waiting_router`] answers 503.
//! - `/health`: as it always was, 200 when Postgres and Redis answer.
//!
//! Postgres and Redis are checked at most once a second per process, behind a
//! single-flight lock, whatever the probe rate. An anonymous caller gets only
//! the status word; pool statistics and the services report go to a loopback
//! caller with no forwarding header, or to the holder of `HEALTH_TOKEN`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::state::AppState;

/// The probe paths, whatever the method. Middleware that only rewrites or
/// gates by path (maintenance, redirects, aliases, language, the installer
/// check) asks this; middleware that costs a session or a rate-limit bucket
/// asks [`is_probe`], which exempts reads only.
pub fn is_probe_path(path: &str) -> bool {
    matches!(path, "/health" | "/health/live" | "/health/ready")
}

/// A probe request: `GET` or `HEAD` on a probe path. The exemptions that
/// remove a cost a visitor would pay (rate limits, credential lookups, the
/// session index) ask this, so any other method on these paths, which answers
/// 405 through the themed error page, is charged like any other request.
pub fn is_probe(method: &axum::http::Method, path: &str) -> bool {
    matches!(*method, axum::http::Method::GET | axum::http::Method::HEAD) && is_probe_path(path)
}

/// How long one dependency check is reused.
const CHECK_TTL: Duration = Duration::from_secs(1);

/// How long one dependency check may take before it counts as down.
const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

/// Readiness state shared by the probes and the shutdown path.
pub struct Probes {
    draining: AtomicBool,
    last: tokio::sync::Mutex<Option<(Instant, Dependencies)>>,
    token: Option<String>,
}

#[derive(Clone, Copy, Debug)]
struct Dependencies {
    postgres: bool,
    redis: bool,
}

impl Probes {
    /// `token` is `HEALTH_TOKEN`: its holder sees the details from anywhere.
    pub fn new(token: Option<String>) -> Self {
        Self {
            draining: AtomicBool::new(false),
            last: tokio::sync::Mutex::new(None),
            token,
        }
    }

    /// Report not ready from now on (at the shutdown signal).
    pub fn start_draining(&self) {
        self.draining.store(true, Ordering::SeqCst);
    }

    /// Whether shutdown has begun.
    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// Postgres and Redis, checked at most once per [`CHECK_TTL`]. Concurrent
    /// callers wait for the check in progress and share its answer.
    async fn dependencies(&self, state: &AppState) -> Dependencies {
        let mut last = self.last.lock().await;
        if let Some((at, deps)) = *last
            && at.elapsed() < CHECK_TTL
        {
            return deps;
        }
        let (postgres, redis) = tokio::join!(
            tokio::time::timeout(CHECK_TIMEOUT, state.postgres_healthy()),
            tokio::time::timeout(CHECK_TIMEOUT, state.redis_healthy()),
        );
        let deps = Dependencies {
            postgres: postgres.unwrap_or(false),
            redis: redis.unwrap_or(false),
        };
        *last = Some((Instant::now(), deps));
        deps
    }

    /// Whether the caller may see the details: the token holder, or a loopback
    /// peer that is not a reverse proxy relaying someone else.
    fn is_trusted(&self, request: &Request) -> bool {
        let headers = request.headers();
        if let Some(expected) = &self.token
            && let Some(given) = bearer(headers)
            // Digests, so the comparison is constant time in the length too.
            && bool::from(Sha256::digest(given.as_bytes()).ct_eq(&Sha256::digest(expected.as_bytes())))
        {
            return true;
        }
        let loopback = request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .is_some_and(|ConnectInfo(addr)| addr.ip().is_loopback());
        let forwarded = ["x-forwarded-for", "forwarded", "x-real-ip"]
            .iter()
            .any(|h| headers.contains_key(*h));
        loopback && !forwarded
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// A probe answer: never cached by anything in between.
fn answer(status: StatusCode, body: serde_json::Value) -> Response {
    (status, [(header::CACHE_CONTROL, "no-store")], Json(body)).into_response()
}

fn status_code(ok: bool) -> StatusCode {
    if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// Database connection pool statistics, for a trusted caller.
#[derive(Debug, Serialize)]
struct PoolHealth {
    /// Current number of connections in the pool.
    size: u32,
    /// Number of idle (unused) connections.
    idle: u32,
    /// Number of actively used connections.
    active: u32,
    /// Pool utilization as a percentage (active / size * 100).
    utilization_pct: u32,
}

async fn live() -> Response {
    answer(StatusCode::OK, json!({"status": "alive"}))
}

async fn ready(State(state): State<AppState>, request: Request) -> Response {
    let probes = state.probes();
    let deps = probes.dependencies(&state).await;
    let draining = probes.is_draining();
    let ok = deps.postgres && deps.redis && !draining;
    let word = if ok { "ready" } else { "not ready" };
    if !probes.is_trusted(&request) {
        return answer(status_code(ok), json!({"status": word}));
    }
    answer(
        status_code(ok),
        json!({
            "status": word,
            "postgres": deps.postgres,
            "redis": deps.redis,
            "draining": draining,
        }),
    )
}

async fn health(State(state): State<AppState>, request: Request) -> Response {
    let probes = state.probes();
    let deps = probes.dependencies(&state).await;
    let ok = deps.postgres && deps.redis;
    let word = if ok { "healthy" } else { "unhealthy" };
    if !probes.is_trusted(&request) {
        return answer(status_code(ok), json!({"status": word}));
    }

    let size = state.db().size();
    let idle = state.db().num_idle() as u32;
    let active = size.saturating_sub(idle);
    let pool = PoolHealth {
        size,
        idle,
        active,
        utilization_pct: (active * 100).checked_div(size).unwrap_or(0),
    };
    // The full service report (circuit breaker states and so on).
    // Bounded like the cached checks: two of them, uncached, run inside it.
    let services = tokio::time::timeout(CHECK_TIMEOUT * 2, state.health_report())
        .await
        .ok();
    answer(
        status_code(ok),
        json!({
            "status": word,
            "postgres": deps.postgres,
            "redis": deps.redis,
            "pool": pool,
            "services": services,
        }),
    )
}

/// The probe routes of the running application.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
}

/// What `serve` answers while it waits for its schema
/// (`MIGRATE_ON_START=false`): alive, not ready, nothing else.
pub fn waiting_router() -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route(
            "/health/ready",
            get(|| async {
                answer(
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({"status": "not ready"}),
                )
            }),
        )
        .route(
            "/health",
            get(|| async {
                answer(
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({"status": "unhealthy"}),
                )
            }),
        )
        .fallback(|| async { StatusCode::SERVICE_UNAVAILABLE })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn request(peer: impl Into<std::net::IpAddr>, headers: &[(&str, &str)]) -> Request {
        let mut builder = axum::http::Request::get("/health");
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        let mut request = builder.body(axum::body::Body::empty()).unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((peer.into(), 4000))));
        request
    }

    #[test]
    fn only_an_operator_or_the_token_holder_is_trusted() {
        let probes = Probes::new(Some("s3cret".to_string()));
        assert!(probes.is_trusted(&request([127, 0, 0, 1], &[])));
        assert!(probes.is_trusted(&request(std::net::Ipv6Addr::LOCALHOST, &[])));
        assert!(!probes.is_trusted(&request(
            std::net::Ipv6Addr::LOCALHOST,
            &[("x-forwarded-for", "203.0.113.9")]
        )));
        assert!(!probes.is_trusted(&request([203, 0, 113, 9], &[])));
        assert!(!probes.is_trusted(&request(
            [127, 0, 0, 1],
            &[("x-forwarded-for", "203.0.113.9")]
        )));
        assert!(!probes.is_trusted(&request([127, 0, 0, 1], &[("forwarded", "for=x")])));
        assert!(!probes.is_trusted(&request([127, 0, 0, 1], &[("x-real-ip", "x")])));
        assert!(probes.is_trusted(&request(
            [203, 0, 113, 9],
            &[("authorization", "Bearer s3cret")]
        )));
        assert!(!probes.is_trusted(&request(
            [203, 0, 113, 9],
            &[("authorization", "Bearer s3cre")]
        )));
        // No token configured: a bearer header earns nothing.
        let open = Probes::new(None);
        assert!(!open.is_trusted(&request([203, 0, 113, 9], &[("authorization", "Bearer ")])));
    }

    #[test]
    fn probe_paths_are_exact() {
        for path in ["/health", "/health/live", "/health/ready"] {
            assert!(is_probe_path(path), "{path}");
        }
        for path in ["/health/", "/health/deep", "/healthz", "/"] {
            assert!(!is_probe_path(path), "{path}");
        }
    }

    #[test]
    fn only_reads_of_a_probe_path_are_probes() {
        use axum::http::Method;
        for method in [Method::GET, Method::HEAD] {
            assert!(is_probe(&method, "/health"), "{method}");
            assert!(is_probe(&method, "/health/ready"), "{method}");
        }
        for method in [Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS] {
            assert!(!is_probe(&method, "/health"), "{method}");
        }
        assert!(!is_probe(&Method::GET, "/healthz"));
    }
}
