//! HTTP request host function for WASM plugins.
//!
//! Provides `request` under the `trovato:kernel/http` WIT interface.
//! The kernel executes outbound HTTP requests on behalf of plugins,
//! enforcing timeouts and URL restrictions. Plugins cannot make direct
//! network calls from WASM.
//!
//! # Security
//!
//! No per-user permission check is enforced here because this function
//! is infrastructure: cron tasks run as anonymous, and user-facing
//! actions should gate on permissions at the plugin/route level. The
//! kernel enforces SSRF protections and resource limits (timeout, body
//! size) instead.
//!
//! The SSRF fence has three layers, all kernel-internal (p11i / G1):
//! 1. A pre-send string/literal check ([`resolve_outbound_client`]): scheme,
//!    internal hostnames, and private/reserved IP literals.
//! 2. A validating DNS resolver ([`ValidatingResolver`]) that resolves the
//!    hostname, rejects the request if any resolved address is private, and
//!    pins the connection to the checked addresses — closing the DNS-rebinding
//!    time-of-check/time-of-use window (a hostname that clears the string check
//!    but resolves to a private address at connect time).
//! 3. A redirect policy ([`revalidating_redirect_policy`]) that re-runs the
//!    literal check on every hop (and, via layer 2, revalidates each hop's
//!    resolved addresses), so a 3xx to a private target such as the cloud
//!    metadata endpoint is denied mid-chain. The 10-hop cap is preserved.
//!
//! Every kernel path that issues plugin-influenced HTTP (one-shot `request`,
//! streaming `http-open`, and the queue-worker / cron dispatch clients), and the
//! administrator-configured AI provider client
//! ([`crate::services::ai_provider::AiProviderService`], used for chat,
//! streaming chat, tool calling, embeddings, the connection test and the plugin
//! `ai-request` call), is built from [`build_outbound_client`] /
//! [`hardened_outbound_builder`], so the fence is shared, not duplicated.
//!
//! All three layers classify addresses through the one shared policy in
//! [`crate::net_policy`], and they classify the host `url` already parsed
//! rather than re-parsing its string form — which is what let bracketed IPv6
//! literals past every one of them at once.
//!
//! # The operator allowlist (`PLUGIN_HTTP_PRIVATE_ALLOW`)
//!
//! An operator can name one plugin, one private/loopback destination and one
//! port that plugin may reach ([`crate::plugin_http_allow`]), for example a
//! self-hosted Varnish cache. [`resolve_outbound_client`] is where this is
//! consulted: it is layer 1's job now, deciding, for a URL layer 1 would
//! otherwise refuse, whether an entry names exactly this plugin, port and
//! destination and, if so, handing back that entry's own client, built once
//! at startup by [`build_plugin_http_allow_runtime`], with a resolver that
//! relaxes only for a hostname entry's own name, and the same per-hop redirect
//! revalidation (layer 3) every client carries, so a redirect away from the
//! granted destination is refused exactly as it would be with no entry at
//! all. Every other plugin, port and destination meets the same refusal as
//! before: the default fence ([`ValidatingResolver`], `revalidating_redirect_policy`) is unchanged.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tracing::warn;
use url::Url;
use wasmtime::Linker;

use super::trace::{HostCallGuard, TracedLinker};

use crate::net_policy::is_disallowed_ip;
use crate::plugin::{PluginState, WasmtimeExt};
use crate::plugin_http_allow::{self, PluginHttpAllow};
use trovato_sdk::host_errors;

use super::{read_string_from_memory, write_bytes_to_memory, write_string_to_memory};

/// Maximum allowed timeout for plugin HTTP requests (60 seconds).
const MAX_TIMEOUT_MS: u32 = 60_000;

/// Default timeout for plugin HTTP requests (30 seconds).
const DEFAULT_TIMEOUT_MS: u32 = 30_000;

/// Maximum response body size (1 MB).
const MAX_RESPONSE_BODY: usize = 1_024 * 1_024;

// -----------------------------------------------------------------------------
// Streaming fetch (`http-open`/`http-read`/`http-close`) — P11e / D-49, D-50
// -----------------------------------------------------------------------------

/// Default total-transfer ceiling for a streaming fetch when a plugin declares no
/// `http_max_transfer` (1 MB — matches the `request` one-shot cap, so a plugin
/// that never sets it sees today's limit). D-50.
pub(crate) const DEFAULT_TRANSFER_CEILING: u64 = 1_024 * 1_024;

/// Kernel hard maximum for the manifest-declared total-transfer ceiling (16 MB).
/// A manifest can never grant more than this regardless of what it declares. D-50.
pub(crate) const MAX_TRANSFER_CEILING: u64 = 16 * 1_024 * 1_024;

/// Maximum bytes returned by a single `http-read` — the 64 KB tap I/O buffer.
/// A read never yields more than this even if the plugin offers a larger buffer,
/// so the per-read size can never exceed the tap buffer (D-49).
const MAX_READ_CHUNK: usize = 64 * 1_024;

/// Maximum concurrent open streaming handles per tap invocation (`Store`). Bounds
/// how many live connections one call can pin; excess `http-open` calls fail with
/// [`host_errors::ERR_HTTP_TOO_MANY_HANDLES`].
pub(crate) const MAX_OPEN_HTTP_STREAMS: usize = 8;

/// Clamp a manifest-declared streaming total-transfer ceiling to the kernel range
/// (P11e / D-50): an absent declaration yields [`DEFAULT_TRANSFER_CEILING`] (1 MB),
/// and any declared value is bounded to `[1, `[`MAX_TRANSFER_CEILING`]`]` so a
/// manifest can never grant more than the kernel maximum (16 MB) nor a nonsense
/// zero. Kept here, beside the constants, as the single home for the policy.
pub(crate) fn clamp_transfer_ceiling(declared: Option<u64>) -> u64 {
    declared
        .unwrap_or(DEFAULT_TRANSFER_CEILING)
        .clamp(1, MAX_TRANSFER_CEILING)
}

/// Total-transfer wall-clock budget for a streaming fetch, across all reads
/// (D-50: the old 60 s ceiling becomes this budget). Enforced alongside the
/// per-read timeout (`HttpRequest.timeout_ms`); a background tap is additionally
/// bounded by the 150 s epoch. The epoch cuts CPU; this budget cuts the wire.
const TRANSFER_BUDGET: Duration = Duration::from_millis(60_000);

/// Register HTTP host functions with the WASM linker.
///
/// Provides `request` (one-shot, unchanged) plus the additive streaming trio
/// `http-open` / `http-read` / `http-close` (P11e / D-49) under
/// `trovato:kernel/http`.
pub fn register_http_functions(linker: &mut Linker<PluginState>) -> Result<()> {
    linker
        .func_wrap_async_traced(
            "trovato:kernel/http",
            "request",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (req_ptr, req_len, out_ptr, out_max_len): (i32, i32, i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };

                    // Read request JSON from WASM memory
                    let Ok(request_json) =
                        read_string_from_memory(&memory, &caller, req_ptr, req_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };

                    let Some(services) = caller.data().request.services() else {
                        return host_errors::ERR_NO_SERVICES;
                    };
                    let http = services.http.clone();
                    let http_allow = services.http_allow.clone();
                    let plugin_name = caller.data().plugin_name.clone();

                    // Deserialize request
                    let request: trovato_sdk::types::HttpRequest =
                        match serde_json::from_str(&request_json) {
                            Ok(r) => r,
                            Err(e) => {
                                warn!(
                                    plugin = %plugin_name,
                                    error = %e,
                                    "invalid HttpRequest JSON from plugin"
                                );
                                return host_errors::ERR_PARAM_DESERIALIZE;
                            }
                        };

                    // Validate URL (scheme, host, SSRF protections) and pick
                    // the client: the default, or a plugin-allowlist entry's.
                    let client = match resolve_outbound_client(
                        &request.url,
                        &plugin_name,
                        &http,
                        &http_allow,
                    ) {
                        Ok(c) => c,
                        Err(code) => return code,
                    };

                    // Execute request
                    let response = match execute_http_request(client, &request, &plugin_name).await
                    {
                        Ok(r) => r,
                        Err(code) => return code,
                    };

                    // Serialize response
                    let Ok(response_json) = serde_json::to_string(&response) else {
                        return host_errors::ERR_SERIALIZE_FAILED;
                    };

                    // Guard against silent truncation
                    if response_json.len() > out_max_len as usize {
                        warn!(
                            plugin = %plugin_name,
                            response_len = response_json.len(),
                            buffer_max = out_max_len,
                            "HTTP response exceeds output buffer"
                        );
                        return host_errors::ERR_HTTP_RESPONSE_TOO_LARGE;
                    }

                    write_string_to_memory(
                        &memory,
                        &mut caller,
                        out_ptr,
                        out_max_len,
                        &response_json,
                    )
                    .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT)
                })
            },
        )
        .into_anyhow()?;

    // http-open(req_ptr, req_len, out_ptr, out_max_len) -> i32: open a streaming
    // fetch and write the response metadata (JSON `HttpOpenResponse`
    // `{handle, status, headers}`) to the output buffer, returning the bytes
    // written (>= 0) or a negative error code (P11e / D-49; metadata p11j /
    // G-HTTP-META). The handle travels in the JSON, not the return value, so the
    // int-return convention matches the one-shot `request` (bytes written) — one
    // vocabulary for both HTTP entry points.
    linker
        .func_wrap_async_traced(
            "trovato:kernel/http",
            "http-open",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (req_ptr, req_len, out_ptr, out_max_len): (i32, i32, i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };

                    let Ok(request_json) =
                        read_string_from_memory(&memory, &caller, req_ptr, req_len)
                    else {
                        return host_errors::ERR_PARAM1_READ;
                    };

                    let Some(services) = caller.data().request.services() else {
                        return host_errors::ERR_NO_SERVICES;
                    };
                    let http = services.http.clone();
                    let http_allow = services.http_allow.clone();
                    let plugin_name = caller.data().plugin_name.clone();
                    let max_transfer = caller.data().http_max_transfer;

                    let request: trovato_sdk::types::HttpRequest =
                        match serde_json::from_str(&request_json) {
                            Ok(r) => r,
                            Err(e) => {
                                warn!(
                                    plugin = %plugin_name,
                                    error = %e,
                                    "invalid HttpRequest JSON from plugin (http-open)"
                                );
                                return host_errors::ERR_PARAM_DESERIALIZE;
                            }
                        };

                    let stream =
                        match open_stream(&http, &http_allow, &request, &plugin_name, max_transfer)
                            .await
                        {
                            Ok(s) => s,
                            Err(code) => return code,
                        };

                    // Capture the metadata before the stream is moved into the
                    // Store; the handle is only known after insert, so the
                    // response is assembled once the slot is allocated.
                    let status = stream.status;
                    let headers = stream.headers.clone();

                    let Some(handle) = caller.data_mut().http_stream_insert(stream) else {
                        warn!(
                            plugin = %plugin_name,
                            max = MAX_OPEN_HTTP_STREAMS,
                            "too many concurrent streaming HTTP handles"
                        );
                        return host_errors::ERR_HTTP_TOO_MANY_HANDLES;
                    };

                    let meta_json = match build_open_metadata(handle, status, headers, out_max_len)
                    {
                        Ok(json) => json,
                        Err(code) => {
                            // The metadata could not be delivered; free the slot so
                            // the handle does not leak past this failed open.
                            if code == host_errors::ERR_HTTP_RESPONSE_TOO_LARGE {
                                warn!(
                                    plugin = %plugin_name,
                                    buffer_max = out_max_len,
                                    "streaming HTTP open metadata exceeds output buffer"
                                );
                            }
                            caller.data_mut().http_stream_close(handle);
                            return code;
                        }
                    };

                    write_string_to_memory(&memory, &mut caller, out_ptr, out_max_len, &meta_json)
                        .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT)
                })
            },
        )
        .into_anyhow()?;

    // http-read(handle, out_ptr, out_max_len) -> i32: bytes written (0 = EOF),
    // or a negative error code (P11e / D-49).
    linker
        .func_wrap_async_traced(
            "trovato:kernel/http",
            "http-read",
            |mut caller: wasmtime::Caller<'_, PluginState>,
             (handle, out_ptr, out_max_len): (i32, i32, i32)| {
                Box::new(async move {
                    let Some(wasmtime::Extern::Memory(memory)) = caller.get_export("memory") else {
                        return host_errors::ERR_MEMORY_MISSING;
                    };

                    let handle = handle as u32;
                    let max_len = out_max_len.max(0) as usize;

                    // Scope the &mut PluginState borrow to the read so the caller
                    // is free for the memory write below. The borrow is held across
                    // the await (fine: single Store), then released here.
                    let read_result = {
                        let Some(stream) = caller.data_mut().http_stream_get(handle) else {
                            return host_errors::ERR_HTTP_HANDLE_INVALID;
                        };
                        stream.read_chunk(max_len).await
                    };

                    match read_result {
                        Ok(bytes) => write_bytes_to_memory(
                            &memory,
                            &mut caller,
                            out_ptr,
                            out_max_len,
                            &bytes,
                        )
                        .unwrap_or(host_errors::ERR_PARAM2_OR_OUTPUT),
                        Err(code) => {
                            // A read error is terminal: drop the dead stream so its
                            // slot frees and a retry fails cleanly as handle-invalid.
                            caller.data_mut().http_stream_close(handle);
                            code
                        }
                    }
                })
            },
        )
        .into_anyhow()?;

    // http-close(handle) -> i32: 0 on success, ERR_HTTP_HANDLE_INVALID if the
    // handle is unknown or already closed (P11e / D-49).
    linker
        .func_wrap(
            "trovato:kernel/http",
            "http-close",
            |mut caller: wasmtime::Caller<'_, PluginState>, handle: i32| {
                let _trace = HostCallGuard::new(&caller, "trovato:kernel/http", "http-close");
                if caller.data_mut().http_stream_close(handle as u32) {
                    0
                } else {
                    host_errors::ERR_HTTP_HANDLE_INVALID
                }
            },
        )
        .into_anyhow()?;

    Ok(())
}

/// The reason a URL failed the string/literal SSRF policy, for logging.
enum UrlPolicyReject {
    /// The scheme is not `http`/`https`.
    Scheme,
    /// The URL has no host component.
    NoHost,
    /// The host is a known-internal name (`localhost`, `*.internal`, …).
    InternalHost,
    /// The host is a private/reserved IP literal.
    PrivateIpLiteral,
}

/// The string/literal half of the SSRF fence, on an already-parsed URL: scheme
/// must be `http`/`https`, the host must be present and not a known-internal
/// name, and any IP *literal* must not be private/reserved.
///
/// This is the check shared by the pre-send [`resolve_outbound_client`] and the
/// per-redirect-hop revalidation in [`revalidating_redirect_policy`]. It does
/// **not** resolve DNS — a hostname that resolves to a private address is caught
/// separately, at connect time, by [`ValidatingResolver`] (layer 2). Splitting
/// it out keeps the two layers on one policy instead of two copies.
fn check_url_policy(parsed: &Url) -> std::result::Result<(), UrlPolicyReject> {
    match parsed.scheme() {
        "http" | "https" => {}
        _ => return Err(UrlPolicyReject::Scheme),
    }

    // Classify through `Url::host()`, not by re-parsing `host_str()`. For an
    // IPv6 literal `host_str()` hands back the bracketed form (`[::1]`), which
    // never parses as an `IpAddr`, so every IPv6 literal used to fall through
    // the address check as though it were a hostname.
    let Some(host) = parsed.host() else {
        return Err(UrlPolicyReject::NoHost);
    };

    match host {
        url::Host::Domain(name) => {
            let host_lower = name.to_ascii_lowercase();
            if host_lower == "localhost"
                || host_lower.ends_with(".local")
                || host_lower.ends_with(".internal")
                || host_lower.ends_with(".localhost")
            {
                return Err(UrlPolicyReject::InternalHost);
            }
        }
        url::Host::Ipv4(v4) => {
            if is_disallowed_ip(IpAddr::V4(v4)) {
                return Err(UrlPolicyReject::PrivateIpLiteral);
            }
        }
        url::Host::Ipv6(v6) => {
            if is_disallowed_ip(IpAddr::V6(v6)) {
                return Err(UrlPolicyReject::PrivateIpLiteral);
            }
        }
    }

    Ok(())
}

/// Validate a URL for safe outbound use before sending (scheme + string/literal
/// SSRF protection), with no allowlist consulted: equivalent to
/// [`resolve_outbound_client`] with an empty [`PluginHttpAllowRuntime`].
///
/// Kept for the tests and call sites that only need "is this URL acceptable
/// at all", not the client it would use.
#[cfg(test)]
fn validate_url(raw_url: &str, plugin_name: &str) -> std::result::Result<(), i32> {
    resolve_outbound_client(
        raw_url,
        plugin_name,
        &reqwest::Client::new(),
        &PluginHttpAllowRuntime::empty(),
    )
    .map(|_| ())
}

/// The pre-send gate at both entry points (`request` and `open_stream`):
/// decide which client an outbound request uses, and refuse it outright when
/// neither the default fence nor the operator allowlist accepts it.
///
/// The allowlist is consulted first, before the default fence's verdict: an
/// entry in `allow` naming exactly this plugin, this port and this
/// destination hands back that entry's own client (relaxed only for its own
/// destination), regardless of whether the host happens to be a reserved
/// internal-looking name. A hostname entry's destination is not a reserved
/// suffix like `*.internal` — it is whatever hostname the operator wrote, so
/// checking the allowlist only after [`check_url_policy`] already rejected
/// the host would silently leave an entry for an ordinary-looking hostname
/// inert, since that check only rejects `*.internal`/`*.local`/`localhost`
/// names and private IP literals, never an arbitrary domain. No entry can
/// match a bad scheme or a missing host ([`plugin_http_allow::Entry::matches`]
/// requires both), so falling through to [`check_url_policy`] for anything
/// the allowlist did not grant still blocks exactly what it blocks today.
/// DNS-based rebinding and redirect-based bypasses are closed by the
/// client-level layers ([`ValidatingResolver`] and
/// [`revalidating_redirect_policy`]) every client, default or per-entry, is
/// built with.
fn resolve_outbound_client<'a>(
    raw_url: &str,
    plugin_name: &str,
    default: &'a reqwest::Client,
    allow: &'a PluginHttpAllowRuntime,
) -> std::result::Result<&'a reqwest::Client, i32> {
    let parsed = Url::parse(raw_url).map_err(|_| {
        warn!(plugin = %plugin_name, url = %raw_url, "malformed URL");
        host_errors::ERR_HTTP_INVALID_URL
    })?;

    if let Some(client) = allow.client_for(plugin_name, &parsed) {
        return Ok(client);
    }

    match check_url_policy(&parsed) {
        Ok(()) => Ok(default),
        Err(reason) => {
            let detail = match reason {
                UrlPolicyReject::Scheme => "blocked HTTP request with non-HTTP scheme",
                UrlPolicyReject::NoHost => "URL has no host",
                UrlPolicyReject::InternalHost => "blocked HTTP request to private hostname",
                UrlPolicyReject::PrivateIpLiteral => "blocked HTTP request to private/loopback IP",
            };
            warn!(
                plugin = %plugin_name,
                url = %raw_url,
                has_allowlist_entries = allow.has_entries_for(plugin_name),
                "{detail}"
            );
            Err(host_errors::ERR_HTTP_INVALID_URL)
        }
    }
}

// -----------------------------------------------------------------------------
// SSRF hardening: rebinding-safe resolver + per-hop redirect revalidation (p11i)
// -----------------------------------------------------------------------------

/// Marker error meaning "this outbound request was denied by the SSRF fence"
/// (a DNS-rebinding resolution or a redirect hop to a private/reserved target).
///
/// It is threaded through `reqwest` as the boxed source of the resolver/redirect
/// error and recovered by [`is_ssrf_block`] on the `send()` result, so both
/// bypasses surface as the existing `ERR_HTTP_INVALID_URL` denial rather than a
/// generic request failure — no new plugin-facing error code.
#[derive(Debug)]
struct SsrfBlocked;

impl std::fmt::Display for SsrfBlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "outbound request blocked by SSRF policy")
    }
}

impl std::error::Error for SsrfBlocked {}

/// Marker error for exceeding the redirect-hop cap, kept distinct from
/// [`SsrfBlocked`] so a capped chain maps to the generic request-failure code
/// (matching `reqwest`'s pre-existing default-policy behavior), not the SSRF
/// denial code.
#[derive(Debug)]
struct TooManyRedirects;

impl std::fmt::Display for TooManyRedirects {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "too many redirects")
    }
}

impl std::error::Error for TooManyRedirects {}

/// Walk an error's `source()` chain looking for an [`SsrfBlocked`] marker.
///
/// `reqwest` wraps a resolver or redirect-policy error and preserves the
/// original as a source; recovering the marker lets the two client-level SSRF
/// layers report the same denial code as the pre-send string check.
fn is_ssrf_block(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(err);
    while let Some(e) = current {
        if e.downcast_ref::<SsrfBlocked>().is_some() {
            return true;
        }
        current = e.source();
    }
    false
}

/// Resolve a hostname to candidate IP addresses.
///
/// The production backend ([`SystemResolver`]) uses the system resolver; tests
/// inject a fixed mapping to simulate a hostname that clears the string check but
/// resolves to a private address (DNS rebinding).
trait HostResolver: Send + Sync + std::fmt::Debug {
    /// Resolve `host` to its candidate IP addresses.
    fn lookup(
        &self,
        host: String,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>>;
}

/// Production [`HostResolver`]: the OS resolver via `tokio::net::lookup_host`.
#[derive(Debug)]
struct SystemResolver;

impl HostResolver for SystemResolver {
    fn lookup(
        &self,
        host: String,
    ) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>> {
        Box::pin(async move {
            // Port 0: reqwest applies the URL/scheme port to the returned addrs.
            let addrs = tokio::net::lookup_host((host.as_str(), 0u16)).await?;
            Ok(addrs.map(|sa| sa.ip()).collect())
        })
    }
}

/// A `reqwest` DNS resolver that closes the rebinding TOCTOU: it resolves the
/// hostname, denies the request if **any** resolved address is private/reserved
/// (same policy as the string check), and returns exactly those checked
/// addresses so the connection is pinned to what was validated. There is no
/// second, unvalidated resolution at connect time.
#[derive(Debug)]
struct ValidatingResolver {
    backend: Arc<dyn HostResolver>,
}

impl reqwest::dns::Resolve for ValidatingResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let backend = Arc::clone(&self.backend);
        Box::pin(async move {
            let host = name.as_str().to_string();
            let ips = backend
                .lookup(host)
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
            for ip in &ips {
                if is_disallowed_ip(*ip) {
                    // Rebinding: the name cleared the string check but resolves
                    // to a private address. Deny the whole request.
                    return Err(Box::new(SsrfBlocked) as Box<dyn std::error::Error + Send + Sync>);
                }
            }
            let addrs: reqwest::dns::Addrs =
                Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

/// A redirect policy that revalidates every hop: it re-runs the string/literal
/// [`check_url_policy`] on each redirect target (rejecting a hop to a private IP
/// literal, an internal hostname, or a non-HTTP scheme) and preserves the 10-hop
/// cap. A hop to a private *resolved* address is caught in addition by
/// [`ValidatingResolver`] when reqwest connects to it.
fn revalidating_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        // `previous()` includes the initial URL, so `> 10` matches reqwest's
        // default `Policy::limited(10)` (allow 10 redirects, deny the 11th).
        if attempt.previous().len() > 10 {
            return attempt.error(TooManyRedirects);
        }
        match check_url_policy(attempt.url()) {
            Ok(()) => attempt.follow(),
            Err(_) => attempt.error(SsrfBlocked),
        }
    })
}

/// Build the SSRF-hardened outbound [`reqwest::ClientBuilder`] with the given
/// resolver backend. Production callers use [`hardened_outbound_builder`]; the
/// backend seam exists so tests can inject a rebinding resolver.
fn builder_with_resolver(backend: Arc<dyn HostResolver>) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .dns_resolver(Arc::new(ValidatingResolver { backend }))
        .redirect(revalidating_redirect_policy())
}

/// The SSRF-hardened outbound client builder shared by every kernel path that
/// issues plugin-influenced HTTP. Installs the rebinding-safe DNS resolver
/// (layer 2) and per-hop redirect revalidation (layer 3). Callers add their own
/// headers (e.g. a User-Agent) and `.build()`.
pub(crate) fn hardened_outbound_builder() -> reqwest::ClientBuilder {
    builder_with_resolver(Arc::new(SystemResolver))
}

/// The default SSRF-hardened outbound client (no extra headers), for the tap
/// dispatch and background-install paths.
///
/// # Panics
///
/// Panics if the TLS backend fails to initialize. This is a fail-closed choice:
/// a build failure at startup must not silently fall back to an unhardened
/// client. In practice `build()` only fails on TLS backend init, which would
/// also fail a bare `reqwest::Client::new()`.
pub(crate) fn build_outbound_client() -> reqwest::Client {
    // Fail-closed: never silently fall back to an unhardened client. `build()`
    // only fails on TLS backend init, which would also fail `Client::new()`.
    #[allow(clippy::expect_used)]
    hardened_outbound_builder()
        .build()
        .expect("SSRF-hardened outbound HTTP client must build")
}

// -----------------------------------------------------------------------------
// The operator allowlist (`PLUGIN_HTTP_PRIVATE_ALLOW`, p312)
// -----------------------------------------------------------------------------

/// A `reqwest` DNS resolver for one hostname [`plugin_http_allow::Entry`]'s own
/// client: it resolves exactly as [`ValidatingResolver`] does for any name
/// other than the entry's own, but for the entry's own hostname it denies only
/// an address [`plugin_http_allow::is_never_allowable`] names (link-local,
/// including the cloud-metadata address, multicast, broadcast or
/// unspecified), and otherwise accepts it, private or loopback included. That
/// relaxation is scoped to this one name because this client is only ever
/// reached, via [`resolve_outbound_client`], by a request whose URL already
/// matched this entry (same plugin, port and hostname); a redirect to a
/// different hostname resolves under the same strict rule
/// [`ValidatingResolver`] applies everywhere else.
#[derive(Debug)]
struct EntryAllowResolver {
    backend: Arc<dyn HostResolver>,
    /// The entry's hostname, already lowercased by
    /// [`plugin_http_allow::Destination::Host`].
    allow_host: String,
}

impl reqwest::dns::Resolve for EntryAllowResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let backend = Arc::clone(&self.backend);
        let allow_host = self.allow_host.clone();
        Box::pin(async move {
            let host = name.as_str().to_string();
            let relaxed = host.eq_ignore_ascii_case(&allow_host);
            let ips = backend
                .lookup(host)
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
            for ip in &ips {
                let refuse = if relaxed {
                    plugin_http_allow::is_never_allowable(*ip)
                } else {
                    is_disallowed_ip(*ip)
                };
                if refuse {
                    return Err(Box::new(SsrfBlocked) as Box<dyn std::error::Error + Send + Sync>);
                }
            }
            let addrs: reqwest::dns::Addrs =
                Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

/// One configured entry plus the client built for it.
///
/// A literal or CIDR destination never triggers DNS resolution: `reqwest`
/// connects to an IP-literal host directly, so that entry's client only ever
/// differs from the default in which URL is let through layer 1; its resolver
/// and redirect policy are the unmodified strict ones. Only a hostname entry
/// needs [`EntryAllowResolver`]'s relaxation.
struct AllowedEntry {
    entry: plugin_http_allow::Entry,
    client: reqwest::Client,
}

impl std::fmt::Debug for AllowedEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AllowedEntry")
            .field("entry", &self.entry)
            .finish_non_exhaustive()
    }
}

/// The built form of `PLUGIN_HTTP_PRIVATE_ALLOW`: every entry, each with its
/// own client, built once at startup by [`build_plugin_http_allow_runtime`].
#[derive(Debug, Default)]
pub(crate) struct PluginHttpAllowRuntime {
    entries: Vec<AllowedEntry>,
}

impl PluginHttpAllowRuntime {
    /// The empty runtime: no plugin reaches a private or loopback address,
    /// today's behaviour. Used by every context, tests and a `Config` that
    /// never set `PLUGIN_HTTP_PRIVATE_ALLOW` alike, that attaches no allowlist.
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    /// The client a request from `plugin_name` to `url` should use, if any
    /// entry grants it. `None` means layer 1 found no bypassable refusal, or
    /// found one but no entry matched: the caller falls back to refusing.
    fn client_for(&self, plugin_name: &str, url: &Url) -> Option<&reqwest::Client> {
        self.entries
            .iter()
            .find(|allowed| allowed.entry.matches(plugin_name, url))
            .map(|allowed| &allowed.client)
    }

    /// Whether `plugin_name` has any configured entry at all, regardless of
    /// whether it matches this request, carried into the refusal log line so
    /// an operator can tell "no entry for this plugin" from "an entry exists
    /// but this destination/port isn't it".
    fn has_entries_for(&self, plugin_name: &str) -> bool {
        self.entries
            .iter()
            .any(|allowed| allowed.entry.plugin == plugin_name)
    }
}

/// Build the per-entry clients for `allow` and log each entry once, at info,
/// naming the plugin, destination and port it grants, not per request, since
/// the entries are static for the process's lifetime.
///
/// Called once at startup; the result is shared (behind an `Arc`) by every
/// dispatch path that issues plugin-influenced HTTP, the same way
/// [`build_outbound_client`]'s result is.
pub(crate) fn build_plugin_http_allow_runtime(allow: &PluginHttpAllow) -> PluginHttpAllowRuntime {
    let entries = allow
        .entries()
        .iter()
        .map(|entry| {
            tracing::info!(
                plugin = %entry.plugin,
                destination = %entry.destination,
                port = entry.port,
                "plugin HTTP private-address allowlist entry active"
            );
            AllowedEntry {
                entry: entry.clone(),
                client: build_entry_client(entry, Arc::new(SystemResolver)),
            }
        })
        .collect();
    PluginHttpAllowRuntime { entries }
}

/// Build one entry's client: [`EntryAllowResolver`] for a hostname
/// destination, the ordinary hardened resolver for a literal or CIDR one (see
/// [`AllowedEntry`]). `backend` is a seam for tests; production always passes
/// [`SystemResolver`].
fn build_entry_client(
    entry: &plugin_http_allow::Entry,
    backend: Arc<dyn HostResolver>,
) -> reqwest::Client {
    let builder = match &entry.destination {
        plugin_http_allow::Destination::Host(name) => reqwest::Client::builder()
            .dns_resolver(Arc::new(EntryAllowResolver {
                backend,
                allow_host: name.clone(),
            }))
            .redirect(revalidating_redirect_policy()),
        plugin_http_allow::Destination::Ip(_) | plugin_http_allow::Destination::Cidr(_) => {
            builder_with_resolver(backend)
        }
    };
    #[allow(clippy::expect_used)]
    builder
        .build()
        .expect("plugin-allowlist outbound HTTP client must build")
}

/// The single status/header representation shared by the one-shot `request` and
/// the streaming `http-open` metadata (parity fence, p11j / G-HTTP-META).
///
/// Both surfaces MUST describe a response the same way, so the vocabulary is
/// frozen once: the status is the raw `u16`, and headers are collected into a
/// `HashMap<String, String>` — reqwest yields lowercased header names, and the
/// `collect` collapses a repeated name to its last value. Keeping this in one
/// function guarantees `http-open` cannot drift from `request`.
fn extract_status_headers(
    response: &reqwest::Response,
) -> (u16, std::collections::HashMap<String, String>) {
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    (status, headers)
}

/// Build the `http-open` response-metadata JSON (`HttpOpenResponse`
/// `{handle, status, headers}`, p11j / G-HTTP-META), enforcing the oversize
/// policy.
///
/// Oversize policy (§9.5, the 64 KB tap I/O buffer): the metadata block must fit
/// the plugin's output buffer. If it does not — only possible with a
/// pathologically large header block — the open **errors** with
/// [`host_errors::ERR_HTTP_RESPONSE_TOO_LARGE`] (parity with one-shot `request`'s
/// oversize behavior) rather than silently truncating headers, because a dropped
/// `ETag`/`Content-Type` would be invisible and unrecoverable. Kept pure and
/// separate from the WASM memory write so the policy is unit-tested directly.
fn build_open_metadata(
    handle: u32,
    status: u16,
    headers: std::collections::HashMap<String, String>,
    out_max_len: i32,
) -> std::result::Result<String, i32> {
    let meta = trovato_sdk::types::HttpOpenResponse::new(handle, status, headers);
    let meta_json = serde_json::to_string(&meta).map_err(|_| host_errors::ERR_SERIALIZE_FAILED)?;
    if meta_json.len() > out_max_len.max(0) as usize {
        return Err(host_errors::ERR_HTTP_RESPONSE_TOO_LARGE);
    }
    Ok(meta_json)
}

/// Execute an HTTP request with timeout and size restrictions.
async fn execute_http_request(
    http: &reqwest::Client,
    request: &trovato_sdk::types::HttpRequest,
    plugin_name: &str,
) -> std::result::Result<trovato_sdk::types::HttpResponse, i32> {
    let method: reqwest::Method = request.method.parse().map_err(|_| {
        warn!(
            plugin = %plugin_name,
            method = %request.method,
            "invalid HTTP method"
        );
        host_errors::ERR_PARAM_DESERIALIZE
    })?;

    let timeout_ms = request
        .timeout_ms
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .min(MAX_TIMEOUT_MS);

    let mut req = http
        .request(method, &request.url)
        .timeout(Duration::from_millis(u64::from(timeout_ms)));

    // Add headers
    for (key, value) in &request.headers {
        req = req.header(key.as_str(), value.as_str());
    }

    // Add body if present
    if let Some(ref body) = request.body {
        req = req.body(body.clone());
    }

    let response = crate::telemetry::send(req).await.map_err(|e| {
        if is_ssrf_block(&e) {
            // A rebinding resolution or a redirect hop to a private target was
            // denied by the client-level SSRF layers; report it as the same
            // denial the pre-send string check uses.
            warn!(
                plugin = %plugin_name,
                url = %request.url,
                "blocked HTTP request (rebinding or redirect to private target)"
            );
            host_errors::ERR_HTTP_INVALID_URL
        } else if e.is_timeout() {
            warn!(
                plugin = %plugin_name,
                url = %request.url,
                "HTTP request timed out"
            );
            host_errors::ERR_HTTP_TIMEOUT
        } else {
            warn!(
                plugin = %plugin_name,
                url = %request.url,
                error = %e,
                "HTTP request failed"
            );
            host_errors::ERR_HTTP_REQUEST_FAILED
        }
    })?;

    // Pre-flight size check via Content-Length header (avoids buffering
    // an oversized body before rejecting it).
    if let Some(content_length) = response.content_length()
        && content_length > MAX_RESPONSE_BODY as u64
    {
        warn!(
            plugin = %plugin_name,
            url = %request.url,
            content_length = content_length,
            max = MAX_RESPONSE_BODY,
            "HTTP response Content-Length exceeds limit"
        );
        return Err(host_errors::ERR_HTTP_RESPONSE_TOO_LARGE);
    }

    let (status, headers) = extract_status_headers(&response);

    // Read body with size limit (still needed: Content-Length may be absent
    // or the server may lie about it).
    let body = response.bytes().await.map_err(|e| {
        warn!(
            plugin = %plugin_name,
            url = %request.url,
            error = %e,
            "failed to read HTTP response body"
        );
        host_errors::ERR_HTTP_REQUEST_FAILED
    })?;

    if body.len() > MAX_RESPONSE_BODY {
        warn!(
            plugin = %plugin_name,
            url = %request.url,
            body_len = body.len(),
            max = MAX_RESPONSE_BODY,
            "HTTP response body too large"
        );
        return Err(host_errors::ERR_HTTP_RESPONSE_TOO_LARGE);
    }

    let body_str = String::from_utf8_lossy(&body).into_owned();

    Ok(trovato_sdk::types::HttpResponse {
        status,
        headers,
        body: body_str,
    })
}

/// An open streaming HTTP fetch (P11e / D-49), stored in the calling tap's
/// [`PluginState`] so it is `Store`-scoped: the handle cannot leak across tap
/// invocations, and a fresh call starts with none.
///
/// The plugin drives it with `http-read` (each call yields ≤ [`MAX_READ_CHUNK`]
/// bytes; an empty result is EOF) and must `http-close` it. Bytes are pulled from
/// the network incrementally so a multi-MB body is never buffered whole in WASM
/// memory. The total wire transfer is bounded by [`Self::max_transfer`]; each
/// network read is bounded by [`Self::per_read_timeout`]; the whole fetch is
/// bounded by [`Self::deadline`].
#[derive(Debug)]
pub(crate) struct HttpStream {
    /// Response status captured at open, surfaced to the plugin in the
    /// `http-open` metadata (p11j / G-HTTP-META). Same representation as the
    /// one-shot `request` (see [`extract_status_headers`]).
    status: u16,
    /// Response headers captured at open, surfaced to the plugin in the
    /// `http-open` metadata (p11j / G-HTTP-META). Same representation as the
    /// one-shot `request`; enables conditional GET (read `ETag`/`Last-Modified`,
    /// distinguish `304` from an empty `200`) on the streaming path.
    headers: std::collections::HashMap<String, String>,
    /// Remaining response body source. `chunk()` pulls the next network piece.
    response: reqwest::Response,
    /// Bytes pulled from the network but not yet handed to the plugin (a single
    /// network chunk may exceed the 64 KB per-read cap), served front-to-back.
    leftover: Vec<u8>,
    /// Read cursor into [`Self::leftover`].
    leftover_pos: usize,
    /// Total bytes pulled from the network so far (the wire transfer bounded by
    /// [`Self::max_transfer`]).
    transferred: u64,
    /// Total-transfer ceiling in bytes (already kernel-clamped).
    max_transfer: u64,
    /// Per-read timeout: the time budget to receive the next network chunk.
    per_read_timeout: Duration,
    /// Wall-clock deadline for the whole fetch (the total-transfer budget).
    deadline: Instant,
    /// Set once the network body is exhausted; further reads return EOF.
    eof: bool,
}

impl HttpStream {
    /// Read up to `max_len` bytes (never more than [`MAX_READ_CHUNK`]) of the body.
    ///
    /// Returns the bytes read; an **empty** vector means EOF. Enforces the
    /// total-transfer ceiling (catching a server that under-reports
    /// `Content-Length`), the per-read timeout, and the wall-clock transfer
    /// budget, mapping each breach to its `ERR_HTTP_*` code.
    async fn read_chunk(&mut self, max_len: usize) -> std::result::Result<Vec<u8>, i32> {
        let cap = max_len.min(MAX_READ_CHUNK);
        if cap == 0 {
            return Ok(Vec::new());
        }

        // Serve buffered bytes from a prior over-sized network chunk first.
        if self.leftover_pos < self.leftover.len() {
            return Ok(self.take_from_leftover(cap));
        }
        if self.eof {
            return Ok(Vec::new());
        }

        // Pull the next network chunk under the per-read timeout, itself clamped
        // to the remaining wall-clock transfer budget.
        let now = Instant::now();
        if now >= self.deadline {
            return Err(host_errors::ERR_HTTP_TIMEOUT);
        }
        let read_budget = self.per_read_timeout.min(self.deadline - now);

        let chunk = match tokio::time::timeout(read_budget, self.response.chunk()).await {
            Err(_) => return Err(host_errors::ERR_HTTP_TIMEOUT), // per-read / budget expiry
            Ok(Err(e)) => {
                return Err(if e.is_timeout() {
                    host_errors::ERR_HTTP_TIMEOUT
                } else {
                    host_errors::ERR_HTTP_STREAM_READ_FAILED
                });
            }
            Ok(Ok(None)) => {
                self.eof = true;
                return Ok(Vec::new());
            }
            Ok(Ok(Some(c))) => c,
        };

        // Enforce the total-transfer ceiling as bytes accumulate. This is the
        // second half of the Content-Length guard: the preflight in `open_stream`
        // catches an honest large body; this catches a server that lies low.
        self.transferred += chunk.len() as u64;
        if self.transferred > self.max_transfer {
            return Err(host_errors::ERR_HTTP_TRANSFER_BUDGET);
        }

        self.leftover.clear();
        self.leftover.extend_from_slice(&chunk);
        self.leftover_pos = 0;
        Ok(self.take_from_leftover(cap))
    }

    /// Take up to `cap` bytes from the front of the leftover buffer, advancing the
    /// cursor. Caller guarantees `cap <= MAX_READ_CHUNK` and buffered bytes exist.
    fn take_from_leftover(&mut self, cap: usize) -> Vec<u8> {
        let end = (self.leftover_pos + cap).min(self.leftover.len());
        let out = self.leftover[self.leftover_pos..end].to_vec();
        self.leftover_pos = end;
        out
    }
}

/// Open a streaming HTTP fetch (P11e / D-49): apply the SSRF fence, send the
/// request, preflight `Content-Length` against `max_transfer`, and return a
/// [`HttpStream`] ready for chunked reads.
///
/// The SSRF fence and the allowlist client choice are
/// [`resolve_outbound_client`]: the SAME decision `request` applies (D-49
/// regression fence; p312 for the allowlist). `max_transfer` is the calling
/// plugin's kernel-clamped total-transfer ceiling (D-50). The per-read timeout
/// comes from `request.timeout_ms` (default 30 s, capped 60 s); the
/// whole-fetch wall-clock budget is [`TRANSFER_BUDGET`], also enforced
/// natively by reqwest's per-request timeout.
async fn open_stream(
    http: &reqwest::Client,
    http_allow: &PluginHttpAllowRuntime,
    request: &trovato_sdk::types::HttpRequest,
    plugin_name: &str,
    max_transfer: u64,
) -> std::result::Result<HttpStream, i32> {
    // Same decision as `request`: scheme, host, private/loopback blocks, and
    // the allowlist's client choice. This gate is the whole point of routing
    // streaming through the kernel; the network fetch below is only ever
    // reached for a URL that has cleared it.
    let client = resolve_outbound_client(&request.url, plugin_name, http, http_allow)?;
    fetch_stream(client, request, plugin_name, max_transfer).await
}

/// The post-SSRF network half of [`open_stream`]: send the request and build the
/// [`HttpStream`]. Separated so tests can exercise the streaming mechanics against
/// a loopback fixture server (which the SSRF fence would otherwise block) while
/// [`open_stream`]'s `validate_url` call is tested independently against blocked
/// hosts. Production code always reaches this through [`open_stream`].
async fn fetch_stream(
    http: &reqwest::Client,
    request: &trovato_sdk::types::HttpRequest,
    plugin_name: &str,
    max_transfer: u64,
) -> std::result::Result<HttpStream, i32> {
    let method: reqwest::Method = request.method.parse().map_err(|_| {
        warn!(
            plugin = %plugin_name,
            method = %request.method,
            "invalid HTTP method (http-open)"
        );
        host_errors::ERR_PARAM_DESERIALIZE
    })?;

    let per_read_timeout = Duration::from_millis(u64::from(
        request
            .timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .min(MAX_TIMEOUT_MS),
    ));

    // reqwest's per-request timeout runs from connect until the body is finished,
    // so it natively enforces the total-transfer wall-clock budget; per-read
    // bounds are applied around each `chunk()` in `HttpStream::read_chunk`.
    let mut req = http.request(method, &request.url).timeout(TRANSFER_BUDGET);
    for (key, value) in &request.headers {
        req = req.header(key.as_str(), value.as_str());
    }
    if let Some(ref body) = request.body {
        req = req.body(body.clone());
    }

    // Bound the initial-response (connect + headers) phase by the per-read timeout.
    let response = match tokio::time::timeout(per_read_timeout, crate::telemetry::send(req)).await {
        Err(_) => {
            warn!(plugin = %plugin_name, url = %request.url, "streaming HTTP open timed out");
            return Err(host_errors::ERR_HTTP_TIMEOUT);
        }
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            return Err(if is_ssrf_block(&e) {
                warn!(plugin = %plugin_name, url = %request.url, "blocked streaming HTTP open (rebinding or redirect to private target)");
                host_errors::ERR_HTTP_INVALID_URL
            } else if e.is_timeout() {
                warn!(plugin = %plugin_name, url = %request.url, "streaming HTTP open timed out");
                host_errors::ERR_HTTP_TIMEOUT
            } else {
                warn!(plugin = %plugin_name, url = %request.url, error = %e, "streaming HTTP open failed");
                host_errors::ERR_HTTP_REQUEST_FAILED
            });
        }
    };

    // Preflight the ceiling against Content-Length when the server is honest.
    if let Some(content_length) = response.content_length()
        && content_length > max_transfer
    {
        warn!(
            plugin = %plugin_name,
            url = %request.url,
            content_length = content_length,
            max = max_transfer,
            "streaming HTTP Content-Length exceeds total-transfer ceiling"
        );
        return Err(host_errors::ERR_HTTP_TRANSFER_BUDGET);
    }

    // Capture the response metadata before the body is consumed, in the SAME
    // representation as the one-shot `request` (parity fence, p11j).
    let (status, headers) = extract_status_headers(&response);

    Ok(HttpStream {
        status,
        headers,
        response,
        leftover: Vec::new(),
        leftover_pos: 0,
        transferred: 0,
        max_transfer,
        per_read_timeout,
        deadline: Instant::now() + TRANSFER_BUDGET,
        eof: false,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::plugin::PluginState;
    use crate::tap::RequestState;

    /// How the fixture server frames the body: an honest `Content-Length`, or none
    /// at all (HTTP/1.1 `Connection: close`, body-until-EOF) so the total-transfer
    /// ceiling can only be enforced mid-stream — the "Content-Length lie" surface.
    #[derive(Clone, Copy)]
    enum Framing {
        ContentLength,
        NoContentLength,
    }

    /// Bind a local multi-connection HTTP server that serves `body` to every
    /// request. Returns its `http://addr` base URL. Loopback-bound, so streaming
    /// mechanics are tested via [`fetch_stream`] (past the SSRF fence, which
    /// deliberately blocks 127.0.0.1); the fence itself is tested separately.
    async fn body_server(body: Vec<u8>, framing: Framing) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let header = match framing {
            Framing::ContentLength => format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            ),
            Framing::NoContentLength => {
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n"
                    .to_string()
            }
        };
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let header = header.clone();
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock.write_all(header.as_bytes()).await;
                    let _ = sock.write_all(&body).await;
                    let _ = sock.flush().await;
                });
            }
        });
        format!("http://{addr}")
    }

    /// Bind a loopback server that answers **every** request with a
    /// `302 Found` to `location`. Used to exercise the per-hop redirect
    /// revalidation without a public network.
    async fn redirect_server(location: String) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let resp = format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let resp = resp.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.flush().await;
                });
            }
        });
        format!("http://{addr}")
    }

    /// Parse a `http://host:port` base URL into its `SocketAddr` for use with
    /// reqwest's `.resolve()` override in redirect tests.
    fn addr_of(base: &str) -> SocketAddr {
        base.strip_prefix("http://")
            .expect("http base")
            .parse()
            .expect("socket addr")
    }

    /// A [`HostResolver`] that maps every hostname to a fixed set of IPs,
    /// simulating a DNS-rebinding record: a name that clears the string check
    /// but resolves to a private/reserved address.
    #[derive(Debug)]
    struct StubResolver {
        ips: Vec<IpAddr>,
    }

    impl HostResolver for StubResolver {
        fn lookup(
            &self,
            _host: String,
        ) -> Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>> {
            let ips = self.ips.clone();
            Box::pin(async move { Ok(ips) })
        }
    }

    fn get(url: &str) -> trovato_sdk::types::HttpRequest {
        trovato_sdk::types::HttpRequest {
            url: url.to_string(),
            method: "GET".to_string(),
            headers: std::collections::HashMap::new(),
            body: None,
            timeout_ms: None,
        }
    }

    /// Deterministic pseudo-random body of `n` bytes — large enough to span many
    /// 64 KB reads and non-uniform so a mis-assembled reorder would be detected.
    fn make_body(n: usize) -> Vec<u8> {
        (0..n)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect()
    }

    /// D-50 clamp: the manifest ceiling defaults to 1 MB, is capped at the 16 MB
    /// kernel maximum, and a zero declaration cannot disable the transfer entirely.
    #[test]
    fn transfer_ceiling_is_clamped_to_the_kernel_range() {
        assert_eq!(clamp_transfer_ceiling(None), DEFAULT_TRANSFER_CEILING);
        assert_eq!(
            clamp_transfer_ceiling(Some(4 * 1024 * 1024)),
            4 * 1024 * 1024
        );
        assert_eq!(
            clamp_transfer_ceiling(Some(64 * 1024 * 1024)),
            MAX_TRANSFER_CEILING,
            "a manifest cannot grant more than the kernel maximum"
        );
        assert_eq!(clamp_transfer_ceiling(Some(0)), 1, "zero clamps up to 1");
    }

    /// D-49 happy path: a 5 MB body streams successfully and reassembles exactly,
    /// and no single read exceeds the 64 KB tap buffer even when the caller offers
    /// a 1 MB buffer.
    #[tokio::test]
    async fn streams_and_reassembles_a_5mb_body_in_bounded_reads() {
        let body = make_body(5 * 1024 * 1024);
        let base = body_server(body.clone(), Framing::ContentLength).await;
        let client = reqwest::Client::new();
        // 8 MB ceiling comfortably clears the 5 MB body.
        let mut stream = fetch_stream(&client, &get(&base), "test", 8 * 1024 * 1024)
            .await
            .expect("open should succeed under the ceiling");

        let mut assembled = Vec::new();
        loop {
            // Offer a 1 MB buffer; the kernel must still cap each read at 64 KB.
            let chunk = stream.read_chunk(1024 * 1024).await.expect("read ok");
            if chunk.is_empty() {
                break;
            }
            assert!(
                chunk.len() <= MAX_READ_CHUNK,
                "a read returned {} bytes, over the 64 KB tap buffer",
                chunk.len()
            );
            assembled.extend_from_slice(&chunk);
        }
        assert_eq!(assembled.len(), body.len(), "reassembled length mismatch");
        assert_eq!(assembled, body, "reassembled body mismatch");
    }

    /// Back-compat fence: the unchanged one-shot `request` path still hard-fails
    /// the same 5 MB body with the same `ERR_HTTP_RESPONSE_TOO_LARGE`.
    #[tokio::test]
    async fn one_shot_request_still_rejects_the_5mb_body() {
        let body = make_body(5 * 1024 * 1024);
        let base = body_server(body, Framing::ContentLength).await;
        let client = reqwest::Client::new();
        let err = execute_http_request(&client, &get(&base), "test")
            .await
            .expect_err("one-shot request must reject a 5 MB body");
        assert_eq!(err, host_errors::ERR_HTTP_RESPONSE_TOO_LARGE);
    }

    /// D-50 preflight: an honest `Content-Length` over the ceiling is caught at
    /// open, before any body is read.
    #[tokio::test]
    async fn open_rejects_content_length_over_ceiling() {
        let body = make_body(2 * 1024 * 1024);
        let base = body_server(body, Framing::ContentLength).await;
        let client = reqwest::Client::new();
        // 1 MB ceiling, 2 MB declared body.
        let err = fetch_stream(&client, &get(&base), "test", 1024 * 1024)
            .await
            .expect_err("Content-Length over the ceiling must fail preflight");
        assert_eq!(err, host_errors::ERR_HTTP_TRANSFER_BUDGET);
    }

    /// D-50 mid-stream: a body with NO `Content-Length` (the preflight-small,
    /// body-large case) is caught as bytes accumulate past the ceiling. This is
    /// both the "transfer-budget expiry mid-stream" and "Content-Length lie" case.
    #[tokio::test]
    async fn read_rejects_transfer_over_ceiling_mid_stream() {
        let body = make_body(3 * 1024 * 1024);
        let base = body_server(body, Framing::NoContentLength).await;
        let client = reqwest::Client::new();
        // 1 MB ceiling with an undeclared 3 MB body: preflight can't see it.
        let mut stream = fetch_stream(&client, &get(&base), "test", 1024 * 1024)
            .await
            .expect("open succeeds — no Content-Length to preflight");

        let mut err = None;
        let mut total = 0usize;
        for _ in 0..1000 {
            match stream.read_chunk(MAX_READ_CHUNK).await {
                Ok(chunk) if chunk.is_empty() => break,
                Ok(chunk) => total += chunk.len(),
                Err(code) => {
                    err = Some(code);
                    break;
                }
            }
        }
        assert_eq!(
            err,
            Some(host_errors::ERR_HTTP_TRANSFER_BUDGET),
            "an over-ceiling undeclared body must fail mid-stream"
        );
        // The plugin never received materially more than the ceiling allowed
        // (bounded by one 64 KB over-read on the chunk that trips the guard).
        assert!(
            total as u64 <= 1024 * 1024 + MAX_READ_CHUNK as u64,
            "delivered {total} bytes, well over the 1 MB ceiling"
        );
    }

    /// D-49 SSRF fence: the SAME `validate_url` gate `request` uses is applied on
    /// the streaming entry point. `open_stream` (which calls it) rejects blocked
    /// hosts without ever touching the network — including the localhost-sidecar
    /// case the fence exists to block.
    #[tokio::test]
    async fn open_applies_the_ssrf_fence() {
        let client = reqwest::Client::new();
        for blocked in [
            "http://localhost:8080/admin",      // localhost sidecar
            "http://127.0.0.1:8080/",           // loopback IP literal
            "http://10.1.2.3/",                 // RFC 1918 private
            "http://192.168.0.1/",              // RFC 1918 private
            "http://169.254.169.254/latest",    // link-local / cloud metadata
            "http://service.internal/",         // internal suffix
            "ftp://example.com/",               // non-HTTP scheme
            "http://[::1]/",                    // IPv6 loopback literal
            "http://[::1]:8080/",               // IPv6 loopback literal with a port
            "http://[fd00::1]/",                // IPv6 unique-local
            "http://[fe80::1]/",                // IPv6 link-local
            "http://[::ffff:127.0.0.1]/",       // IPv4-mapped loopback
            "http://[::ffff:169.254.169.254]/", // IPv4-mapped cloud metadata
        ] {
            let err = open_stream(
                &client,
                &PluginHttpAllowRuntime::empty(),
                &get(blocked),
                "test",
                DEFAULT_TRANSFER_CEILING,
            )
            .await
            .expect_err("SSRF fence must block");
            assert_eq!(
                err,
                host_errors::ERR_HTTP_INVALID_URL,
                "expected block for {blocked}"
            );
        }
        // A public host clears the shared fence (validated without connecting),
        // and so does a public IPv6 literal and a public IPv4 one — the fix
        // must not turn "IPv6 literals are checked" into "IPv6 is refused".
        assert!(validate_url("https://confs.tech/api", "test").is_ok());
        assert!(validate_url("https://[2606:4700:4700::1111]/", "test").is_ok());
        assert!(validate_url("https://93.184.216.34/", "test").is_ok());
    }

    /// D-49 handle scoping: handles live in the per-call `PluginState`, so a fresh
    /// call cannot see another call's handle (cross-call reuse), and a closed
    /// handle is gone (read-after-close). Both surface as a missing map entry,
    /// which the host functions map to `ERR_HTTP_HANDLE_INVALID`.
    #[tokio::test]
    async fn handles_are_store_scoped_and_die_on_close() {
        let base = body_server(b"hello".to_vec(), Framing::ContentLength).await;
        let client = reqwest::Client::new();
        let stream = fetch_stream(&client, &get(&base), "test", DEFAULT_TRANSFER_CEILING)
            .await
            .expect("open");

        let mut call_a = PluginState::new(RequestState::default(), "p".to_string());
        let handle = call_a.http_stream_insert(stream).expect("first handle");
        assert!(
            call_a.http_stream_get(handle).is_some(),
            "handle live in its call"
        );

        // Cross-call reuse: a different call's state has no such handle.
        let mut call_b = PluginState::new(RequestState::default(), "p".to_string());
        assert!(
            call_b.http_stream_get(handle).is_none(),
            "a handle must not leak across tap invocations"
        );

        // Read-after-close: closing frees the slot; the id no longer resolves and
        // closing again reports it was already gone.
        assert!(
            call_a.http_stream_close(handle),
            "close reports it was open"
        );
        assert!(
            call_a.http_stream_get(handle).is_none(),
            "a closed handle must not resolve"
        );
        assert!(
            !call_a.http_stream_close(handle),
            "double close reports already-closed"
        );
    }

    /// D-49 handle cap: a single call cannot pin more than `MAX_OPEN_HTTP_STREAMS`
    /// live streams; the next open is refused (the host maps `None` to
    /// `ERR_HTTP_TOO_MANY_HANDLES`).
    #[tokio::test]
    async fn open_handles_are_capped_per_call() {
        let base = body_server(b"hi".to_vec(), Framing::ContentLength).await;
        let client = reqwest::Client::new();
        let mut state = PluginState::new(RequestState::default(), "p".to_string());
        for i in 0..MAX_OPEN_HTTP_STREAMS {
            let stream = fetch_stream(&client, &get(&base), "test", DEFAULT_TRANSFER_CEILING)
                .await
                .expect("open under cap");
            assert!(
                state.http_stream_insert(stream).is_some(),
                "insert {i} should be under the cap"
            );
        }
        let overflow = fetch_stream(&client, &get(&base), "test", DEFAULT_TRANSFER_CEILING)
            .await
            .expect("open");
        assert!(
            state.http_stream_insert(overflow).is_none(),
            "the {}th open must be refused",
            MAX_OPEN_HTTP_STREAMS + 1
        );
    }

    // -------------------------------------------------------------------------
    // SSRF hardening (p11i / G1): rebinding + redirect-hop revalidation.
    //
    // A live DNS-rebinding end-to-end is not reproducible in tests without
    // resolver injection; [`StubResolver`] is the accepted stand-in — it forces
    // the exact TOCTOU the fix closes (a name that clears the string check but
    // resolves to a private address), so the resolver-and-pin layer is exercised
    // directly. The redirect tests use a real loopback 302 server.
    // -------------------------------------------------------------------------

    /// Rebinding, one-shot `request` path: a hostname that clears the string
    /// check but resolves (via the validating resolver) to loopback is denied
    /// with the SSRF code, without ever connecting.
    #[tokio::test]
    async fn one_shot_denies_dns_rebinding() {
        let client = builder_with_resolver(Arc::new(StubResolver {
            ips: vec![IpAddr::from([127, 0, 0, 1])],
        }))
        .build()
        .expect("hardened client builds");
        let err = execute_http_request(&client, &get("http://rebind.invalid/"), "test")
            .await
            .expect_err("a name resolving to loopback must be denied");
        assert_eq!(
            err,
            host_errors::ERR_HTTP_INVALID_URL,
            "rebinding must surface as the SSRF denial, not a request failure"
        );
    }

    /// Rebinding, streaming `http-open` path: same TOCTOU, same denial. Proves
    /// the fence holds on both entry points, since they share the client.
    #[tokio::test]
    async fn streaming_denies_dns_rebinding() {
        let client = builder_with_resolver(Arc::new(StubResolver {
            ips: vec![IpAddr::from([169, 254, 169, 254])], // link-local / metadata
        }))
        .build()
        .expect("hardened client builds");
        let err = fetch_stream(
            &client,
            &get("http://metadata.invalid/"),
            "test",
            DEFAULT_TRANSFER_CEILING,
        )
        .await
        .expect_err("a name resolving to link-local must be denied");
        assert_eq!(err, host_errors::ERR_HTTP_INVALID_URL);
    }

    /// A rebinding record with a mix of public and private addresses is denied:
    /// the validating resolver rejects if **any** resolved address is private,
    /// so an attacker cannot smuggle a private target alongside a public one.
    #[tokio::test]
    async fn rebinding_mixed_addresses_are_denied() {
        let client = builder_with_resolver(Arc::new(StubResolver {
            ips: vec![
                IpAddr::from([93, 184, 216, 34]),
                IpAddr::from([10, 0, 0, 5]),
            ],
        }))
        .build()
        .expect("hardened client builds");
        let err = execute_http_request(&client, &get("http://mixed.invalid/"), "test")
            .await
            .expect_err("any private resolved address must deny the whole request");
        assert_eq!(err, host_errors::ERR_HTTP_INVALID_URL);
    }

    /// Redirect-to-private, one-shot: a public fetch that 302s to a metadata-style
    /// address is denied mid-chain with the SSRF code. The initial hop is a
    /// loopback fixture reached directly (as the streaming tests do), so the
    /// redirect policy — not the entry check — is what blocks the private target.
    #[tokio::test]
    async fn one_shot_denies_redirect_to_private_target() {
        let base = redirect_server("http://169.254.169.254/latest/meta-data/".to_string()).await;
        let client = build_outbound_client();
        let err = execute_http_request(&client, &get(&base), "test")
            .await
            .expect_err("a redirect to a private target must be denied");
        assert_eq!(err, host_errors::ERR_HTTP_INVALID_URL);
    }

    /// Redirect hop to a bracketed IPv6 loopback literal. The redirect policy
    /// runs the same `check_url_policy` as the entry gate, so before the fix
    /// this hop was followed: the bracketed form never parsed as an address, so
    /// the hop read as an ordinary hostname with no matching internal suffix.
    #[tokio::test]
    async fn one_shot_denies_redirect_to_ipv6_loopback_literal() {
        // Port 9 (discard) is closed on the loopback interface in practice, so
        // a regression shows up as the SSRF code being absent, never as a hang.
        let base = redirect_server("http://[::1]:9/".to_string()).await;
        let client = build_outbound_client();
        let err = execute_http_request(&client, &get(&base), "test")
            .await
            .expect_err("a redirect to an IPv6 loopback literal must be denied");
        assert_eq!(err, host_errors::ERR_HTTP_INVALID_URL);
    }

    /// Rebinding onto an IPv4-mapped IPv6 address: a hostname whose AAAA record
    /// answers `::ffff:127.0.0.1`. The resolver saw a v6 address and read its
    /// high bits as global unicast, so the embedded loopback went unnoticed.
    #[tokio::test]
    async fn one_shot_denies_rebinding_to_an_ipv4_mapped_address() {
        let client = builder_with_resolver(Arc::new(StubResolver {
            ips: vec!["::ffff:127.0.0.1".parse().expect("mapped address parses")],
        }))
        .build()
        .expect("hardened client builds");
        let err = execute_http_request(&client, &get("http://mapped.invalid/"), "test")
            .await
            .expect_err("an IPv4-mapped loopback answer must be denied");
        assert_eq!(err, host_errors::ERR_HTTP_INVALID_URL);
    }

    /// Redirect-to-private, streaming: the same denial on the `http-open` path.
    #[tokio::test]
    async fn streaming_denies_redirect_to_private_target() {
        let base = redirect_server("http://127.0.0.1:9/".to_string()).await;
        let client = build_outbound_client();
        let err = fetch_stream(&client, &get(&base), "test", DEFAULT_TRANSFER_CEILING)
            .await
            .expect_err("a redirect to loopback must be denied");
        assert_eq!(err, host_errors::ERR_HTTP_INVALID_URL);
    }

    /// Redirect-cap regression: an endless public redirect loop still stops at the
    /// 10-hop cap and surfaces as a request failure (NOT the SSRF code). The
    /// `.resolve()` override maps the public-looking name back to the loopback
    /// fixture so the chain clears the per-hop policy and only the cap ends it.
    #[tokio::test]
    async fn redirect_cap_is_preserved() {
        let base = redirect_server("http://loop.test/".to_string()).await;
        let client = hardened_outbound_builder()
            .resolve("loop.test", addr_of(&base))
            .build()
            .expect("hardened client builds");
        let err = execute_http_request(&client, &get("http://loop.test/"), "test")
            .await
            .expect_err("an endless redirect loop must stop at the cap");
        assert_eq!(
            err,
            host_errors::ERR_HTTP_REQUEST_FAILED,
            "the hop cap is a request failure, not an SSRF denial"
        );
    }

    /// Happy-path regression: a public fetch whose single redirect stays within
    /// policy still succeeds and returns the final body. Two loopback fixtures
    /// (a 302 then a 200) are mapped to public-looking names via `.resolve()` so
    /// the resolver and redirect layers are both traversed on a legitimate chain.
    #[tokio::test]
    async fn public_redirect_within_policy_succeeds() {
        let final_base = body_server(b"final-body".to_vec(), Framing::ContentLength).await;
        let redir_base = redirect_server("http://dest.test/".to_string()).await;
        let client = hardened_outbound_builder()
            .resolve("start.test", addr_of(&redir_base))
            .resolve("dest.test", addr_of(&final_base))
            .build()
            .expect("hardened client builds");
        let resp = execute_http_request(&client, &get("http://start.test/"), "test")
            .await
            .expect("a policy-clean redirect chain must succeed");
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "final-body");
    }

    /// The `is_ssrf_block` recovery helper matches an [`SsrfBlocked`] anywhere in
    /// the error `source()` chain but not an unrelated error — the discriminator
    /// that routes rebinds/blocked hops to the SSRF code and leaves timeouts and
    /// genuine failures on their own codes.
    #[test]
    fn is_ssrf_block_walks_the_source_chain() {
        #[derive(Debug)]
        struct Wrapper(Box<dyn std::error::Error + Send + Sync>);
        impl std::fmt::Display for Wrapper {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "wrapper")
            }
        }
        impl std::error::Error for Wrapper {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(self.0.as_ref())
            }
        }

        let nested = Wrapper(Box::new(SsrfBlocked));
        assert!(is_ssrf_block(&nested), "must find a nested SsrfBlocked");
        assert!(
            is_ssrf_block(&SsrfBlocked),
            "must find a direct SsrfBlocked"
        );

        let unrelated = Wrapper(Box::new(std::io::Error::other("boom")));
        assert!(
            !is_ssrf_block(&unrelated),
            "must not match unrelated errors"
        );
    }

    // -------------------------------------------------------------------------
    // http-open response metadata (p11j / G-HTTP-META)
    // -------------------------------------------------------------------------

    /// Bind a loopback server that replies to every request with an exact raw
    /// HTTP response (status line + headers + body framed by the caller). Lets a
    /// test assert status/header capture and 304 handling on the streaming path,
    /// past the SSRF fence (via [`fetch_stream`]) as the other streaming tests do.
    async fn raw_server(raw_response: Vec<u8>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let raw = raw_response.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock.write_all(&raw).await;
                    let _ = sock.flush().await;
                });
            }
        });
        format!("http://{addr}")
    }

    /// p11j happy path: `http-open` captures the response status and headers, and
    /// they are byte-identical to what the one-shot `request` path reports for the
    /// same response — the parity fence, so streaming conditional GET reads the
    /// same vocabulary. ETag/Last-Modified/Content-Type are all present.
    #[tokio::test]
    async fn streaming_open_captures_status_and_headers_matching_one_shot() {
        let body = b"<rss>feed</rss>".to_vec();
        let raw = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nETag: \"v1-abc\"\r\n\
             Last-Modified: Wed, 21 Oct 2026 07:28:00 GMT\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
            body.len()
        )
        .into_bytes()
        .into_iter()
        .chain(body.clone())
        .collect::<Vec<u8>>();
        let base = raw_server(raw).await;
        let client = reqwest::Client::new();

        let stream = fetch_stream(&client, &get(&base), "test", DEFAULT_TRANSFER_CEILING)
            .await
            .expect("streaming open should succeed");
        assert_eq!(stream.status, 200, "streaming open must capture the status");
        assert_eq!(
            stream.headers.get("etag").map(String::as_str),
            Some("\"v1-abc\""),
            "ETag must be readable from the streaming open metadata"
        );
        assert!(
            stream.headers.contains_key("last-modified"),
            "Last-Modified must be present"
        );
        assert_eq!(
            stream.headers.get("content-type").map(String::as_str),
            Some("application/rss+xml")
        );

        // Parity: the one-shot path reports the identical status and headers.
        let one_shot = execute_http_request(&client, &get(&base), "test")
            .await
            .expect("one-shot request should succeed");
        assert_eq!(
            one_shot.status, stream.status,
            "streaming and one-shot status must match"
        );
        assert_eq!(
            one_shot.headers, stream.headers,
            "streaming and one-shot header representation must match exactly"
        );
    }

    /// p11j 304 semantics: a conditional streaming GET answered `304 Not Modified`
    /// exposes the status in the open metadata, and the first `http-read` on its
    /// (body-less) handle returns a clean, immediate EOF — the streaming
    /// short-circuit for conditional GET.
    #[tokio::test]
    async fn streaming_conditional_get_304_is_immediate_eof() {
        let raw = "HTTP/1.1 304 Not Modified\r\nETag: \"v1-abc\"\r\nConnection: close\r\n\r\n"
            .as_bytes()
            .to_vec();
        let base = raw_server(raw).await;
        let client = reqwest::Client::new();

        let mut stream = fetch_stream(&client, &get(&base), "test", DEFAULT_TRANSFER_CEILING)
            .await
            .expect("streaming open should succeed even for 304");
        assert_eq!(
            stream.status, 304,
            "the 304 must be visible in the metadata"
        );
        let chunk = stream
            .read_chunk(MAX_READ_CHUNK)
            .await
            .expect("read on a 304 handle must not error");
        assert!(
            chunk.is_empty(),
            "a 304 body-less stream must read immediate EOF"
        );
    }

    /// p11j oversize policy: metadata that fits the buffer serializes to a
    /// round-trippable `HttpOpenResponse`; metadata whose header block overruns the
    /// buffer errors the open with `ERR_HTTP_RESPONSE_TOO_LARGE` (parity with the
    /// one-shot oversize behavior) rather than silently truncating headers.
    #[test]
    fn build_open_metadata_enforces_the_oversize_policy() {
        let mut headers = std::collections::HashMap::new();
        headers.insert("etag".to_string(), "\"v1\"".to_string());

        let json = build_open_metadata(7, 200, headers.clone(), 64 * 1024)
            .expect("small metadata must fit a generous buffer");
        let parsed: trovato_sdk::types::HttpOpenResponse =
            serde_json::from_str(&json).expect("metadata round-trips");
        assert_eq!(parsed.handle, 7);
        assert_eq!(parsed.status, 200);
        assert_eq!(
            parsed.headers.get("etag").map(String::as_str),
            Some("\"v1\"")
        );

        // A pathologically large header block over a tiny buffer errors the open.
        let mut big = std::collections::HashMap::new();
        big.insert("x-huge".to_string(), "z".repeat(4096));
        let err = build_open_metadata(1, 200, big, 256)
            .expect_err("oversized metadata must error, not truncate");
        assert_eq!(err, host_errors::ERR_HTTP_RESPONSE_TOO_LARGE);
    }

    // -------------------------------------------------------------------------
    // The operator allowlist (`PLUGIN_HTTP_PRIVATE_ALLOW`, p312): a plugin
    // named in an entry reaches exactly the destination and port it names,
    // every other plugin/port/destination meets the same refusal as before,
    // and a redirect off the granted destination is refused like any other.
    // -------------------------------------------------------------------------

    /// One literal-destination entry's runtime, built the production way
    /// (`SystemResolver`, fine here since no DNS resolution happens for an
    /// IP-literal host).
    fn allow_for_literal(plugin: &str, ip_port: &str) -> PluginHttpAllowRuntime {
        build_plugin_http_allow_runtime(
            &PluginHttpAllow::parse(&format!("{plugin}@{ip_port}")).expect("entry parses"),
        )
    }

    /// (a) a request from the named plugin to the named destination and port
    /// reaches the mock server.
    #[tokio::test]
    async fn allowlisted_plugin_reaches_its_named_destination() {
        let base = body_server(b"purged".to_vec(), Framing::ContentLength).await;
        let port = addr_of(&base).port();
        let allow = allow_for_literal("p1", &format!("127.0.0.1:{port}"));
        let default = reqwest::Client::new();

        let client = resolve_outbound_client(&base, "p1", &default, &allow)
            .expect("the named plugin must be granted its named destination");
        let resp = execute_http_request(client, &get(&base), "p1")
            .await
            .expect("the request must reach the mock");
        assert_eq!(resp.body, "purged");
    }

    /// (b) the identical request from a different plugin is refused: an entry
    /// grants one plugin, not every plugin.
    #[tokio::test]
    async fn allowlist_entry_does_not_grant_a_different_plugin() {
        let base = body_server(b"purged".to_vec(), Framing::ContentLength).await;
        let port = addr_of(&base).port();
        let allow = allow_for_literal("p1", &format!("127.0.0.1:{port}"));
        let default = reqwest::Client::new();

        let err = resolve_outbound_client(&base, "p2", &default, &allow)
            .expect_err("a different plugin must not be granted another plugin's entry");
        assert_eq!(err, host_errors::ERR_HTTP_INVALID_URL);
    }

    /// (c) the named plugin, to the same address but a different port, is
    /// refused: the port is part of the match, not just the address.
    #[tokio::test]
    async fn allowlist_entry_does_not_grant_a_different_port() {
        let allow = allow_for_literal("p1", "127.0.0.1:6081");
        let default = reqwest::Client::new();

        let err = resolve_outbound_client("http://127.0.0.1:6082/", "p1", &default, &allow)
            .expect_err("a different port on the same address must be refused");
        assert_eq!(err, host_errors::ERR_HTTP_INVALID_URL);
    }

    /// (d) the named plugin, to a private address no entry names, is refused
    /// at validation: an entry grants one destination, not every private one.
    /// The probe keeps the entry's own port, so this isolates the destination
    /// check from the port check (c) isolates separately.
    #[tokio::test]
    async fn allowlist_entry_does_not_grant_an_unlisted_destination() {
        let allow = allow_for_literal("p1", "127.0.0.1:6081");
        let default = reqwest::Client::new();

        let err = resolve_outbound_client("http://10.0.0.9:6081/", "p1", &default, &allow)
            .expect_err("an unlisted private destination must be refused");
        assert_eq!(err, host_errors::ERR_HTTP_INVALID_URL);
    }

    /// (e) a hostname entry grants the plugin named, only through the
    /// hostname it names: a different plugin is refused for the identical
    /// hostname, and the literal address the hostname resolves to does not
    /// itself match the entry.
    #[tokio::test]
    async fn hostname_entry_grants_only_its_plugin_and_only_through_its_name() {
        let base = body_server(b"varnish-ok".to_vec(), Framing::ContentLength).await;
        let port = addr_of(&base).port();
        let entry = PluginHttpAllow::parse(&format!("p1@varnish.internal:{port}"))
            .expect("entry parses")
            .entries()[0]
            .clone();
        let client = build_entry_client(
            &entry,
            Arc::new(StubResolver {
                ips: vec![IpAddr::from([127, 0, 0, 1])],
            }),
        );
        let allow = PluginHttpAllowRuntime {
            entries: vec![AllowedEntry { entry, client }],
        };
        let default = reqwest::Client::new();
        let url = format!("http://varnish.internal:{port}/");

        // The named plugin reaches it, resolved through the stubbed private
        // address: "its resolved address is accepted only for that request".
        let client = resolve_outbound_client(&url, "p1", &default, &allow)
            .expect("p1 must be granted through the hostname");
        let resp = execute_http_request(client, &get(&url), "p1")
            .await
            .expect("the request must reach the mock");
        assert_eq!(resp.body, "varnish-ok");

        // A different plugin is refused for the identical hostname.
        let err = resolve_outbound_client(&url, "p2", &default, &allow)
            .expect_err("a different plugin must not be granted this hostname entry");
        assert_eq!(err, host_errors::ERR_HTTP_INVALID_URL);

        // The literal address the hostname resolves to does not itself match
        // a hostname entry: the match is on the name, not on whatever it
        // happens to resolve to.
        let literal_url = format!("http://127.0.0.1:{port}/");
        let err = resolve_outbound_client(&literal_url, "p1", &default, &allow)
            .expect_err("the literal address must not match a hostname entry");
        assert_eq!(err, host_errors::ERR_HTTP_INVALID_URL);
    }

    /// A hostname entry's relaxation never reaches link-local, multicast,
    /// broadcast or unspecified space, even for its own matched request: "the
    /// address the name resolved to, still never link local".
    #[tokio::test]
    async fn hostname_entry_still_refuses_a_never_allowable_resolved_address() {
        let entry = PluginHttpAllow::parse("p1@varnish.internal:80")
            .expect("entry parses")
            .entries()[0]
            .clone();
        let client = build_entry_client(
            &entry,
            Arc::new(StubResolver {
                ips: vec![IpAddr::from([169, 254, 169, 254])],
            }),
        );
        let allow = PluginHttpAllowRuntime {
            entries: vec![AllowedEntry { entry, client }],
        };
        let default = reqwest::Client::new();
        let url = "http://varnish.internal:80/";

        // Layer 1 grants it (the name and port match), but the resolver still
        // refuses the link-local address it resolved to.
        let client = resolve_outbound_client(url, "p1", &default, &allow)
            .expect("layer 1 grants the matched hostname and port");
        let err = execute_http_request(client, &get(url), "p1")
            .await
            .expect_err("a link-local resolved address must still be refused");
        assert_eq!(err, host_errors::ERR_HTTP_INVALID_URL);
    }

    /// A hostname entry's destination need not be a reserved internal-looking
    /// name: `check_url_policy` only rejects `*.internal`/`*.local`/
    /// `localhost` names and private IP literals outright, so an ordinary
    /// domain the operator still wants to grant (e.g. a real hostname that
    /// simply happens to resolve to a private address) would never reach
    /// `allow.client_for` if the allowlist were only consulted after that
    /// check rejected the URL. The allowlist must be asked first, for every
    /// hostname, not only ones `check_url_policy` would have refused anyway.
    #[tokio::test]
    async fn hostname_entry_grants_an_ordinary_hostname_not_just_a_reserved_suffix() {
        let base = body_server(b"cache-ok".to_vec(), Framing::ContentLength).await;
        let port = addr_of(&base).port();
        let entry = PluginHttpAllow::parse(&format!("p1@cache.example.com:{port}"))
            .expect("entry parses")
            .entries()[0]
            .clone();
        let client = build_entry_client(
            &entry,
            Arc::new(StubResolver {
                ips: vec![IpAddr::from([127, 0, 0, 1])],
            }),
        );
        let allow = PluginHttpAllowRuntime {
            entries: vec![AllowedEntry { entry, client }],
        };
        let default = reqwest::Client::new();
        let url = format!("http://cache.example.com:{port}/");

        let client = resolve_outbound_client(&url, "p1", &default, &allow)
            .expect("an ordinary hostname entry must still be granted");
        let resp = execute_http_request(client, &get(&url), "p1")
            .await
            .expect("the entry's relaxed resolver must accept the loopback address it resolved to");
        assert_eq!(resp.body, "cache-ok");
    }

    /// (f) a redirect from the granted destination to a different private
    /// address (including the same address on a different port) is
    /// refused, exactly as a redirect to any private target always is.
    #[tokio::test]
    async fn redirect_off_the_granted_destination_is_refused() {
        let other = body_server(b"nope".to_vec(), Framing::ContentLength).await;
        let other_port = addr_of(&other).port();
        let base = redirect_server(format!("http://127.0.0.1:{other_port}/")).await;
        let granted_port = addr_of(&base).port();
        assert_ne!(
            granted_port, other_port,
            "the fixture ports must differ for this to test anything"
        );

        let allow = allow_for_literal("p1", &format!("127.0.0.1:{granted_port}"));
        let default = reqwest::Client::new();

        let client = resolve_outbound_client(&base, "p1", &default, &allow)
            .expect("p1 is granted the first hop");
        let err = execute_http_request(client, &get(&base), "p1")
            .await
            .expect_err("a redirect off the granted destination must be refused");
        assert_eq!(err, host_errors::ERR_HTTP_INVALID_URL);
    }

    /// (h) with no entries configured, every outcome is exactly what
    /// `resolve_outbound_client` with no allowlist gives today: the existing
    /// SSRF tests above (run unmodified, with an empty runtime threaded
    /// through) are the fuller version of this; this test names the point
    /// directly.
    #[tokio::test]
    async fn empty_allowlist_keeps_default_behaviour_unchanged() {
        let default = reqwest::Client::new();
        let allow = PluginHttpAllowRuntime::empty();
        for blocked in [
            "http://127.0.0.1:9/",
            "http://10.0.0.5/",
            "http://service.internal/",
        ] {
            let err = resolve_outbound_client(blocked, "any_plugin", &default, &allow)
                .expect_err("with no allowlist, every private destination is refused");
            assert_eq!(err, host_errors::ERR_HTTP_INVALID_URL);
        }
        assert!(
            resolve_outbound_client("https://93.184.216.34/", "any_plugin", &default, &allow)
                .is_ok()
        );
    }

    /// The refusal log line's `has_allowlist_entries` field: a plugin with no
    /// configured entry at all is distinguishable from one whose entry simply
    /// did not match this request.
    #[test]
    fn has_entries_for_reports_whether_the_plugin_has_any_entry_at_all() {
        let allow = allow_for_literal("p1", "127.0.0.1:80");
        assert!(allow.has_entries_for("p1"));
        assert!(!allow.has_entries_for("p2"));
    }
}
