//! Minimal HTTP layer for the M2 network code (upstream: `ModNet.vb`'s
//! `NetRequestByClient*` family).
//!
//! The login/download code talks to the [`HttpClient`] trait so unit
//! tests can inject scripted responses; [`ReqwestClient`] is the real
//! transport (reqwest blocking; TLS backend per target — OS-native
//! on Windows like PCL's .NET client, rustls elsewhere; see the
//! Cargo.toml note on ALPN).
//!
//! Error model mirrors the original's `HttpRequestCodeException`: a
//! non-2xx status is an error that still carries the response body,
//! because several upstream flows (XSTS, step-1 refresh, yggdrasil)
//! parse error payloads to decide what to do next.

use std::time::Duration;

/// HTTP method subset the port needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Head,
}

/// One HTTP request. `body` is sent verbatim; `content_type` sets the
/// Content-Type header when present.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub content_type: Option<String>,
    /// Per-request timeout; the retry wrapper varies it per attempt
    /// exactly like `NetRequestByClientRetry` does.
    pub timeout: Duration,
}

/// Percent-encode one form field (space → %20, matching the
/// upstream request strings).
pub fn form_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

impl HttpRequest {
    pub fn get(url: impl Into<String>) -> HttpRequest {
        HttpRequest {
            method: Method::Get,
            url: url.into(),
            headers: Vec::new(),
            body: None,
            content_type: None,
            timeout: Duration::from_secs(10),
        }
    }

    pub fn post(url: impl Into<String>) -> HttpRequest {
        let mut req = HttpRequest::get(url);
        req.method = Method::Post;
        req
    }

    /// POST a JSON body (the Xbox / yggdrasil request shape).
    pub fn post_json(url: &str, body: &str) -> HttpRequest {
        HttpRequest::post(url).json(body)
    }

    /// POST form fields, percent-encoded
    /// (`application/x-www-form-urlencoded`, the OAuth shape).
    pub fn post_form(url: &str, fields: &[(&str, &str)]) -> HttpRequest {
        let body = fields
            .iter()
            .map(|(k, v)| format!("{}={}", form_encode(k), form_encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        HttpRequest::post(url).form(body)
    }

    pub fn header(mut self, name: &str, value: &str) -> HttpRequest {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn form(mut self, body: impl Into<String>) -> HttpRequest {
        self.body = Some(body.into().into_bytes());
        self.content_type = Some("application/x-www-form-urlencoded".to_string());
        self
    }

    pub fn json(mut self, body: impl Into<String>) -> HttpRequest {
        self.body = Some(body.into().into_bytes());
        self.content_type = Some("application/json".to_string());
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> HttpRequest {
        self.timeout = timeout;
        self
    }
}

/// A completed HTTP response (status + headers + full body).
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Case-insensitive header lookup (e.g. `Content-Range` for the
    /// download engine's size probe).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Parse the body as JSON (`DeserializeJson` in the original).
    pub fn json(&self) -> Result<serde_json::Value, HttpError> {
        serde_json::from_slice(&self.body).map_err(|e| HttpError::InvalidJson(e.to_string()))
    }
}

/// Transport failure or an HTTP error status (the original's
/// `HttpRequestCodeException` case carries status + body).
#[derive(Debug, Clone)]
pub enum HttpError {
    /// Network-level failure (DNS, connect, TLS, timeout, ...).
    Transport(String),
    /// Server answered with a non-2xx status; body preserved.
    Status { status: u16, body: String },
    /// `RequireJson` was set but the body is not valid JSON.
    InvalidJson(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(msg) => write!(f, "{msg}"),
            Self::Status { status, body } => {
                if body.is_empty() {
                    write!(f, "远程服务器返回错误代码（{status}）")
                } else {
                    write!(f, "远程服务器返回错误代码（{status}）：{body}")
                }
            }
            Self::InvalidJson(msg) => write!(f, "JSON 解析失败：{msg}"),
        }
    }
}
impl std::error::Error for HttpError {}

impl HttpError {
    /// The HTTP status when this is a [`HttpError::Status`].
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Status { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// The response body for status errors ("" otherwise), which the
    /// upstream login flows substring-match against.
    pub fn response_body(&self) -> &str {
        match self {
            Self::Status { body, .. } => body,
            _ => "",
        }
    }

    /// `Exception.IsBadNetwork` as used by the login flows: transport
    /// failures count as bad network, HTTP statuses do not.
    pub fn is_bad_network(&self) -> bool {
        matches!(self, Self::Transport(_))
    }
}

/// Synchronous HTTP transport used by all M2 network code.
pub trait HttpClient: Send + Sync {
    /// Perform one request. Non-2xx statuses must be returned as
    /// [`HttpError::Status`], not as an Ok response.
    fn request(&self, req: &HttpRequest) -> Result<HttpResponse, HttpError>;

    /// `NetRequestByClientRetry`: up to 3 attempts — 10s timeout, then
    /// (after a 500ms pause) 30s, then (only if the first two attempts
    /// already took over 5.5s in total) a 4s fast retry. 403 / 404 are
    /// never retried; a 429 pauses 10s before the next attempt (the
    /// pause is modeled by the caller's clock in tests — here it is a
    /// real sleep, as in the original).
    fn request_retry(&self, req: &HttpRequest) -> Result<HttpResponse, HttpError> {
        let start = std::time::Instant::now();
        let mut last_err: Option<HttpError> = None;
        for attempt in 0..3 {
            let timeout = match attempt {
                0 => Duration::from_secs(10),
                1 => Duration::from_secs(30),
                _ => Duration::from_secs(4),
            };
            if attempt == 2 && start.elapsed() <= Duration::from_millis(5500) {
                // The original only makes the third attempt when the
                // first two were slow; otherwise it rethrows.
                break;
            }
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(500));
            }
            let mut attempt_req = req.clone();
            attempt_req.timeout = timeout;
            match self.request(&attempt_req) {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    if matches!(e.status(), Some(403) | Some(404)) {
                        return Err(e);
                    }
                    if e.status() == Some(429) {
                        std::thread::sleep(Duration::from_secs(10));
                    }
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| HttpError::Transport("请求失败".to_string())))
    }

    /// `NetRequestByClientRetry` with a backup URL: the original tries
    /// the main URL's retry cycle, and the authlib-injector metadata
    /// fetch falls back to the backup URL when the first attempt
    /// fails. Modeled as: full retry cycle on the main URL, then one
    /// retry cycle on the backup URL.
    fn request_retry_backup(
        &self,
        req: &HttpRequest,
        backup_url: &str,
    ) -> Result<HttpResponse, HttpError> {
        match self.request_retry(req) {
            Ok(resp) => Ok(resp),
            Err(first) => {
                let mut backup = req.clone();
                backup.url = backup_url.to_string();
                self.request_retry(&backup).map_err(|_| first)
            }
        }
    }
}

/// Real transport backed by reqwest (blocking).
pub struct ReqwestClient {
    client: reqwest::blocking::Client,
}

impl Default for ReqwestClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ReqwestClient {
    pub fn new() -> ReqwestClient {
        let mut builder = reqwest::blocking::Client::builder()
            // PCL identifies itself through the default .NET UA; a
            // plain descriptive UA is the closest equivalent.
            .user_agent("Plain Craft Launcher (Rust port)");
        // Honor the standard proxy environment variables: PCL's
        // .NET client goes through the system proxy, and reqwest
        // only reads these variables with its default features,
        // which this crate disables. NO_PROXY is honored through
        // reqwest's env-based NoProxy. Values are never logged
        // (they may carry credentials).
        let env_first = |names: &[&str]| {
            names
                .iter()
                .find_map(|name| std::env::var(name).ok())
                .filter(|v| !v.trim().is_empty())
        };
        let https_proxy = env_first(&["HTTPS_PROXY", "https_proxy"]);
        let http_proxy = env_first(&["HTTP_PROXY", "http_proxy"]);
        let all_proxy = env_first(&["ALL_PROXY", "all_proxy"]);
        fn add_proxy(
            proxy: reqwest::Proxy,
            builder: reqwest::blocking::ClientBuilder,
        ) -> reqwest::blocking::ClientBuilder {
            let proxy = proxy.no_proxy(reqwest::NoProxy::from_env());
            builder.proxy(proxy)
        }
        if let Some(url) = &https_proxy {
            if let Ok(proxy) = reqwest::Proxy::https(url) {
                builder = add_proxy(proxy, builder);
            }
        }
        if let Some(url) = &http_proxy {
            if let Ok(proxy) = reqwest::Proxy::http(url) {
                builder = add_proxy(proxy, builder);
            }
        }
        if https_proxy.is_none() && http_proxy.is_none() {
            if let Some(url) = &all_proxy {
                if let Ok(proxy) = reqwest::Proxy::all(url) {
                    builder = add_proxy(proxy, builder);
                }
            }
        }
        let client = builder
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        ReqwestClient { client }
    }
}

impl HttpClient for ReqwestClient {
    fn request(&self, req: &HttpRequest) -> Result<HttpResponse, HttpError> {
        let method = match req.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
            Method::Head => reqwest::Method::HEAD,
        };
        let mut builder = self.client.request(method, &req.url).timeout(req.timeout);
        for (name, value) in &req.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some(content_type) = &req.content_type {
            builder = builder.header("Content-Type", content_type.as_str());
        }
        if let Some(body) = &req.body {
            builder = builder.body(body.clone());
        }
        let resp = builder
            .send()
            .map_err(|e| HttpError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(n, v)| {
                (
                    n.as_str().to_string(),
                    v.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let body = resp
            .bytes()
            .map_err(|e| HttpError::Transport(e.to_string()))?
            .to_vec();
        if (200..300).contains(&status) {
            Ok(HttpResponse {
                status,
                headers,
                body,
            })
        } else {
            Err(HttpError::Status {
                status,
                body: String::from_utf8_lossy(&body).into_owned(),
            })
        }
    }
}

/// Scripted transport for tests: each request pops the next scripted
/// result; every request is recorded for assertions.
#[cfg(test)]
pub struct FakeHttp {
    pub script: std::sync::Mutex<Vec<Result<HttpResponse, HttpError>>>,
    pub requests: std::sync::Mutex<Vec<HttpRequest>>,
}

#[cfg(test)]
impl FakeHttp {
    pub fn new(script: Vec<Result<HttpResponse, HttpError>>) -> FakeHttp {
        FakeHttp {
            script: std::sync::Mutex::new(script.into_iter().rev().collect()),
            requests: std::sync::Mutex::new(Vec::new()),
        }
    }

    pub fn ok_json(text: &str) -> Result<HttpResponse, HttpError> {
        Ok(HttpResponse {
            status: 200,
            headers: Vec::new(),
            body: text.as_bytes().to_vec(),
        })
    }

    pub fn status(status: u16, body: &str) -> Result<HttpResponse, HttpError> {
        Err(HttpError::Status {
            status,
            body: body.to_string(),
        })
    }
}

#[cfg(test)]
impl HttpClient for FakeHttp {
    fn request(&self, req: &HttpRequest) -> Result<HttpResponse, HttpError> {
        self.requests.lock().unwrap().push(req.clone());
        self.script
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Err(HttpError::Transport("no scripted response".to_string())))
    }

    // Tests must not sleep 10s on a scripted 429 or pause between
    // retries, so the retry wrapper collapses to a single pass-through
    // loop without delays (attempt counting is still exercised).
    fn request_retry(&self, req: &HttpRequest) -> Result<HttpResponse, HttpError> {
        let mut last: Option<HttpError> = None;
        for _ in 0..3 {
            match self.request(req) {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    if matches!(e.status(), Some(403) | Some(404)) {
                        return Err(e);
                    }
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap())
    }
}
