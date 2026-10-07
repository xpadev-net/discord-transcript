//! Minimal S3-compatible object storage client for recording artifacts.
//!
//! The client speaks AWS SigV4 directly (no SDK dependency) so it works
//! against AWS S3 and against S3-compatible services (MinIO, Cloudflare R2,
//! Garage, ...) when `endpoint` is overridden. Only the operations the
//! recorder needs are implemented: PUT, HEAD, ListObjectsV2, batched
//! DeleteObjects, and presigned GET URLs for browser playback.

use std::fmt::{Display, Formatter};
use std::future::Future;
use std::time::Duration;

use base64::Engine;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

const S3_SERVICE: &str = "s3";
pub const DEFAULT_REGION: &str = "us-east-1";
/// Default lifetime of presigned playback URLs handed to the frontend.
pub const DEFAULT_PRESIGN_TTL_SECONDS: u64 = 900;
/// SigV4 presigned URLs may live at most 7 days.
pub const MAX_PRESIGN_TTL_SECONDS: u64 = 604_800;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// DeleteObjects accepts at most 1000 keys per request.
const MAX_DELETE_KEYS_PER_REQUEST: usize = 1000;
/// How much of an error response body to surface in diagnostics.
const MAX_ERROR_BODY_CHARS: usize = 300;

/// Resolved S3 settings (post env parsing). `secret_access_key` is kept out of
/// logs and `PartialEq` is only derived for tests.
#[derive(Clone, PartialEq, Eq)]
pub struct S3Settings {
    pub bucket: String,
    /// Normalized endpoint root `scheme://host[:port][/path]`; `None` selects
    /// the default AWS endpoint for `region`.
    pub endpoint: Option<String>,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Key prefix applied to every stored object; empty or ends with '/'.
    pub key_prefix: String,
    /// Path-style addressing (`endpoint/bucket/key`) vs virtual-hosted
    /// (`bucket.host/key`). Defaults to path-style when a custom endpoint is
    /// set (what most S3-compatible services expect).
    pub path_style: bool,
    pub presign_ttl_seconds: u64,
}

impl std::fmt::Debug for S3Settings {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Settings")
            .field("bucket", &self.bucket)
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"[redacted]")
            .field("key_prefix", &self.key_prefix)
            .field("path_style", &self.path_style)
            .field("presign_ttl_seconds", &self.presign_ttl_seconds)
            .finish()
    }
}

/// Validates an `CHUNK_STORAGE_S3_ENDPOINT` value (scheme + host, optional
/// path prefix, no query/fragment). Called by config parsing.
pub(crate) fn validate_endpoint(endpoint: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(endpoint).map_err(|err| format!("invalid endpoint: {err}"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("endpoint must use http or https".to_owned());
    }
    if url.host_str().is_none() {
        return Err("endpoint has no host".to_owned());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("endpoint must not carry a query or fragment".to_owned());
    }
    Ok(())
}

/// Synchronous object-store surface used by the recording pipeline. `S3Client`
/// implements it against real HTTP; tests substitute an in-memory fake.
pub trait ObjectStore: Send + Sync + std::fmt::Debug {
    /// Stores `body` under `key`, overwriting any existing object.
    fn put_object(&self, key: &str, body: &[u8], content_type: &str) -> Result<(), S3Error>;

    /// Returns whether `key` exists.
    fn head_object(&self, key: &str) -> Result<bool, S3Error>;

    /// Lists all keys under `prefix` (paginates as needed).
    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, S3Error>;

    /// Deletes `keys` in batches. Unknown keys are ignored by the service.
    fn delete_keys(&self, keys: &[String]) -> Result<(), S3Error>;

    /// Presigned GET URL valid for `ttl`. Pure signing; performs no request.
    fn presigned_get_url(&self, key: &str, ttl: Duration) -> String;

    /// Short label identifying the backing service for logs.
    fn endpoint_label(&self) -> String;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S3Error {
    /// Client-side configuration or request build failure.
    Config(String),
    /// Transport-level failure (DNS, TLS, timeout, ...).
    Http(String),
    /// The service answered with a non-success status.
    Status { status: u16, detail: String },
}

impl Display for S3Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(err) => write!(f, "s3 config error: {err}"),
            Self::Http(err) => write!(f, "s3 request error: {err}"),
            Self::Status { status, detail } => {
                write!(f, "s3 request failed with status {status}: {detail}")
            }
        }
    }
}

impl std::error::Error for S3Error {}

/// Decodes `%XX` escapes in a URL path segment. `+` is left alone (it is a
/// literal in paths, not a form-encoded space).
fn percent_decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug)]
struct EndpointParts {
    scheme: String,
    /// host[:port]
    host: String,
    /// URI-encoded path prefix without leading '/', e.g. `s3` for
    /// `https://host/s3`. Empty for normal endpoints.
    path_prefix: String,
}

impl EndpointParts {
    fn resolve(settings: &S3Settings) -> Result<Self, S3Error> {
        let Some(endpoint) = &settings.endpoint else {
            return Ok(Self {
                scheme: "https".to_owned(),
                host: format!("s3.{}.amazonaws.com", settings.region),
                path_prefix: String::new(),
            });
        };
        let url = reqwest::Url::parse(endpoint)
            .map_err(|err| S3Error::Config(format!("invalid endpoint: {err}")))?;
        let host = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_owned(),
        };
        let path_prefix = url
            .path()
            .trim_matches('/')
            .split('/')
            .filter(|segment| !segment.is_empty())
            // `url.path()` is already percent-encoded; decode first so
            // uri_encode_segment produces the canonical form instead of
            // double-encoding `%` (e.g. `%20` -> `%2520`).
            .map(|segment| uri_encode_segment(&percent_decode(segment)))
            .collect::<Vec<_>>()
            .join("/");
        Ok(Self {
            scheme: url.scheme().to_owned(),
            host,
            path_prefix,
        })
    }

    /// Authority used in the URL and the signed `host` header.
    fn authority(&self, settings: &S3Settings) -> String {
        if settings.path_style {
            self.host.clone()
        } else {
            format!("{}.{}", settings.bucket, self.host)
        }
    }

    /// Canonical (URI-encoded) request path for `key`.
    fn canonical_path(&self, settings: &S3Settings, key: &str) -> String {
        let mut path = String::from("/");
        if !self.path_prefix.is_empty() {
            path.push_str(&self.path_prefix);
            path.push('/');
        }
        if settings.path_style {
            path.push_str(&uri_encode_segment(&settings.bucket));
            path.push('/');
        }
        path.push_str(&uri_encode_key(key));
        path
    }
}

/// SigV4-signed S3 client over `reqwest`. `ObjectStore` methods are
/// synchronous because `ChunkStorage` is a sync trait: each call bridges onto
/// a private single-thread runtime (a scoped OS thread when invoked from
/// inside another Tokio runtime, so it is safe from async handlers and plain
/// threads alike).
#[derive(Debug)]
pub struct S3Client {
    http: reqwest::Client,
    settings: S3Settings,
    endpoint: EndpointParts,
    /// Lazily built because `S3Client` is constructed inside async contexts
    /// where an eager runtime's blocking drop would panic (see `Drop`).
    fallback_rt: std::sync::OnceLock<tokio::runtime::Runtime>,
}

impl S3Client {
    pub fn new(settings: S3Settings) -> Result<Self, S3Error> {
        let endpoint = EndpointParts::resolve(&settings)?;
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|err| S3Error::Http(err.to_string()))?;
        Ok(Self {
            http,
            settings,
            endpoint,
            fallback_rt: std::sync::OnceLock::new(),
        })
    }

    /// Runtime for non-Tokio callers; created on first use so dropping an
    /// unused client never tears down a runtime at all.
    fn fallback_runtime(&self) -> &tokio::runtime::Runtime {
        self.fallback_rt.get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("s3 fallback runtime must build")
        })
    }

    pub fn settings(&self) -> &S3Settings {
        &self.settings
    }

    /// Runs `fut` to completion regardless of the caller's context: from a
    /// Tokio runtime thread the future is driven on a private runtime inside a
    /// scoped OS thread (this avoids `block_in_place` restrictions on
    /// current-thread runtimes and blocking pools); from a plain thread it
    /// runs in place on the lazily built fallback runtime.
    fn block_on<F>(&self, fut: F) -> F::Output
    where
        F: Future + Send,
        F::Output: Send,
    {
        if tokio::runtime::Handle::try_current().is_ok() {
            std::thread::scope(|scope| {
                match scope
                    .spawn(|| {
                        // A fresh runtime per call on the scoped thread: it is
                        // created and dropped on a plain OS thread, so it is
                        // safe from the async-context drop restriction.
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("scoped s3 runtime must build");
                        rt.block_on(fut)
                    })
                    .join()
                {
                    Ok(output) => output,
                    Err(payload) => std::panic::resume_unwind(payload),
                }
            })
        } else {
            self.fallback_runtime().block_on(fut)
        }
    }

    /// Signs and sends a request, returning the raw response for callers that
    /// handle non-2xx statuses themselves (e.g. HEAD → 404).
    async fn send_signed(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(String, String)],
        body: Vec<u8>,
        content_type: Option<&str>,
        extra_headers: &[(&str, String)],
    ) -> Result<reqwest::Response, S3Error> {
        let now = Utc::now();
        let payload_hash = sha256_hex(&body);
        let (amz_date, date_stamp) = amz_dates(now);
        let authority = self.endpoint.authority(&self.settings);
        let mut headers = vec![
            ("host".to_owned(), authority.clone()),
            ("x-amz-content-sha256".to_owned(), payload_hash.clone()),
            ("x-amz-date".to_owned(), amz_date.clone()),
        ];
        if let Some(content_type) = content_type {
            headers.push(("content-type".to_owned(), content_type.to_owned()));
        }
        for (name, value) in extra_headers {
            headers.push((name.to_ascii_lowercase(), value.clone()));
        }
        let (signed_headers, canonical_headers) = canonicalize_headers(&headers);
        let canonical_query = canonicalize_query(query);
        let canonical_request = format!(
            "{method}\n{path}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
        );
        let signature = self.sign_canonical_request(&canonical_request, &amz_date, &date_stamp);
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            self.settings.access_key_id,
            credential_scope(&date_stamp, &self.settings.region),
            signed_headers,
            signature
        );

        let url = format!(
            "{}://{}{}{}",
            self.endpoint.scheme,
            authority,
            path,
            if canonical_query.is_empty() {
                String::new()
            } else {
                format!("?{canonical_query}")
            }
        );
        let mut request = self
            .http
            .request(method, &url)
            .header("x-amz-date", amz_date)
            .header("x-amz-content-sha256", payload_hash)
            .header("authorization", authorization);
        if let Some(content_type) = content_type {
            request = request.header("content-type", content_type);
        }
        for (name, value) in extra_headers {
            request = request.header(*name, value);
        }
        request
            .body(body)
            .send()
            .await
            .map_err(|err| S3Error::Http(err.to_string()))
    }

    /// Signs and sends a request with `body`, expecting a 2xx status.
    async fn signed_request(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(String, String)],
        body: Vec<u8>,
        content_type: Option<&str>,
        extra_headers: &[(&str, String)],
    ) -> Result<reqwest::Response, S3Error> {
        let response = self
            .send_signed(method, path, query, body, content_type, extra_headers)
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable body>".to_owned());
            return Err(S3Error::Status {
                status: status.as_u16(),
                detail: truncate_body(&body),
            });
        }
        Ok(response)
    }

    fn sign_canonical_request(
        &self,
        canonical_request: &str,
        amz_date: &str,
        date_stamp: &str,
    ) -> String {
        let scope = credential_scope(date_stamp, &self.settings.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            sha256_hex(canonical_request.as_bytes())
        );
        let key = signing_key(
            &self.settings.secret_access_key,
            date_stamp,
            &self.settings.region,
        );
        hex_encode(&hmac(&key, string_to_sign.as_bytes()))
    }

    async fn put_object_async(
        &self,
        key: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<(), S3Error> {
        let path = self.endpoint.canonical_path(&self.settings, key);
        self.signed_request(
            reqwest::Method::PUT,
            &path,
            &[],
            body,
            Some(content_type),
            &[],
        )
        .await?;
        Ok(())
    }

    async fn head_object_async(&self, key: &str) -> Result<bool, S3Error> {
        let path = self.endpoint.canonical_path(&self.settings, key);
        let response = self
            .send_signed(reqwest::Method::HEAD, &path, &[], Vec::new(), None, &[])
            .await?;
        match response.status().as_u16() {
            404 => Ok(false),
            status if (200..300).contains(&status) => Ok(true),
            status => Err(S3Error::Status {
                status,
                detail: "unexpected HEAD status".to_owned(),
            }),
        }
    }

    async fn list_keys_async(&self, prefix: &str) -> Result<Vec<String>, S3Error> {
        let mut keys = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            // ListObjectsV2 lives on the bucket, not on an object key: for
            // path-style endpoints the path is `/bucket`, for virtual-hosted
            // the bucket is part of the authority and the path is `/`.
            let mut path = String::from("/");
            if !self.endpoint.path_prefix.is_empty() {
                path.push_str(&self.endpoint.path_prefix);
                path.push('/');
            }
            if self.settings.path_style {
                path.push_str(&uri_encode_segment(&self.settings.bucket));
            }
            let mut query = vec![
                ("list-type".to_owned(), "2".to_owned()),
                ("prefix".to_owned(), prefix.to_owned()),
            ];
            if let Some(token) = &continuation {
                query.push(("continuation-token".to_owned(), token.clone()));
            }
            let response = self
                .signed_request(reqwest::Method::GET, &path, &query, Vec::new(), None, &[])
                .await?;
            let body = response
                .text()
                .await
                .map_err(|err| S3Error::Http(err.to_string()))?;
            let page = parse_list_objects_v2(&body);
            keys.extend(page.keys);
            match (page.is_truncated, page.next_continuation) {
                (true, Some(token)) => continuation = Some(token),
                // Some S3-compatible services omit NextContinuationToken but
                // still flag truncation; bail out loudly instead of looping.
                (true, None) => {
                    return Err(S3Error::Status {
                        status: 200,
                        detail: "ListObjectsV2 truncated without a continuation token".to_owned(),
                    });
                }
                (false, _) => return Ok(keys),
            }
        }
    }

    fn presigned_get_url_at(&self, key: &str, ttl: Duration, now: DateTime<Utc>) -> String {
        let (amz_date, date_stamp) = amz_dates(now);
        let path = self.endpoint.canonical_path(&self.settings, key);
        let authority = self.endpoint.authority(&self.settings);
        let scope = credential_scope(&date_stamp, &self.settings.region);
        let credential = format!("{}/{}", self.settings.access_key_id, scope);
        let mut query = vec![
            ("X-Amz-Algorithm".to_owned(), "AWS4-HMAC-SHA256".to_owned()),
            ("X-Amz-Credential".to_owned(), credential),
            ("X-Amz-Date".to_owned(), amz_date.clone()),
            (
                "X-Amz-Expires".to_owned(),
                ttl.as_secs().clamp(1, MAX_PRESIGN_TTL_SECONDS).to_string(),
            ),
            ("X-Amz-SignedHeaders".to_owned(), "host".to_owned()),
        ];
        query.sort();
        let canonical_query = canonicalize_query(&query);
        let canonical_request =
            format!("GET\n{path}\n{canonical_query}\nhost:{authority}\n\nhost\nUNSIGNED-PAYLOAD");
        let signature = self.sign_canonical_request(&canonical_request, &amz_date, &date_stamp);
        format!(
            "{}://{}{}?{}&X-Amz-Signature={}",
            self.endpoint.scheme, authority, path, canonical_query, signature
        )
    }

    async fn delete_keys_async(&self, keys: &[String]) -> Result<(), S3Error> {
        if keys.is_empty() {
            return Ok(());
        }
        let mut path = String::from("/");
        if !self.endpoint.path_prefix.is_empty() {
            path.push_str(&self.endpoint.path_prefix);
            path.push('/');
        }
        if self.settings.path_style {
            path.push_str(&uri_encode_segment(&self.settings.bucket));
        }
        let query = vec![("delete".to_owned(), String::new())];
        for chunk in keys.chunks(MAX_DELETE_KEYS_PER_REQUEST) {
            let mut body = String::from("<Delete><Quiet>true</Quiet>");
            for key in chunk {
                body.push_str("<Object><Key>");
                body.push_str(&xml_escape(key));
                body.push_str("</Key></Object>");
            }
            body.push_str("</Delete>");
            let body = body.into_bytes();
            // AWS S3 requires Content-MD5 on DeleteObjects (x-amz-content-
            // sha256 does not substitute for it); sign the header too.
            let content_md5 =
                base64::engine::general_purpose::STANDARD.encode(md5::Md5::digest(&body));
            let response = self
                .signed_request(
                    reqwest::Method::POST,
                    &path,
                    &query,
                    body,
                    Some("application/xml"),
                    &[("content-md5", content_md5)],
                )
                .await?;
            let body = response
                .text()
                .await
                .map_err(|err| S3Error::Http(err.to_string()))?;
            let errors = parse_delete_errors(&body);
            if !errors.is_empty() {
                return Err(S3Error::Status {
                    status: 200,
                    detail: format!(
                        "DeleteObjects reported {} error(s): {}",
                        errors.len(),
                        errors.join("; ")
                    ),
                });
            }
        }
        Ok(())
    }
}

impl Drop for S3Client {
    fn drop(&mut self) {
        // Dropping a Tokio Runtime blocks and panics when the client is
        // dropped inside an async context (e.g. a task holding it exits).
        // The nonblocking shutdown leaves orphaned background tasks to the
        // process teardown instead of panicking.
        if let Some(rt) = self.fallback_rt.take() {
            rt.shutdown_background();
        }
    }
}

impl ObjectStore for S3Client {
    fn put_object(&self, key: &str, body: &[u8], content_type: &str) -> Result<(), S3Error> {
        let body = body.to_vec();
        let content_type = content_type.to_owned();
        self.block_on(async move { self.put_object_async(key, body, &content_type).await })
    }

    fn head_object(&self, key: &str) -> Result<bool, S3Error> {
        self.block_on(async move { self.head_object_async(key).await })
    }

    fn list_keys(&self, prefix: &str) -> Result<Vec<String>, S3Error> {
        self.block_on(async move { self.list_keys_async(prefix).await })
    }

    fn delete_keys(&self, keys: &[String]) -> Result<(), S3Error> {
        let keys = keys.to_vec();
        self.block_on(async move { self.delete_keys_async(&keys).await })
    }

    fn presigned_get_url(&self, key: &str, ttl: Duration) -> String {
        self.presigned_get_url_at(key, ttl, Utc::now())
    }

    fn endpoint_label(&self) -> String {
        self.settings
            .endpoint
            .clone()
            .unwrap_or_else(|| format!("s3.{}.amazonaws.com", self.settings.region))
    }
}

// ---- SigV4 helpers ---------------------------------------------------------

fn amz_dates(now: DateTime<Utc>) -> (String, String) {
    (
        now.format("%Y%m%dT%H%M%SZ").to_string(),
        now.format("%Y%m%d").to_string(),
    )
}

fn credential_scope(date_stamp: &str, region: &str) -> String {
    format!("{date_stamp}/{region}/{S3_SERVICE}/aws4_request")
}

fn signing_key(secret: &str, date_stamp: &str, region: &str) -> Vec<u8> {
    let k_date = hmac(format!("AWS4{secret}").as_bytes(), date_stamp.as_bytes());
    let k_region = hmac(&k_date, region.as_bytes());
    let k_service = hmac(&k_region, S3_SERVICE.as_bytes());
    hmac(&k_service, b"aws4_request")
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts arbitrary key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn sha256_hex(data: &[u8]) -> String {
    hex_encode(&Sha256::digest(data))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Sorts headers by name (they must already be lowercase) and returns the
/// `SignedHeaders` list plus the canonical headers block (each `name:value\n`).
fn canonicalize_headers(headers: &[(String, String)]) -> (String, String) {
    let mut headers = headers.to_vec();
    headers.sort_by(|a, b| a.0.cmp(&b.0));
    let signed = headers
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>()
        .join(";");
    let canonical = headers
        .iter()
        .map(|(name, value)| format!("{}:{}\n", name, collapse_spaces(value.trim())))
        .collect::<String>();
    (signed, canonical)
}

/// Collapses sequential spaces per SigV4 canonical-header rules.
fn collapse_spaces(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut pending_space = false;
    for ch in value.chars() {
        if ch == ' ' {
            pending_space = true;
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(ch);
        }
    }
    out
}

/// Sorts query parameters and renders `name=value` pairs with strict
/// URI-encoding on both sides.
fn canonicalize_query(query: &[(String, String)]) -> String {
    let mut query = query.to_vec();
    query.sort();
    query
        .iter()
        .map(|(name, value)| {
            format!(
                "{}={}",
                uri_encode_component(name),
                uri_encode_component(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// RFC 3986 segment encoding: every byte outside the unreserved set is
/// percent-encoded. `/` is encoded too (callers keep separators explicitly).
fn uri_encode_segment(segment: &str) -> String {
    segment
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// Query-component encoding per SigV4 (same set as segment encoding).
fn uri_encode_component(value: &str) -> String {
    uri_encode_segment(value)
}

/// Encodes an object key for use in a URI path: segments are encoded
/// individually so existing `/` separators are preserved.
pub fn uri_encode_key(key: &str) -> String {
    key.split('/')
        .map(uri_encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

// ---- Minimal XML extraction ------------------------------------------------
// ListObjectsV2/DeleteObjects responses are simple flat documents; extracting
// elements with a small scanner avoids pulling in an XML dependency. Content
// is always entity-unescaped before use.

fn parse_list_objects_v2(body: &str) -> ListPage {
    ListPage {
        keys: xml_elements(body, "Key"),
        is_truncated: xml_element(body, "IsTruncated")
            .map(|value| value.eq_ignore_ascii_case("true"))
            .unwrap_or(false),
        next_continuation: xml_element(body, "NextContinuationToken")
            .or_else(|| xml_element(body, "NextKeyToken")),
    }
}

#[derive(Debug)]
struct ListPage {
    keys: Vec<String>,
    is_truncated: bool,
    next_continuation: Option<String>,
}

fn parse_delete_errors(body: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("<Error>") {
        rest = &rest[start + "<Error>".len()..];
        let end = rest.find("</Error>").unwrap_or(rest.len());
        let block = &rest[..end];
        let key = xml_element(block, "Key").unwrap_or_default();
        let message = xml_element(block, "Message").unwrap_or_default();
        errors.push(format!("{key}: {message}"));
        rest = &rest[end..];
    }
    errors
}

/// Extracts the first `<name>...</name>` element's text (entity-decoded).
/// Used for flat S3 response fields only — never for nested structures.
fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(xml_unescape(&body[start..end]))
}

fn xml_elements(body: &str, name: &str) -> Vec<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find(&open) {
        rest = &rest[start + open.len()..];
        let Some(end) = rest.find(&close) else {
            break;
        };
        out.push(xml_unescape(&rest[..end]));
        rest = &rest[end..];
    }
    out
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        // &amp; must come last so a literal "&lt;" stays "<" text, not a tag.
        .replace("&amp;", "&")
}

fn truncate_body(body: &str) -> String {
    let single_line = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= MAX_ERROR_BODY_CHARS {
        return single_line;
    }
    format!(
        "{}…",
        single_line
            .chars()
            .take(MAX_ERROR_BODY_CHARS)
            .collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn test_client(endpoint: Option<&str>, path_style: bool) -> S3Client {
        S3Client {
            http: reqwest::Client::new(),
            settings: S3Settings {
                bucket: "examplebucket".to_owned(),
                endpoint: endpoint.map(str::to_owned),
                region: "us-east-1".to_owned(),
                access_key_id: "AKIAIOSFODNN7EXAMPLE".to_owned(),
                secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_owned(),
                key_prefix: String::new(),
                path_style,
                presign_ttl_seconds: DEFAULT_PRESIGN_TTL_SECONDS,
            },
            endpoint: EndpointParts {
                scheme: "https".to_owned(),
                host: "s3.amazonaws.com".to_owned(),
                path_prefix: String::new(),
            },
            fallback_rt: std::sync::OnceLock::new(),
        }
    }

    /// SigV4 presigned-URL known-answer test. The expected signature was
    /// cross-checked against botocore's `S3SigV4QueryAuth` reference
    /// implementation with the same inputs.
    #[test]
    fn presigned_url_matches_reference_signature() {
        let client = test_client(Some("https://s3.amazonaws.com"), false);
        let now = Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap();
        let url = client.presigned_get_url_at("test.txt", Duration::from_secs(86400), now);
        assert_eq!(
            url,
            "https://examplebucket.s3.amazonaws.com/test.txt?\
             X-Amz-Algorithm=AWS4-HMAC-SHA256&\
             X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request&\
             X-Amz-Date=20130524T000000Z&\
             X-Amz-Expires=86400&\
             X-Amz-SignedHeaders=host&\
             X-Amz-Signature=3ed0be64024db54d5574a27da223529635c383f911f80e636f0ccc13890053d2"
                .replace(char::is_whitespace, "")
        );
    }

    /// Signing-key derivation KAT for the s3 service (computed with the
    /// standard HMAC chain and verified against a reference implementation).
    #[test]
    fn signing_key_matches_reference() {
        let key = signing_key(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20120215",
            "us-east-1",
        );
        assert_eq!(
            hex_encode(&key),
            "f22dfcb5da3bb8d6c924d26e9bb28e9f96ba691a1b1791d907c06f10ede94251"
        );
    }

    #[test]
    fn uri_encode_key_preserves_separators_and_encodes_bytes() {
        assert_eq!(
            uri_encode_key("guild/chan/meeting name/あ.wav"),
            "guild/chan/meeting%20name/%E3%81%82.wav"
        );
        assert_eq!(uri_encode_key("a+b/c~d"), "a%2Bb/c~d");
    }

    #[test]
    fn canonical_path_honours_path_style_and_prefix() {
        let endpoint = EndpointParts {
            scheme: "http".to_owned(),
            host: "localhost:9000".to_owned(),
            path_prefix: "s3".to_owned(),
        };
        let mut settings = S3Settings {
            bucket: "b".to_owned(),
            endpoint: Some("http://localhost:9000/s3".to_owned()),
            region: DEFAULT_REGION.to_owned(),
            access_key_id: "x".to_owned(),
            secret_access_key: "y".to_owned(),
            key_prefix: String::new(),
            path_style: true,
            presign_ttl_seconds: DEFAULT_PRESIGN_TTL_SECONDS,
        };
        assert_eq!(endpoint.canonical_path(&settings, "k/1"), "/s3/b/k/1");
        settings.path_style = false;
        assert_eq!(endpoint.canonical_path(&settings, "k/1"), "/s3/k/1");
    }

    #[test]
    fn list_objects_v2_parses_keys_and_pagination() {
        let page = parse_list_objects_v2(
            r#"<?xml version="1.0"?>
            <ListBucketResult>
              <IsTruncated>true</IsTruncated>
              <Contents><Key>a/b&amp;c.wav</Key></Contents>
              <Contents><Key>a/d.wav</Key></Contents>
              <NextContinuationToken>tok123</NextContinuationToken>
            </ListBucketResult>"#,
        );
        assert_eq!(page.keys, vec!["a/b&c.wav", "a/d.wav"]);
        assert!(page.is_truncated);
        assert_eq!(page.next_continuation.as_deref(), Some("tok123"));
    }

    #[test]
    fn delete_errors_are_parsed_per_key() {
        let errors = parse_delete_errors(
            r#"<DeleteResult>
              <Deleted><Key>ok.wav</Key></Deleted>
              <Error><Key>bad.wav</Key><Code>AccessDenied</Code><Message>denied</Message></Error>
            </DeleteResult>"#,
        );
        assert_eq!(errors, vec!["bad.wav: denied"]);
    }

    #[test]
    fn endpoint_validation_rejects_bad_urls() {
        assert!(validate_endpoint("https://minio.example.com").is_ok());
        assert!(validate_endpoint("http://localhost:9000/sub").is_ok());
        assert!(validate_endpoint("ftp://host").is_err());
        assert!(validate_endpoint("https://host/path?q=1").is_err());
        assert!(validate_endpoint("not a url").is_err());
    }

    #[test]
    fn settings_debug_redacts_secret_access_key() {
        let settings = test_client(None, true).settings.clone();
        let rendered = format!("{settings:?}");
        assert!(rendered.contains("[redacted]"));
        assert!(!rendered.contains("wJalrXUtnFEMI"));
    }

    #[test]
    fn endpoint_path_prefix_decodes_then_reencodes_segments() {
        let endpoint = EndpointParts::resolve(&S3Settings {
            bucket: "b".to_owned(),
            endpoint: Some("http://localhost:9000/my%20store".to_owned()),
            region: DEFAULT_REGION.to_owned(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            key_prefix: String::new(),
            path_style: true,
            presign_ttl_seconds: DEFAULT_PRESIGN_TTL_SECONDS,
        })
        .expect("endpoint should resolve");
        assert_eq!(endpoint.path_prefix, "my%20store");
    }

    #[test]
    fn endpoint_path_prefix_keeps_unencoded_characters() {
        let endpoint = EndpointParts::resolve(&S3Settings {
            bucket: "b".to_owned(),
            endpoint: Some("http://localhost:9000/桶".to_owned()),
            region: DEFAULT_REGION.to_owned(),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            key_prefix: String::new(),
            path_style: true,
            presign_ttl_seconds: DEFAULT_PRESIGN_TTL_SECONDS,
        })
        .expect("endpoint should resolve");
        assert_eq!(endpoint.path_prefix, "%E6%A1%B6");
    }

    /// Constructing and dropping the client inside a Tokio runtime must not
    /// panic: the fallback runtime is lazily built and shut down without
    /// blocking (regression test for the async-context drop restriction).
    #[test]
    fn client_drops_cleanly_inside_async_context() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let _client = S3Client::new(test_client(None, true).settings.clone()).unwrap();
            // Client dropped here, inside the runtime's thread.
        });
    }
}
