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

use axum::{body::Body, extract::State, http::Request, middleware::Next, response::Response};

use crate::models::tenant::{DEFAULT_TENANT_ID, TenantContext};
use crate::state::AppState;

/// How the active tenant is resolved for a request.
///
/// Resolved once at startup from `TENANT_RESOLUTION_METHOD` rather than read on
/// every request, so the strategy is an input a caller sets rather than a
/// process-global. An unrecognized value is [`Self::Default`]: a typo must not
/// silently enable header-driven tenant selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TenantResolution {
    /// Always [`DEFAULT_TENANT_ID`], with no database overhead.
    #[default]
    Default,
    /// Read `X-Tenant-ID` from the request.
    ///
    /// Only meaningful behind a proxy that sets the header itself — the kernel
    /// trusts it, so it must not be reachable from a client.
    Header,
}

impl TenantResolution {
    /// Resolve the strategy from a settings lookup.
    ///
    /// `subdomain` and `path_prefix` are named in the module docs but require
    /// database lookups, so they are not implemented and fall back to
    /// [`Self::Default`] like any other unrecognized value.
    pub(crate) fn from_lookup(lookup: crate::config::Lookup<'_>) -> Self {
        match lookup("TENANT_RESOLUTION_METHOD")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "header" => Self::Header,
            _ => Self::Default,
        }
    }
}

/// Resolve the tenant for the current request.
///
/// The strategy comes from [`TenantResolution`] on the application state; the
/// default costs one static `TenantContext` construction and no database work.
pub async fn resolve_tenant(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let tenant_context = match state.runtime().tenant_resolution {
        TenantResolution::Header => resolve_from_header(&request),
        TenantResolution::Default => TenantContext::default_tenant(),
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Resolve the strategy from an explicit settings map, no globals involved.
    fn from_map(value: Option<&str>) -> TenantResolution {
        TenantResolution::from_lookup(&|name| match name {
            "TENANT_RESOLUTION_METHOD" => value.map(str::to_string),
            _ => None,
        })
    }

    /// Nothing configured means the single-tenant fast path.
    #[test]
    fn unconfigured_resolves_to_the_default_tenant() {
        assert_eq!(from_map(None), TenantResolution::Default);
        assert_eq!(TenantResolution::default(), TenantResolution::Default);
    }

    #[test]
    fn header_strategy_is_recognized_case_insensitively() {
        for spelling in ["header", "HEADER", " Header "] {
            assert_eq!(
                from_map(Some(spelling)),
                TenantResolution::Header,
                "{spelling:?} should select the header strategy"
            );
        }
    }

    /// A typo, or one of the two documented-but-unimplemented strategies, must
    /// fail closed to the default rather than enabling header-driven selection.
    #[test]
    fn unrecognized_strategies_fail_closed() {
        for value in ["", "subdomain", "path_prefix", "headers", "hdr", "1"] {
            assert_eq!(
                from_map(Some(value)),
                TenantResolution::Default,
                "{value:?} must not enable header resolution"
            );
        }
    }
}
