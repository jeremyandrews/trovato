//! S3-compatible object storage: AWS, MinIO, Ceph RGW, DigitalOcean Spaces,
//! Backblaze B2's S3 gateway — anything that speaks the S3 REST API and
//! SigV4 request signing (`crate::file::sigv4`).
//!
//! No AWS SDK and no native TLS: requests go through the kernel's own
//! `reqwest` client (rustls, the same TLS stack every other outbound request
//! in this kernel uses), signed by hand. See `crate::file::storage` for why.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use bytes::Bytes;
use futures_core::Stream;
use tracing::debug;

use super::sigv4::{self, Credentials, SigningRequest};
use super::storage::FileStorage;

/// Settings for an [`S3Storage`] backend. Credentials come from the process
/// environment only (`Config::from_env`): never the database, never a config
/// set, never logged.
#[derive(Debug, Clone)]
pub struct S3Settings {
    /// `https://s3.amazonaws.com`, `http://minio.example.svc:9000`, etc. — no
    /// trailing slash.
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    /// Prepended to every key, so several sites (or schemes) can share one
    /// bucket without colliding. May be empty.
    pub key_prefix: String,
    /// `http://endpoint/bucket/key` instead of `http://bucket.endpoint/key`.
    /// MinIO and most self-hosted stores need this; AWS S3 does not.
    pub path_style: bool,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Served straight from a CDN or the bucket's own public endpoint instead
    /// of proxied through this kernel, for a `public` S3 backend. `None`
    /// proxies every read through the kernel's own `/files/{path}` route.
    pub public_base_url: Option<String>,
    /// How long a presigned GET URL is valid.
    pub presign_ttl: Duration,
}

/// An S3-compatible backend.
pub struct S3Storage {
    http: reqwest::Client,
    settings: S3Settings,
    /// The authority (`host[:port]`) requests are sent to, and the `Host`
    /// header SigV4 signs — computed once, since `path_style` and `endpoint`
    /// never change after construction.
    host: String,
    /// `http` or `https`, from `endpoint`.
    url_scheme: String,
}

impl S3Storage {
    pub fn new(settings: S3Settings) -> Result<Self> {
        let parsed = url::Url::parse(&settings.endpoint)
            .with_context(|| format!("invalid S3 endpoint {:?}", settings.endpoint))?;
        let endpoint_host = parsed
            .host_str()
            .with_context(|| format!("S3 endpoint {:?} has no host", settings.endpoint))?;
        let host = match parsed.port() {
            Some(port) if settings.path_style => format!("{endpoint_host}:{port}"),
            Some(port) => format!("{}.{endpoint_host}:{port}", settings.bucket),
            None if settings.path_style => endpoint_host.to_string(),
            None => format!("{}.{endpoint_host}", settings.bucket),
        };
        Ok(Self {
            http: reqwest::Client::new(),
            url_scheme: parsed.scheme().to_string(),
            host,
            settings,
        })
    }

    fn credentials(&self) -> Credentials {
        Credentials {
            access_key_id: self.settings.access_key_id.clone(),
            secret_access_key: self.settings.secret_access_key.clone(),
        }
    }

    /// The object key for `uri` (`s3://{key}`), with the configured prefix
    /// applied. Rejects `..` components the same way `LocalFileStorage` does.
    fn key_for(&self, uri: &str) -> Result<String> {
        let path = uri
            .strip_prefix("s3://")
            .context("invalid s3 URI, must start with s3://")?;
        for component in std::path::Path::new(path).components() {
            if matches!(component, std::path::Component::ParentDir) {
                bail!("directory traversal not allowed in storage URI");
            }
        }
        Ok(format!("{}{path}", self.settings.key_prefix))
    }

    /// The request path: `/{bucket}/{key}` path-style, `/{key}` virtual-hosted
    /// (the bucket is already in the host).
    fn request_path(&self, key: &str) -> String {
        if self.settings.path_style {
            format!("/{}/{key}", self.settings.bucket)
        } else {
            format!("/{key}")
        }
    }

    fn object_url(&self, key: &str) -> String {
        format!("{}://{}{}", self.url_scheme, self.host, self.request_path(key))
    }

    /// Sign and send one request with `method` against `key`, with `body`
    /// (empty for GET/HEAD/DELETE) whose SHA-256 is signed.
    async fn send(
        &self,
        method: reqwest::Method,
        key: &str,
        body: Vec<u8>,
    ) -> Result<reqwest::Response> {
        let path = self.request_path(key);
        let payload_hash = sigv4::sha256_hex(&body);
        let now = chrono::Utc::now();
        let (_, amz_date) = sigv4::timestamps(now);

        let req = SigningRequest {
            method: method.as_str(),
            host: &self.host,
            path: &path,
            query: &[],
            payload_hash: &payload_hash,
        };
        let authorization = sigv4::sign_request(
            &req,
            &self.credentials(),
            &self.settings.region,
            "s3",
            now,
            &[
                ("x-amz-date", &amz_date),
                ("x-amz-content-sha256", &payload_hash),
            ],
        );

        let url = self.object_url(key);
        let mut builder = self
            .http
            .request(method, &url)
            .header("host", &self.host)
            .header("x-amz-date", amz_date)
            .header("x-amz-content-sha256", payload_hash)
            .header("authorization", authorization);
        if !body.is_empty() {
            builder = builder.body(body);
        }
        builder
            .send()
            .await
            .with_context(|| format!("S3 request to {url} failed"))
    }

    fn not_found(status: reqwest::StatusCode) -> bool {
        status == reqwest::StatusCode::NOT_FOUND
    }
}

#[async_trait]
impl FileStorage for S3Storage {
    async fn write(&self, uri: &str, data: &[u8]) -> Result<()> {
        let key = self.key_for(uri)?;
        let resp = self
            .send(reqwest::Method::PUT, &key, data.to_vec())
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("S3 PUT {key} failed: {status} {body}");
        }
        debug!(uri = %uri, size = data.len(), "file written to s3");
        Ok(())
    }

    async fn read(&self, uri: &str) -> Result<Vec<u8>> {
        let key = self.key_for(uri)?;
        let resp = self.send(reqwest::Method::GET, &key, Vec::new()).await?;
        if Self::not_found(resp.status()) {
            bail!("S3 object not found: {key}");
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("S3 GET {key} failed: {status} {body}");
        }
        let bytes = resp
            .bytes()
            .await
            .with_context(|| format!("failed to read S3 response body for {key}"))?;
        debug!(uri = %uri, size = bytes.len(), "file read from s3");
        Ok(bytes.to_vec())
    }

    async fn read_stream(
        &self,
        uri: &str,
    ) -> Result<std::pin::Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>> {
        let key = self.key_for(uri)?;
        let resp = self.send(reqwest::Method::GET, &key, Vec::new()).await?;
        if Self::not_found(resp.status()) {
            bail!("S3 object not found: {key}");
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("S3 GET {key} failed: {status} {body}");
        }
        let stream = async_stream::stream! {
            let mut resp = resp;
            loop {
                match resp.chunk().await {
                    Ok(Some(chunk)) => yield Ok(chunk),
                    Ok(None) => break,
                    Err(e) => {
                        yield Err(anyhow::anyhow!("S3 stream error: {e}"));
                        break;
                    }
                }
            }
        };
        Ok(Box::pin(stream))
    }

    async fn delete(&self, uri: &str) -> Result<()> {
        let key = self.key_for(uri)?;
        let resp = self.send(reqwest::Method::DELETE, &key, Vec::new()).await?;
        if !resp.status().is_success() && !Self::not_found(resp.status()) {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("S3 DELETE {key} failed: {status} {body}");
        }
        debug!(uri = %uri, "file deleted from s3");
        Ok(())
    }

    async fn exists(&self, uri: &str) -> Result<bool> {
        let key = self.key_for(uri)?;
        let resp = self.send(reqwest::Method::HEAD, &key, Vec::new()).await?;
        if Self::not_found(resp.status()) {
            return Ok(false);
        }
        if !resp.status().is_success() {
            bail!("S3 HEAD {key} failed: {}", resp.status());
        }
        Ok(true)
    }

    async fn size(&self, uri: &str) -> Result<u64> {
        let key = self.key_for(uri)?;
        let resp = self.send(reqwest::Method::HEAD, &key, Vec::new()).await?;
        if !resp.status().is_success() {
            bail!("S3 HEAD {key} failed: {}", resp.status());
        }
        resp.headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .with_context(|| format!("S3 HEAD {key} had no Content-Length"))
    }

    fn public_url(&self, uri: &str) -> String {
        let key = self.key_for(uri).unwrap_or_default();
        match &self.settings.public_base_url {
            Some(base) => format!("{}/{key}", base.trim_end_matches('/')),
            None => self.object_url(&key),
        }
    }

    async fn presigned_get_url(&self, uri: &str) -> Result<Option<String>> {
        let key = self.key_for(uri)?;
        let path = self.request_path(&key);
        let query = sigv4::presign_query(
            &self.host,
            &path,
            &self.credentials(),
            &self.settings.region,
            "s3",
            chrono::Utc::now(),
            self.settings.presign_ttl.as_secs().min(u32::MAX as u64) as u32,
        );
        Ok(Some(format!(
            "{}://{}{path}?{query}",
            self.url_scheme, self.host
        )))
    }

    fn scheme(&self) -> &'static str {
        "s3"
    }

    fn describe(&self) -> String {
        format!(
            "s3 bucket={} endpoint={} path_style={}",
            self.settings.bucket, self.settings.endpoint, self.settings.path_style
        )
    }
}

impl std::fmt::Debug for S3Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Storage")
            .field("bucket", &self.settings.bucket)
            .field("endpoint", &self.settings.endpoint)
            .field("path_style", &self.settings.path_style)
            .finish()
    }
}
