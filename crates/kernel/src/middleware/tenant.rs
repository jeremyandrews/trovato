//! Tenant resolution middleware.
//!
//! Resolves the active tenant for each request and stores it in
//! request extensions as `TenantContext`. Runs after auth middleware,
//! before route handlers.
//!
//! Resolution strategies:
//! - `default`: always resolves to `DEFAULT_TENANT_ID` (zero overhead for single-tenant)
//! - `subdomain`: `tenant-a.example.com` → look up by machine_name
//! - `path_prefix`: `/t/tenant-a/...` → strip prefix and resolve
//! - `header`: `X-Tenant-ID: {uuid}` → direct UUID resolution

use axum::{body::Body, http::Request, middleware::Next, response::Response};

use crate::models::tenant::{DEFAULT_TENANT_ID, TenantContext};

/// Resolve the tenant for the current request.
///
/// The resolution method is controlled by `TENANT_RESOLUTION_METHOD` env var.
/// Default is `"default"` — always returns `DEFAULT_TENANT_ID` with zero
/// database overhead (static `TenantContext` construction).
pub async fn resolve_tenant(mut request: Request<Body>, next: Next) -> Response {
    let method = std::env::var("TENANT_RESOLUTION_METHOD").unwrap_or_default();

    let tenant_context = match method.as_str() {
        "header" => resolve_from_header(&request),
        // "subdomain" and "path_prefix" require database lookups —
        // deferred until a multi-tenant deployment needs them.
        _ => TenantContext::default_tenant(),
    };

    request.extensions_mut().insert(tenant_context);
    next.run(request).await
}

/// Resolve tenant from `X-Tenant-ID` header (UUID).
fn resolve_from_header(request: &Request<Body>) -> TenantContext {
    if let Some(header_val) = request.headers().get("x-tenant-id")
        && let Ok(id_str) = header_val.to_str()
        && let Ok(id) = uuid::Uuid::parse_str(id_str)
        && id != DEFAULT_TENANT_ID
    {
        // Non-default tenant — return with header-provided ID.
        // Full tenant name/machine_name lookup deferred to DB integration.
        return TenantContext {
            id,
            name: String::new(),
            machine_name: String::new(),
        };
    }
    TenantContext::default_tenant()
}
