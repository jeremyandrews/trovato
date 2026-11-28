//! AWS Signature Version 4 request signing, enough to talk to an S3-compatible
//! store (AWS, MinIO, Ceph RGW, DigitalOcean Spaces, Backblaze B2's S3
//! gateway) over the kernel's own `reqwest` client.
//!
//! This is deliberately not a client SDK: it signs a request (header auth, for
//! PUT/GET/DELETE/HEAD) or a URL (query auth, for a presigned GET), using only
//! `hmac`, `sha2`, `hex` and `urlencoding` — all workspace dependencies already
//! used elsewhere in the kernel. See `crates/kernel/src/file/storage.rs` for
//! why this was chosen over `object_store` or the AWS SDK.
//!
//! Reference: <https://docs.aws.amazon.com/general/latest/gr/signature-version-4.html>

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// SHA-256 of zero bytes, used as the payload hash of every request this
/// module signs with an empty body (GET, HEAD, DELETE).
pub const EMPTY_PAYLOAD_HASH: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// The payload hash a presigned URL uses: the signature covers no body, since
/// the request that redeems it is never signed by this process.
pub const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

/// Credentials used to sign a request. Never logged, never stored in the
/// database: read from the environment only (see `Config::from_env`).
#[derive(Clone)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

/// A request to sign: method, host, path (already the exact bytes that will
/// go on the wire, e.g. `/bucket/key` or `/key` depending on path style),
/// query pairs and the SHA-256 hex of the body (or [`UNSIGNED_PAYLOAD`]).
pub struct SigningRequest<'a> {
    pub method: &'a str,
    pub host: &'a str,
    pub path: &'a str,
    pub query: &'a [(&'a str, &'a str)],
    pub payload_hash: &'a str,
}

/// `sha256(data)` as lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    // The key is never empty-length-invalid for HMAC-SHA256 (any length is
    // accepted), so this cannot fail.
    #[allow(clippy::unwrap_used)]
    let mut mac = HmacSha256::new_from_slice(key).unwrap();
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// The four-round signing key derivation (SigV4 Task 3).
fn signing_key(secret: &str, date_stamp: &str, region: &str, service: &str) -> Vec<u8> {
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date_stamp.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, service.as_bytes());
    hmac(&k_service, b"aws4_request")
}

/// Percent-encode `s` the way SigV4 requires: unreserved characters
/// (`A-Za-z0-9-_.~`) pass through verbatim, everything else is `%XX`
/// uppercase hex. `encode_slash` controls whether `/` is encoded — SigV4
/// encodes it in a query string value but not in a canonical URI path (S3's
/// documented exception: path segments are not double-encoded).
fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b'/' if !encode_slash => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The canonical URI: each `/`-separated segment percent-encoded, slashes
/// kept as separators.
fn canonical_uri(path: &str) -> String {
    if path.is_empty() {
        return "/".to_string();
    }
    uri_encode(path, false)
}

/// The canonical query string: every pair percent-encoded (slashes included),
/// sorted by key then value, joined with `&`. Empty when `query` is empty.
fn canonical_query_string(query: &[(&str, &str)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (uri_encode(k, true), uri_encode(v, true)))
        .collect();
    pairs.sort();
    pairs
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// The headers this module signs, always exactly `host` plus whatever the
/// caller adds (`x-amz-date`, `x-amz-content-sha256` for header auth; just
/// `host` for a presigned URL, handled by the caller omitting them).
fn canonical_headers_and_signed(headers: &[(&str, &str)]) -> (String, String) {
    let mut pairs: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    pairs.sort();
    let canonical = pairs
        .iter()
        .map(|(k, v)| format!("{k}:{v}\n"))
        .collect::<String>();
    let signed = pairs
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join(";");
    (canonical, signed)
}

/// `date_stamp` (`YYYYMMDD`) and `amz_date` (`YYYYMMDDTHHMMSSZ`) for `when`.
pub fn timestamps(when: DateTime<Utc>) -> (String, String) {
    (
        when.format("%Y%m%d").to_string(),
        when.format("%Y%m%dT%H%M%SZ").to_string(),
    )
}

/// The `Authorization` header value for `req`, signed with `creds` for
/// `region`/`service` at `when`. `extra_headers` are additional headers
/// (besides `host`) the caller will also send and wants covered by the
/// signature — each must be lowercase-safe-to-trim, as sent on the wire.
#[allow(clippy::too_many_arguments)]
pub fn sign_request(
    req: &SigningRequest<'_>,
    creds: &Credentials,
    region: &str,
    service: &str,
    when: DateTime<Utc>,
    extra_headers: &[(&str, &str)],
) -> String {
    let (date_stamp, amz_date) = timestamps(when);
    let mut headers: Vec<(&str, &str)> = vec![("host", req.host)];
    headers.extend_from_slice(extra_headers);
    let (canonical_headers, signed_headers) = canonical_headers_and_signed(&headers);

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        req.method,
        canonical_uri(req.path),
        canonical_query_string(req.query),
        canonical_headers,
        signed_headers,
        req.payload_hash,
    );

    let credential_scope = format!("{date_stamp}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let key = signing_key(&creds.secret_access_key, &date_stamp, region, service);
    let signature = hex::encode(hmac(&key, string_to_sign.as_bytes()));

    format!(
        "AWS4-HMAC-SHA256 Credential={}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}",
        creds.access_key_id,
    )
}

/// The query string (already `key=value&...`, ready to append to the URL) for
/// a presigned GET of `path` on `host`, valid for `expires_secs` seconds from
/// `when`. Only `host` is a signed header: a presigned URL is redeemed by
/// whatever fetches it (a browser, `curl`), which sends no custom headers.
pub fn presign_query(
    host: &str,
    path: &str,
    creds: &Credentials,
    region: &str,
    service: &str,
    when: DateTime<Utc>,
    expires_secs: u32,
) -> String {
    let (date_stamp, amz_date) = timestamps(when);
    let credential_scope = format!("{date_stamp}/{region}/{service}/aws4_request");
    let credential = format!("{}/{credential_scope}", creds.access_key_id);

    let mut query: Vec<(String, String)> = vec![
        ("X-Amz-Algorithm".into(), "AWS4-HMAC-SHA256".into()),
        ("X-Amz-Credential".into(), credential),
        ("X-Amz-Date".into(), amz_date.clone()),
        ("X-Amz-Expires".into(), expires_secs.to_string()),
        ("X-Amz-SignedHeaders".into(), "host".into()),
    ];
    query.sort();
    let borrowed: Vec<(&str, &str)> = query
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let (canonical_headers, signed_headers) = canonical_headers_and_signed(&[("host", host)]);
    let canonical_request = format!(
        "GET\n{}\n{}\n{}\n{}\n{}",
        canonical_uri(path),
        canonical_query_string(&borrowed),
        canonical_headers,
        signed_headers,
        UNSIGNED_PAYLOAD,
    );

    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );
    let key = signing_key(&creds.secret_access_key, &date_stamp, region, service);
    let signature = hex::encode(hmac(&key, string_to_sign.as_bytes()));

    let mut final_query = canonical_query_string(&borrowed);
    final_query.push_str("&X-Amz-Signature=");
    final_query.push_str(&signature);
    final_query
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_time() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 24, 0, 0, 0).unwrap()
    }

    fn test_creds() -> Credentials {
        Credentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
        }
    }

    #[test]
    fn sha256_of_empty_matches_the_well_known_constant() {
        // This exact value is ubiquitous (every SigV4 and TLS reference cites
        // it); a mismatch means the hex/digest wiring itself is broken, not
        // that the algorithm is subtly wrong.
        assert_eq!(sha256_hex(b""), EMPTY_PAYLOAD_HASH);
    }

    #[test]
    fn uri_encode_keeps_unreserved_and_escapes_the_rest() {
        assert_eq!(uri_encode("abcXYZ019-_.~", true), "abcXYZ019-_.~");
        assert_eq!(uri_encode("a b", true), "a%20b");
        assert_eq!(uri_encode("a/b", true), "a%2Fb");
        assert_eq!(uri_encode("a/b", false), "a/b");
        assert_eq!(uri_encode("héllo", true), "h%C3%A9llo");
    }

    #[test]
    fn canonical_query_string_sorts_and_encodes() {
        let q = [("b", "2"), ("a", "1"), ("c", "x y")];
        assert_eq!(canonical_query_string(&q), "a=1&b=2&c=x%20y");
        assert_eq!(canonical_query_string(&[]), "");
    }

    #[test]
    fn canonical_headers_lowercases_sorts_and_trims() {
        let headers = [
            ("X-Amz-Date", " 20260524T000000Z "),
            ("Host", "example.com"),
        ];
        let (canonical, signed) = canonical_headers_and_signed(&headers);
        assert_eq!(canonical, "host:example.com\nx-amz-date:20260524T000000Z\n");
        assert_eq!(signed, "host;x-amz-date");
    }

    #[test]
    fn signing_is_deterministic_and_keyed_by_every_input() {
        let req = SigningRequest {
            method: "GET",
            host: "examplebucket.s3.amazonaws.com",
            path: "/test.txt",
            query: &[],
            payload_hash: EMPTY_PAYLOAD_HASH,
        };
        let creds = test_creds();
        let when = fixed_time();
        let sig_a = sign_request(
            &req,
            &creds,
            "us-east-1",
            "s3",
            when,
            &[
                ("x-amz-date", &timestamps(when).1),
                ("x-amz-content-sha256", EMPTY_PAYLOAD_HASH),
            ],
        );
        let sig_b = sign_request(
            &req,
            &creds,
            "us-east-1",
            "s3",
            when,
            &[
                ("x-amz-date", &timestamps(when).1),
                ("x-amz-content-sha256", EMPTY_PAYLOAD_HASH),
            ],
        );
        assert_eq!(sig_a, sig_b, "same inputs must sign identically");

        // Cross-checked against an independent, from-scratch SigV4
        // implementation in Node's `crypto` module (same fixed inputs: see
        // the derivation script kept alongside this PR's description) —
        // not a value transcribed from memory of an external example.
        assert_eq!(
            sig_a,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;x-amz-content-sha256;x-amz-date, \
             Signature=ddc9a6a4c703a52852086a7bc783b2917aeda9a96b9b81f0ff16f5e8903aeac1"
        );

        // A different path must change the signature (it is not ignoring the
        // request it was asked to sign).
        let other = SigningRequest {
            path: "/other.txt",
            ..req
        };
        let sig_c = sign_request(
            &other,
            &creds,
            "us-east-1",
            "s3",
            when,
            &[
                ("x-amz-date", &timestamps(when).1),
                ("x-amz-content-sha256", EMPTY_PAYLOAD_HASH),
            ],
        );
        assert_ne!(sig_a, sig_c);
        assert_eq!(
            sig_c,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;x-amz-content-sha256;x-amz-date, \
             Signature=1f24a563b5c9fe237ae4eac5fe38a7610e949be4c4e7c2133cf8579914be7f9d"
        );
    }

    #[test]
    fn presign_query_matches_an_independent_implementation() {
        let creds = test_creds();
        let when = fixed_time();
        let qs = presign_query(
            "my-bucket.s3.amazonaws.com",
            "/2026/01/abc_photo.jpg",
            &creds,
            "us-east-1",
            "s3",
            when,
            900,
        );
        // Cross-checked the same way as the header-auth signature above.
        assert_eq!(
            qs,
            "X-Amz-Algorithm=AWS4-HMAC-SHA256\
             &X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20260524%2Fus-east-1%2Fs3%2Faws4_request\
             &X-Amz-Date=20260524T000000Z\
             &X-Amz-Expires=900\
             &X-Amz-SignedHeaders=host\
             &X-Amz-Signature=62796a360d49cd6a376ec024aa2ac2d046b526db0107d3b9ec2fd35290a83b85"
        );
    }
}
