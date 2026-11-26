//! Pure-Rust Spotify Web API client, pluggable [`HttpTransport`] trait,
//! `reqwest`/`rustls-tls` transport, `curl`-backed transport (zero native TLS/OpenSSL linking),
//! mock test transport, HTTP `429 Retry-After` backoff, `ETag`/`snapshot_id` short-circuiting,
//! and HTTP `403` graceful degradation.

use std::collections::VecDeque;
use std::fmt;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use reqwest::Method as ReqwestMethod;
use reqwest::blocking::Client as ReqwestClient;
use reqwest::header::{CONTENT_LENGTH, HeaderName, HeaderValue};
use vynl_core::model::{
    AlbumSummary, ArtistSummary, ConnectDevice, EpisodeSummary, ImageRef, PlaybackState,
    PlaylistSummary, RepeatMode, SearchResults, TrackSummary, UserProfile,
};

use crate::json::{JsonValue, parse_json, to_json_string};
use crate::pkce::{
    SPOTIFY_TOKEN_URL, TokenBundle, build_token_exchange_body, build_token_refresh_body,
    percent_encode,
};
use crate::{is_gracefully_degradable_status, rate_limit_delay};

/// Default official Spotify Web API v1 base URL.
pub const SPOTIFY_API_BASE_URL: &str = "https://api.spotify.com/v1";

/// HTTP request method supported by [`HttpTransport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    /// HTTP `GET`.
    Get,
    /// HTTP `POST`.
    Post,
    /// HTTP `PUT`.
    Put,
    /// HTTP `DELETE`.
    Delete,
}

impl HttpMethod {
    /// Returns the uppercase HTTP verb string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }
}

/// Outbound HTTP request passed to [`HttpTransport::send`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// HTTP method verb.
    pub method: HttpMethod,
    /// Fully-qualified target URL.
    pub url: String,
    /// HTTP headers as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// Optional UTF-8 request body.
    pub body: Option<String>,
}

impl HttpRequest {
    /// Returns the first header matching `name` (ASCII case-insensitive).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Inbound HTTP response returned by [`HttpTransport::send`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// Numeric HTTP status code (e.g., `200`, `204`, `304`, `403`, `429`).
    pub status: u16,
    /// Response headers as `(name, value)` pairs.
    pub headers: Vec<(String, String)>,
    /// Response body text.
    pub body: String,
}

impl HttpResponse {
    /// Creates a new response with status code and body and no extra headers.
    #[must_use]
    pub fn new(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    /// Appends a response header and returns `self`.
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Returns the first header matching `name` (ASCII case-insensitive).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Typed Spotify Web API error surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    /// Underlying network, subprocess, or I/O transport error.
    Transport(String),
    /// HTTP `401 Unauthorized` — access token expired or revoked.
    Unauthorized,
    /// HTTP `403 Forbidden` — restricted endpoint that degrades gracefully per ADR-0003.
    RestrictedEndpoint {
        /// Spotify endpoint path that returned `403`.
        endpoint: String,
        /// Human-readable error message from Spotify.
        message: String,
    },
    /// HTTP `429 Too Many Requests` after retry budget exhaustion.
    RateLimited {
        /// Computed retry delay honoring `Retry-After` or exponential backoff.
        retry_after: Duration,
        /// Human-readable message from Spotify.
        message: String,
    },
    /// HTTP `404 Not Found`.
    NotFound(String),
    /// Any other unexpected non-2xx HTTP status.
    HttpStatus {
        /// HTTP status code.
        status: u16,
        /// Response body snippet.
        body: String,
    },
    /// JSON or Spotify schema deserialization error.
    Parse(String),
}

impl ApiError {
    /// Returns `true` if this error should degrade gracefully without surfacing a modal.
    #[must_use]
    pub const fn is_gracefully_degradable(&self) -> bool {
        matches!(self, Self::RestrictedEndpoint { .. })
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(msg) => write!(f, "HTTP transport error: {msg}"),
            Self::Unauthorized => write!(f, "Spotify authentication expired (HTTP 401)"),
            Self::RestrictedEndpoint { endpoint, message } => {
                write!(
                    f,
                    "Spotify restricted endpoint `{endpoint}` (HTTP 403): {message}"
                )
            }
            Self::RateLimited {
                retry_after,
                message,
            } => write!(
                f,
                "Spotify rate limited (HTTP 429, retry after {}s): {message}",
                retry_after.as_secs()
            ),
            Self::NotFound(endpoint) => write!(f, "Spotify resource not found: `{endpoint}`"),
            Self::HttpStatus { status, body } => {
                write!(f, "Spotify API returned HTTP {status}: {body}")
            }
            Self::Parse(msg) => write!(f, "Spotify response parse error: {msg}"),
        }
    }
}

impl std::error::Error for ApiError {}

/// Pluggable synchronous HTTP transport seam (allows `curl`, `reqwest`/`rustls`, or mocks).
pub trait HttpTransport {
    /// Executes an HTTP request and returns the raw HTTP response.
    ///
    /// # Errors
    /// Returns [`ApiError::Transport`] when the request cannot be completed.
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ApiError>;
}

/// Zero-dependency HTTP transport invoking the system `curl` binary via `std::process::Command`.
///
/// Passes request bodies over `stdin` so bearer tokens or form secrets are never exposed
/// as body arguments, while avoiding any `openssl-sys` C linking.
#[derive(Debug, Clone)]
pub struct CurlHttpTransport {
    curl_bin: String,
    timeout_secs: u64,
}

impl Default for CurlHttpTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl CurlHttpTransport {
    /// Creates a `curl` transport with a 15-second request timeout.
    #[must_use]
    pub fn new() -> Self {
        Self {
            curl_bin: String::from("curl"),
            timeout_secs: 15,
        }
    }

    /// Configures the per-request maximum timeout in seconds.
    #[must_use]
    pub const fn with_timeout_secs(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }

    /// Overrides the `curl` executable path (useful for custom environments).
    #[must_use]
    pub fn with_binary(mut self, binary: impl Into<String>) -> Self {
        self.curl_bin = binary.into();
        self
    }
}

impl HttpTransport for CurlHttpTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ApiError> {
        let mut cmd = Command::new(&self.curl_bin);
        cmd.arg("-sS")
            .arg("-i")
            .arg("--max-time")
            .arg(self.timeout_secs.to_string())
            .arg("-X")
            .arg(request.method.as_str());

        for (name, value) in &request.headers {
            cmd.arg("-H").arg(format!("{name}: {value}"));
        }

        let has_body = request.body.is_some();
        if has_body {
            cmd.arg("--data-binary").arg("@-");
            cmd.stdin(Stdio::piped());
        } else {
            if matches!(request.method, HttpMethod::Post | HttpMethod::Put) {
                cmd.arg("-H").arg("Content-Length: 0");
            }
            cmd.stdin(Stdio::null());
        }

        cmd.arg(&request.url);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|err| {
            ApiError::Transport(format!("failed to spawn `{}`: {err}", self.curl_bin))
        })?;

        if let Some(body) = request.body
            && let Some(mut stdin) = child.stdin.take()
        {
            stdin
                .write_all(body.as_bytes())
                .map_err(|err| ApiError::Transport(format!("failed writing curl stdin: {err}")))?;
        }

        let output = child
            .wait_with_output()
            .map_err(|err| ApiError::Transport(format!("failed waiting for curl: {err}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(ApiError::Transport(format!(
                "curl exited with {}: {stderr}",
                output.status
            )));
        }

        let raw = String::from_utf8_lossy(&output.stdout);
        parse_raw_http_response(&raw)
    }
}

/// Pure-Rust HTTP transport backed by `reqwest::blocking::Client` with `rustls-tls`
/// and automatic `gzip` decompression (zero `openssl-sys` linking).
#[derive(Debug, Clone)]
pub struct ReqwestHttpTransport {
    client: ReqwestClient,
    timeout: Duration,
}

impl ReqwestHttpTransport {
    /// Default per-request timeout used by [`ReqwestHttpTransport::new`].
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

    /// Creates a `reqwest`/`rustls-tls` HTTP transport with the default 15-second timeout.
    ///
    /// # Errors
    /// Returns [`ApiError::Transport`] if the underlying `reqwest` client cannot be built.
    pub fn new() -> Result<Self, ApiError> {
        Self::with_timeout(Self::DEFAULT_TIMEOUT)
    }

    /// Creates a `reqwest`/`rustls-tls` HTTP transport with a custom per-request `timeout`.
    ///
    /// # Errors
    /// Returns [`ApiError::Transport`] if the underlying `reqwest` client cannot be built.
    pub fn with_timeout(timeout: Duration) -> Result<Self, ApiError> {
        let client = ReqwestClient::builder()
            .use_rustls_tls()
            .timeout(timeout)
            .build()
            .map_err(|err| {
                ApiError::Transport(format!("failed to initialize reqwest HTTP client: {err}"))
            })?;
        Ok(Self { client, timeout })
    }

    /// Returns the configured per-request timeout.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl HttpTransport for ReqwestHttpTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ApiError> {
        let method = match request.method {
            HttpMethod::Get => ReqwestMethod::GET,
            HttpMethod::Post => ReqwestMethod::POST,
            HttpMethod::Put => ReqwestMethod::PUT,
            HttpMethod::Delete => ReqwestMethod::DELETE,
        };

        let needs_empty_content_length = request.body.is_none()
            && matches!(request.method, HttpMethod::Post | HttpMethod::Put)
            && request.header("Content-Length").is_none();

        let mut builder = self.client.request(method, &request.url);
        for (name, value) in &request.headers {
            let header_name = HeaderName::from_bytes(name.as_bytes()).map_err(|err| {
                ApiError::Transport(format!("invalid HTTP header name `{name}`: {err}"))
            })?;
            let header_value = HeaderValue::from_str(value).map_err(|err| {
                ApiError::Transport(format!("invalid HTTP header value for `{name}`: {err}"))
            })?;
            builder = builder.header(header_name, header_value);
        }

        if let Some(body) = request.body {
            builder = builder.body(body);
        } else if needs_empty_content_length {
            builder = builder.header(CONTENT_LENGTH, 0);
        }

        let response = builder
            .send()
            .map_err(|err| ApiError::Transport(format!("HTTP request failed: {err}")))?;

        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                let text = value.to_str().map_or_else(
                    |_| String::from_utf8_lossy(value.as_bytes()).into_owned(),
                    ToOwned::to_owned,
                );
                (name.as_str().to_owned(), text)
            })
            .collect();
        let body = response.text().map_err(|err| {
            ApiError::Transport(format!("failed reading HTTP response body: {err}"))
        })?;

        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

/// Parses a raw HTTP response produced by `curl -i` (including intermediate `100 Continue`
/// or proxy `200 Connection established` header blocks).
///
/// # Errors
/// Returns [`ApiError::Transport`] when the status line or header block is malformed.
pub fn parse_raw_http_response(raw: &str) -> Result<HttpResponse, ApiError> {
    let mut remainder = raw;

    loop {
        let Some((header_block, rest)) = split_header_and_body(remainder) else {
            return Err(ApiError::Transport(String::from(
                "malformed HTTP response: missing header separator",
            )));
        };

        let mut lines = header_block.lines();
        let Some(status_line) = lines.next() else {
            return Err(ApiError::Transport(String::from(
                "malformed HTTP response: empty status line",
            )));
        };

        let status = parse_status_code(status_line)?;
        let is_intermediate = (100..200).contains(&status)
            || (status == 200
                && status_line
                    .to_ascii_lowercase()
                    .contains("connection established"));

        if is_intermediate && rest.starts_with("HTTP/") {
            remainder = rest;
            continue;
        }

        let mut headers = Vec::new();
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_owned(), value.trim().to_owned()));
            }
        }

        return Ok(HttpResponse {
            status,
            headers,
            body: rest.to_owned(),
        });
    }
}

fn split_header_and_body(raw: &str) -> Option<(&str, &str)> {
    if let Some(pos) = raw.find("\r\n\r\n") {
        Some((&raw[..pos], &raw[pos + 4..]))
    } else if let Some(pos) = raw.find("\n\n") {
        Some((&raw[..pos], &raw[pos + 2..]))
    } else {
        None
    }
}

fn parse_status_code(status_line: &str) -> Result<u16, ApiError> {
    let mut parts = status_line.split_whitespace();
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/") {
        return Err(ApiError::Transport(format!(
            "invalid HTTP status line `{status_line}`"
        )));
    }
    let code_str = parts.next().ok_or_else(|| {
        ApiError::Transport(format!("missing HTTP status code in `{status_line}`"))
    })?;
    code_str
        .parse::<u16>()
        .map_err(|_| ApiError::Transport(format!("invalid HTTP status code `{code_str}`")))
}

/// Thread-safe FIFO mock [`HttpTransport`] for deterministic unit and integration testing.
#[derive(Debug, Default)]
pub struct MockSpotifyTransport {
    responses: Mutex<VecDeque<Result<HttpResponse, ApiError>>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl MockSpotifyTransport {
    /// Creates an empty mock transport.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueues a raw [`HttpResponse`] to be returned by the next request.
    pub fn push_response(&self, response: HttpResponse) {
        self.push_result(Ok(response));
    }

    /// Enqueues a JSON [`HttpResponse`] with `status` and `body`.
    pub fn push_json(&self, status: u16, body: impl Into<String>) {
        self.push_response(
            HttpResponse::new(status, body).with_header("Content-Type", "application/json"),
        );
    }

    /// Enqueues an arbitrary [`Result<HttpResponse, ApiError>`].
    pub fn push_result(&self, result: Result<HttpResponse, ApiError>) {
        if let Ok(mut guard) = self.responses.lock() {
            guard.push_back(result);
        }
    }

    /// Returns a snapshot of all [`HttpRequest`] values observed so far.
    #[must_use]
    pub fn recorded_requests(&self) -> Vec<HttpRequest> {
        self.requests
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }
}

impl HttpTransport for MockSpotifyTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, ApiError> {
        if let Ok(mut reqs) = self.requests.lock() {
            reqs.push(request);
        }
        let next = self
            .responses
            .lock()
            .map_err(|_| ApiError::Transport(String::from("mock transport mutex poisoned")))?
            .pop_front();
        next.unwrap_or_else(|| {
            Err(ApiError::Transport(String::from(
                "mock transport has no queued response",
            )))
        })
    }
}

/// Outcome of [`SpotifyApiClient::playlist_tracks`] supporting `ETag` and `snapshot_id` short-circuiting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaylistTracksOutcome {
    /// Playlist has not changed since `snapshot_id` (HTTP `304` or matching `snapshot_id`).
    NotModified {
        /// Confirmed unchanged Spotify `snapshot_id`.
        snapshot_id: String,
    },
    /// Playlist changed and returned an updated track listing.
    Updated {
        /// Latest Spotify `snapshot_id`.
        snapshot_id: String,
        /// Optional HTTP `ETag` header returned by Spotify.
        etag: Option<String>,
        /// Parsed playlist tracks.
        tracks: Vec<TrackSummary>,
    },
}

/// Typed Spotify Web API client parameterized over [`HttpTransport`].
#[derive(Debug, Clone)]
pub struct SpotifyApiClient<T: HttpTransport> {
    transport: T,
    access_token: String,
    base_url: String,
    max_retries: u32,
    sleep_on_retry: bool,
}

impl SpotifyApiClient<ReqwestHttpTransport> {
    /// Creates a new Spotify API client backed by [`ReqwestHttpTransport`] (`rustls-tls`).
    ///
    /// # Errors
    /// Returns [`ApiError::Transport`] if the underlying `reqwest` client cannot be initialized.
    pub fn new_reqwest(access_token: impl Into<String>) -> Result<Self, ApiError> {
        let transport = ReqwestHttpTransport::new()?;
        Ok(Self::new(transport, access_token))
    }
}

impl<T: HttpTransport> SpotifyApiClient<T> {
    /// Creates a new Spotify API client targeting [`SPOTIFY_API_BASE_URL`].
    #[must_use]
    pub fn new(transport: T, access_token: impl Into<String>) -> Self {
        Self {
            transport,
            access_token: access_token.into(),
            base_url: String::from(SPOTIFY_API_BASE_URL),
            max_retries: 2,
            sleep_on_retry: true,
        }
    }

    /// Overrides the API base URL (helpful for local proxies or integration harnesses).
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        base_url
            .into()
            .trim_end_matches('/')
            .clone_into(&mut self.base_url);
        self
    }

    /// Configures maximum automatic retries when Spotify returns HTTP `429 Too Many Requests`.
    #[must_use]
    pub const fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Enables or disables sleeping during `429` retry backoff (disable in unit tests for instant runs).
    #[must_use]
    pub const fn with_retry_sleep(mut self, sleep_on_retry: bool) -> Self {
        self.sleep_on_retry = sleep_on_retry;
        self
    }

    /// Updates the Bearer access token after an OAuth refresh.
    pub fn set_access_token(&mut self, access_token: impl Into<String>) {
        self.access_token = access_token.into();
    }

    /// Returns a reference to the underlying [`HttpTransport`].
    #[must_use]
    pub const fn transport(&self) -> &T {
        &self.transport
    }

    /// Exchanges an OAuth 2.0 PKCE authorization code for a [`TokenBundle`].
    ///
    /// # Errors
    /// Returns [`ApiError`] if the HTTP request fails, Spotify rejects the code, or JSON is invalid.
    pub fn exchange_pkce_code(
        &self,
        client_id: &str,
        code: &str,
        redirect_uri: &str,
        verifier: &str,
        now_epoch_secs: u64,
    ) -> Result<TokenBundle, ApiError> {
        let body = build_token_exchange_body(client_id, code, redirect_uri, verifier);
        let req = HttpRequest {
            method: HttpMethod::Post,
            url: String::from(SPOTIFY_TOKEN_URL),
            headers: vec![(
                String::from("Content-Type"),
                String::from("application/x-www-form-urlencoded"),
            )],
            body: Some(body),
        };
        let resp = self.transport.send(req)?;
        if !(200..300).contains(&resp.status) {
            return Err(ApiError::HttpStatus {
                status: resp.status,
                body: resp.body,
            });
        }
        let json = parse_json(&resp.body).map_err(ApiError::Parse)?;
        TokenBundle::from_token_response(&json, now_epoch_secs, None).map_err(ApiError::Parse)
    }

    /// Refreshes an expired OAuth access token using `refresh_token`.
    ///
    /// # Errors
    /// Returns [`ApiError`] if the token endpoint fails or returns an invalid payload.
    pub fn refresh_access_token(
        &self,
        client_id: &str,
        refresh_token: &str,
        now_epoch_secs: u64,
    ) -> Result<TokenBundle, ApiError> {
        let body = build_token_refresh_body(client_id, refresh_token);
        let req = HttpRequest {
            method: HttpMethod::Post,
            url: String::from(SPOTIFY_TOKEN_URL),
            headers: vec![(
                String::from("Content-Type"),
                String::from("application/x-www-form-urlencoded"),
            )],
            body: Some(body),
        };
        let resp = self.transport.send(req)?;
        if !(200..300).contains(&resp.status) {
            return Err(ApiError::HttpStatus {
                status: resp.status,
                body: resp.body,
            });
        }
        let json = parse_json(&resp.body).map_err(ApiError::Parse)?;
        TokenBundle::from_token_response(&json, now_epoch_secs, Some(refresh_token))
            .map_err(ApiError::Parse)
    }

    /// Fetches the currently authenticated Spotify user profile (`GET /me`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, rate-limit, or parse failures.
    pub fn me(&self) -> Result<UserProfile, ApiError> {
        let resp = self.send_api(HttpMethod::Get, "/me", &[], None)?;
        let json = parse_json(&resp.body).map_err(ApiError::Parse)?;
        parse_user_profile(&json)
    }

    /// Fetches the current playback state (`GET /me/player`).
    ///
    /// Returns `Ok(None)` when Spotify responds with HTTP `204 No Content`.
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, rate-limit, or parse failures.
    pub fn playback_state(&self) -> Result<Option<PlaybackState>, ApiError> {
        let resp = self.send_api(HttpMethod::Get, "/me/player", &[], None)?;
        if resp.status == 204 || resp.body.trim().is_empty() {
            return Ok(None);
        }
        let json = parse_json(&resp.body).map_err(ApiError::Parse)?;
        parse_playback_state(&json).map(Some)
    }

    /// Lists available Spotify Connect devices (`GET /me/player/devices`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, rate-limit, or parse failures.
    pub fn devices(&self) -> Result<Vec<ConnectDevice>, ApiError> {
        let resp = self.send_api(HttpMethod::Get, "/me/player/devices", &[], None)?;
        let json = parse_json(&resp.body).map_err(ApiError::Parse)?;
        let Some(items) = json.get_array("devices") else {
            return Ok(Vec::new());
        };
        Ok(items.iter().filter_map(parse_device).collect())
    }

    /// Starts or resumes playback on the active device (`PUT /me/player/play`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, or rate-limit failures.
    pub fn play(&self) -> Result<(), ApiError> {
        self.send_api(HttpMethod::Put, "/me/player/play", &[], None)?;
        Ok(())
    }

    /// Starts playback of specific Spotify track or episode URIs (`PUT /me/player/play`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, or rate-limit failures.
    pub fn play_uris(&self, uris: &[&str]) -> Result<(), ApiError> {
        let payload = JsonValue::object([(
            "uris",
            JsonValue::Array(uris.iter().copied().map(JsonValue::from).collect()),
        )]);
        let body = to_json_string(&payload);
        let headers = [(
            String::from("Content-Type"),
            String::from("application/json"),
        )];
        self.send_api(HttpMethod::Put, "/me/player/play", &headers, Some(&body))?;
        Ok(())
    }

    /// Pauses playback on the active device (`PUT /me/player/pause`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, or rate-limit failures.
    pub fn pause(&self) -> Result<(), ApiError> {
        self.send_api(HttpMethod::Put, "/me/player/pause", &[], None)?;
        Ok(())
    }

    /// Skips to the next track in the playback queue (`POST /me/player/next`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, or rate-limit failures.
    pub fn next_track(&self) -> Result<(), ApiError> {
        self.send_api(HttpMethod::Post, "/me/player/next", &[], None)?;
        Ok(())
    }

    /// Skips to the previous track (`POST /me/player/previous`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, or rate-limit failures.
    pub fn prev_track(&self) -> Result<(), ApiError> {
        self.send_api(HttpMethod::Post, "/me/player/previous", &[], None)?;
        Ok(())
    }

    /// Seeks playback to `position_ms` (`PUT /me/player/seek`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, or rate-limit failures.
    pub fn seek(&self, position_ms: u64) -> Result<(), ApiError> {
        let path = format!("/me/player/seek?position_ms={position_ms}");
        self.send_api(HttpMethod::Put, &path, &[], None)?;
        Ok(())
    }

    /// Sets playback volume (`0..=100`, `PUT /me/player/volume`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, or rate-limit failures.
    pub fn set_volume(&self, volume_percent: u8) -> Result<(), ApiError> {
        let clamped = volume_percent.min(100);
        let path = format!("/me/player/volume?volume_percent={clamped}");
        self.send_api(HttpMethod::Put, &path, &[], None)?;
        Ok(())
    }

    /// Toggles shuffle state (`PUT /me/player/shuffle`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, or rate-limit failures.
    pub fn set_shuffle(&self, enabled: bool) -> Result<(), ApiError> {
        let path = format!("/me/player/shuffle?state={enabled}");
        self.send_api(HttpMethod::Put, &path, &[], None)?;
        Ok(())
    }

    /// Sets playback repeat mode (`PUT /me/player/repeat`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, or rate-limit failures.
    pub fn set_repeat(&self, mode: RepeatMode) -> Result<(), ApiError> {
        let path = format!("/me/player/repeat?state={}", mode.as_api_str());
        self.send_api(HttpMethod::Put, &path, &[], None)?;
        Ok(())
    }

    /// Transfers active playback to Spotify Connect `device_id` (`PUT /me/player`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, or rate-limit failures.
    pub fn transfer_playback(&self, device_id: &str) -> Result<(), ApiError> {
        let payload = JsonValue::object([
            (
                "device_ids",
                JsonValue::Array(vec![JsonValue::from(device_id)]),
            ),
            ("play", JsonValue::Bool(true)),
        ]);
        let body = to_json_string(&payload);
        let headers = [(
            String::from("Content-Type"),
            String::from("application/json"),
        )];
        self.send_api(HttpMethod::Put, "/me/player", &headers, Some(&body))?;
        Ok(())
    }

    /// Fetches the user's Liked Songs (`GET /me/tracks?limit=50`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, rate-limit, or parse failures.
    pub fn liked_tracks(&self) -> Result<Vec<TrackSummary>, ApiError> {
        let resp = self.send_api(HttpMethod::Get, "/me/tracks?limit=50", &[], None)?;
        let json = parse_json(&resp.body).map_err(ApiError::Parse)?;
        let Some(items) = json.get_array("items") else {
            return Ok(Vec::new());
        };
        let mut tracks = Vec::with_capacity(items.len());
        for entry in items {
            if let Some(track_val) = entry.get("track")
                && let Some(mut track) = parse_track(track_val)
            {
                track.is_liked = true;
                tracks.push(track);
            }
        }
        Ok(tracks)
    }

    /// Saves (`liked == true`) or removes (`liked == false`) a track in Liked Songs (`PUT`/`DELETE /me/tracks`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, or rate-limit failures.
    pub fn toggle_like(&self, track_id: &str, liked: bool) -> Result<(), ApiError> {
        let method = if liked {
            HttpMethod::Put
        } else {
            HttpMethod::Delete
        };
        let path = format!("/me/tracks?ids={}", percent_encode(track_id));
        self.send_api(method, &path, &[], None)?;
        Ok(())
    }

    /// Lists the user's playlists (`GET /me/playlists?limit=50`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, rate-limit, or parse failures.
    pub fn playlists(&self) -> Result<Vec<PlaylistSummary>, ApiError> {
        let resp = self.send_api(HttpMethod::Get, "/me/playlists?limit=50", &[], None)?;
        let json = parse_json(&resp.body).map_err(ApiError::Parse)?;
        let Some(items) = json.get_array("items") else {
            return Ok(Vec::new());
        };
        Ok(items.iter().filter_map(parse_playlist).collect())
    }

    /// Fetches tracks for `playlist_id`, honoring `cached_snapshot_id` via `If-None-Match`
    /// and short-circuiting both HTTP `304 Not Modified` and matching `snapshot_id` responses.
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, rate-limit, or parse failures.
    pub fn playlist_tracks(
        &self,
        playlist_id: &str,
        cached_snapshot_id: Option<&str>,
    ) -> Result<PlaylistTracksOutcome, ApiError> {
        let mut extra_headers = Vec::new();
        if let Some(snap) = cached_snapshot_id.filter(|s| !s.is_empty()) {
            let etag = if snap.starts_with('"') {
                snap.to_owned()
            } else {
                format!("\"{snap}\"")
            };
            extra_headers.push((String::from("If-None-Match"), etag));
        }

        let path = format!(
            "/playlists/{}?fields=snapshot_id,tracks.items(track(id,name,duration_ms,explicit,artists(name),album(name,images)))",
            percent_encode(playlist_id)
        );
        let resp = self.send_api(HttpMethod::Get, &path, &extra_headers, None)?;

        if resp.status == 304 {
            return Ok(PlaylistTracksOutcome::NotModified {
                snapshot_id: cached_snapshot_id.unwrap_or_default().to_owned(),
            });
        }

        let json = parse_json(&resp.body).map_err(ApiError::Parse)?;
        let snapshot_id = json.get_str("snapshot_id").unwrap_or_default().to_owned();

        if let Some(cached) = cached_snapshot_id
            && !cached.is_empty()
            && snapshot_id == cached
        {
            return Ok(PlaylistTracksOutcome::NotModified { snapshot_id });
        }

        let etag = resp.header("ETag").map(ToOwned::to_owned);
        let mut tracks = Vec::new();
        if let Some(items) = json
            .get_path(&["tracks", "items"])
            .and_then(JsonValue::as_array)
        {
            for entry in items {
                if let Some(track_val) = entry.get("track")
                    && let Some(track) = parse_track(track_val)
                {
                    tracks.push(track);
                }
            }
        }

        Ok(PlaylistTracksOutcome::Updated {
            snapshot_id,
            etag,
            tracks,
        })
    }

    /// Searches Spotify across tracks, albums, artists, playlists, and episodes (`GET /search`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, rate-limit, or parse failures.
    pub fn search(&self, query: &str) -> Result<SearchResults, ApiError> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(SearchResults::default());
        }

        let path = format!(
            "/search?q={}&type=track,album,artist,playlist,episode&limit=20",
            percent_encode(trimmed)
        );
        let resp = self.send_api(HttpMethod::Get, &path, &[], None)?;
        let json = parse_json(&resp.body).map_err(ApiError::Parse)?;

        let tracks = json
            .get_path(&["tracks", "items"])
            .and_then(JsonValue::as_array)
            .map(|items| items.iter().filter_map(parse_track).collect())
            .unwrap_or_default();
        let albums = json
            .get_path(&["albums", "items"])
            .and_then(JsonValue::as_array)
            .map(|items| items.iter().filter_map(parse_album).collect())
            .unwrap_or_default();
        let artists = json
            .get_path(&["artists", "items"])
            .and_then(JsonValue::as_array)
            .map(|items| items.iter().filter_map(parse_artist).collect())
            .unwrap_or_default();
        let playlists = json
            .get_path(&["playlists", "items"])
            .and_then(JsonValue::as_array)
            .map(|items| items.iter().filter_map(parse_playlist).collect())
            .unwrap_or_default();
        let episodes = json
            .get_path(&["episodes", "items"])
            .and_then(JsonValue::as_array)
            .map(|items| items.iter().filter_map(parse_episode).collect())
            .unwrap_or_default();

        Ok(SearchResults {
            query: trimmed.to_owned(),
            tracks,
            albums,
            artists,
            playlists,
            episodes,
        })
    }

    /// Appends a track or episode URI to the active playback queue (`POST /me/player/queue`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, or rate-limit failures.
    pub fn queue_track(&self, track_uri: &str) -> Result<(), ApiError> {
        let path = format!("/me/player/queue?uri={}", percent_encode(track_uri));
        self.send_api(HttpMethod::Post, &path, &[], None)?;
        Ok(())
    }

    /// Fetches the user's recently played tracks (`GET /me/player/recently-played`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, rate-limit, or parse failures.
    pub fn recently_played(&self) -> Result<Vec<TrackSummary>, ApiError> {
        let resp = self.send_api(
            HttpMethod::Get,
            "/me/player/recently-played?limit=50",
            &[],
            None,
        )?;
        let json = parse_json(&resp.body).map_err(ApiError::Parse)?;
        let Some(items) = json.get_array("items") else {
            return Ok(Vec::new());
        };
        Ok(items
            .iter()
            .filter_map(|entry| entry.get("track").and_then(parse_track))
            .collect())
    }

    /// Fetches the user's saved albums (`GET /me/albums?limit=50`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, rate-limit, or parse failures.
    pub fn saved_albums(&self) -> Result<Vec<AlbumSummary>, ApiError> {
        let resp = self.send_api(HttpMethod::Get, "/me/albums?limit=50", &[], None)?;
        let json = parse_json(&resp.body).map_err(ApiError::Parse)?;
        let Some(items) = json.get_array("items") else {
            return Ok(Vec::new());
        };
        Ok(items
            .iter()
            .filter_map(|entry| entry.get("album").and_then(parse_album))
            .collect())
    }

    /// Fetches tracks belonging to `album_id` (`GET /albums/{album_id}/tracks?limit=50`).
    ///
    /// Spotify's album tracks endpoint omits the nested `album` object on each item,
    /// so returned tracks default their `album` field to `""`.
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, rate-limit, or parse failures.
    pub fn album_tracks(&self, album_id: &str) -> Result<Vec<TrackSummary>, ApiError> {
        let path = format!("/albums/{}/tracks?limit=50", percent_encode(album_id));
        let resp = self.send_api(HttpMethod::Get, &path, &[], None)?;
        let json = parse_json(&resp.body).map_err(ApiError::Parse)?;
        let Some(items) = json.get_array("items") else {
            return Ok(Vec::new());
        };
        Ok(items
            .iter()
            .filter_map(|item| parse_track_with_album_fallback(item, ""))
            .collect())
    }

    /// Lists artists followed by the current user (`GET /me/following?type=artist&limit=50`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, rate-limit, or parse failures.
    pub fn followed_artists(&self) -> Result<Vec<ArtistSummary>, ApiError> {
        let resp = self.send_api(
            HttpMethod::Get,
            "/me/following?type=artist&limit=50",
            &[],
            None,
        )?;
        let json = parse_json(&resp.body).map_err(ApiError::Parse)?;
        let Some(items) = json
            .get_path(&["artists", "items"])
            .and_then(JsonValue::as_array)
        else {
            return Ok(Vec::new());
        };
        let mut artists = Vec::with_capacity(items.len());
        for entry in items {
            if let Some(mut artist) = parse_artist(entry) {
                artist.following = true;
                artists.push(artist);
            }
        }
        Ok(artists)
    }

    /// Fetches an artist's top tracks (`GET /artists/{artist_id}/top-tracks`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, rate-limit, or parse failures.
    pub fn artist_top_tracks(&self, artist_id: &str) -> Result<Vec<TrackSummary>, ApiError> {
        let path = format!("/artists/{}/top-tracks", percent_encode(artist_id));
        let resp = self.send_api(HttpMethod::Get, &path, &[], None)?;
        let json = parse_json(&resp.body).map_err(ApiError::Parse)?;
        let Some(items) = json.get_array("tracks") else {
            return Ok(Vec::new());
        };
        Ok(items.iter().filter_map(parse_track).collect())
    }

    /// Fetches the user's saved podcast episodes (`GET /me/episodes?limit=50`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, rate-limit, or parse failures.
    pub fn saved_episodes(&self) -> Result<Vec<EpisodeSummary>, ApiError> {
        let resp = self.send_api(HttpMethod::Get, "/me/episodes?limit=50", &[], None)?;
        let json = parse_json(&resp.body).map_err(ApiError::Parse)?;
        let Some(items) = json.get_array("items") else {
            return Ok(Vec::new());
        };
        Ok(items
            .iter()
            .filter_map(|entry| entry.get("episode").and_then(parse_episode))
            .collect())
    }

    /// Appends one or more track or episode URIs to `playlist_id`
    /// (`POST /playlists/{playlist_id}/tracks`).
    ///
    /// # Errors
    /// Returns [`ApiError`] on transport, auth, restriction, or rate-limit failures.
    pub fn add_tracks_to_playlist(
        &self,
        playlist_id: &str,
        track_uris: &[&str],
    ) -> Result<(), ApiError> {
        let payload = JsonValue::object([(
            "uris",
            JsonValue::Array(track_uris.iter().copied().map(JsonValue::from).collect()),
        )]);
        let body = to_json_string(&payload);
        let headers = [(
            String::from("Content-Type"),
            String::from("application/json"),
        )];
        let path = format!("/playlists/{}/tracks", percent_encode(playlist_id));
        self.send_api(HttpMethod::Post, &path, &headers, Some(&body))?;
        Ok(())
    }

    fn send_api(
        &self,
        method: HttpMethod,
        path: &str,
        extra_headers: &[(String, String)],
        body: Option<&str>,
    ) -> Result<HttpResponse, ApiError> {
        let url = format!("{}{path}", self.base_url);
        let mut attempt = 0_u32;

        loop {
            let mut headers = Vec::with_capacity(extra_headers.len() + 2);
            headers.push((
                String::from("Authorization"),
                format!("Bearer {}", self.access_token),
            ));
            headers.push((String::from("Accept"), String::from("application/json")));
            headers.extend(extra_headers.iter().cloned());

            let req = HttpRequest {
                method,
                url: url.clone(),
                headers,
                body: body.map(ToOwned::to_owned),
            };

            let resp = self.transport.send(req)?;

            if (200..300).contains(&resp.status) || resp.status == 304 {
                return Ok(resp);
            }

            if resp.status == 429 {
                let retry_after_secs = resp
                    .header("Retry-After")
                    .and_then(|v| v.trim().parse::<u64>().ok());
                let delay = rate_limit_delay(retry_after_secs, attempt);
                if attempt < self.max_retries {
                    if self.sleep_on_retry {
                        thread::sleep(delay);
                    }
                    attempt += 1;
                    continue;
                }
                let message = extract_spotify_error_message(&resp.body)
                    .unwrap_or_else(|| String::from("Too many requests"));
                return Err(ApiError::RateLimited {
                    retry_after: delay,
                    message,
                });
            }

            if is_gracefully_degradable_status(resp.status) {
                let message = extract_spotify_error_message(&resp.body)
                    .unwrap_or_else(|| String::from("Restricted Spotify Web API endpoint"));
                return Err(ApiError::RestrictedEndpoint {
                    endpoint: path.to_owned(),
                    message,
                });
            }

            return match resp.status {
                401 => Err(ApiError::Unauthorized),
                404 => Err(ApiError::NotFound(path.to_owned())),
                status => Err(ApiError::HttpStatus {
                    status,
                    body: resp.body,
                }),
            };
        }
    }
}

fn extract_spotify_error_message(body: &str) -> Option<String> {
    let parsed = parse_json(body).ok()?;
    parsed
        .get_path(&["error", "message"])
        .and_then(JsonValue::as_str)
        .map(ToOwned::to_owned)
}

fn parse_first_image(value: &JsonValue) -> Option<ImageRef> {
    let images = value.get_array("images")?;
    let first = images.first()?;
    let url = first.get_str("url")?.to_owned();
    Some(ImageRef {
        url,
        width: first.get_u32("width"),
        height: first.get_u32("height"),
    })
}

fn parse_artist_names(value: &JsonValue) -> Vec<String> {
    value
        .get_array("artists")
        .map(|items| {
            items
                .iter()
                .filter_map(|artist| artist.get_str("name").map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_user_profile(value: &JsonValue) -> Result<UserProfile, ApiError> {
    let id = value
        .get_str("id")
        .ok_or_else(|| ApiError::Parse(String::from("user profile missing `id`")))?
        .to_owned();
    let display_name = value
        .get_str("display_name")
        .filter(|s| !s.is_empty())
        .unwrap_or(&id)
        .to_owned();
    let product = value.get_str("product").unwrap_or("free").to_owned();
    let avatar = parse_first_image(value);
    Ok(UserProfile {
        id,
        display_name,
        product,
        avatar,
    })
}

fn parse_device(value: &JsonValue) -> Option<ConnectDevice> {
    let id = value.get_str("id")?.to_owned();
    let name = value.get_str("name").unwrap_or("Spotify Device").to_owned();
    let device_type = value.get_str("type").unwrap_or("Computer").to_owned();
    let is_active = value.get_bool("is_active").unwrap_or(false);
    let volume_percent = value.get_u8("volume_percent").unwrap_or(50).min(100);
    Some(ConnectDevice {
        id,
        name,
        device_type,
        is_active,
        volume_percent,
    })
}

fn parse_track(value: &JsonValue) -> Option<TrackSummary> {
    parse_track_with_album_fallback(value, "Unknown Album")
}

fn parse_track_with_album_fallback(
    value: &JsonValue,
    album_fallback: &str,
) -> Option<TrackSummary> {
    if value.is_null() {
        return None;
    }
    let id = value.get_str("id")?.to_owned();
    let title = value.get_str("name")?.to_owned();
    let artists = parse_artist_names(value);
    let album_val = value.get("album");
    let album = album_val
        .and_then(|a| a.get_str("name"))
        .unwrap_or(album_fallback)
        .to_owned();
    let cover = album_val.and_then(parse_first_image);
    let duration_ms = value.get_u64("duration_ms").unwrap_or(0);
    let explicit = value.get_bool("explicit").unwrap_or(false);
    Some(TrackSummary {
        id,
        title,
        artists,
        album,
        duration_ms,
        explicit,
        is_liked: false,
        cover,
    })
}

fn parse_album(value: &JsonValue) -> Option<AlbumSummary> {
    if value.is_null() {
        return None;
    }
    let id = value.get_str("id")?.to_owned();
    let title = value.get_str("name")?.to_owned();
    let artists = parse_artist_names(value);
    let release_date = value.get_str("release_date").unwrap_or("");
    let release_year = release_date.chars().take(4).collect();
    let album_type = value.get_str("album_type").unwrap_or("album").to_owned();
    let total_tracks = value.get_u32("total_tracks").unwrap_or(0);
    let cover = parse_first_image(value);
    Some(AlbumSummary {
        id,
        title,
        artists,
        release_year,
        album_type,
        total_tracks,
        cover,
    })
}

fn parse_artist(value: &JsonValue) -> Option<ArtistSummary> {
    if value.is_null() {
        return None;
    }
    let id = value.get_str("id")?.to_owned();
    let name = value.get_str("name")?.to_owned();
    let followers = value
        .get_path(&["followers", "total"])
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    let genres = value
        .get_array("genres")
        .map(|items| {
            items
                .iter()
                .filter_map(|g| g.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let avatar = parse_first_image(value);
    Some(ArtistSummary {
        id,
        name,
        followers,
        genres,
        following: false,
        avatar,
    })
}

fn parse_playlist(value: &JsonValue) -> Option<PlaylistSummary> {
    if value.is_null() {
        return None;
    }
    let id = value.get_str("id")?.to_owned();
    let name = value.get_str("name")?.to_owned();
    let description = value.get_str("description").unwrap_or("").to_owned();
    let owner = value
        .get_path(&["owner", "display_name"])
        .and_then(JsonValue::as_str)
        .or_else(|| value.get_path(&["owner", "id"]).and_then(JsonValue::as_str))
        .unwrap_or("Spotify")
        .to_owned();
    let track_count = value
        .get_path(&["tracks", "total"])
        .and_then(JsonValue::as_u32)
        .unwrap_or(0);
    let snapshot_id = value.get_str("snapshot_id").unwrap_or("").to_owned();
    let cover = parse_first_image(value);
    Some(PlaylistSummary {
        id,
        name,
        description,
        owner,
        track_count,
        snapshot_id,
        pinned: false,
        folder: None,
        cover,
    })
}

fn parse_episode(value: &JsonValue) -> Option<EpisodeSummary> {
    if value.is_null() {
        return None;
    }
    let id = value.get_str("id")?.to_owned();
    let title = value.get_str("name")?.to_owned();
    let show_name = value
        .get_path(&["show", "name"])
        .and_then(JsonValue::as_str)
        .unwrap_or("Podcast")
        .to_owned();
    let duration_ms = value.get_u64("duration_ms").unwrap_or(0);
    let resume_position_ms = value
        .get_path(&["resume_point", "resume_position_ms"])
        .and_then(JsonValue::as_u64)
        .unwrap_or(0);
    let fully_played = value
        .get_path(&["resume_point", "fully_played"])
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let cover = parse_first_image(value);
    Some(EpisodeSummary {
        id,
        title,
        show_name,
        duration_ms,
        resume_position_ms,
        fully_played,
        cover,
    })
}

fn parse_playback_state(value: &JsonValue) -> Result<PlaybackState, ApiError> {
    let is_playing = value.get_bool("is_playing").unwrap_or(false);
    let position_ms = value.get_u64("progress_ms").unwrap_or(0);
    let shuffle = value.get_bool("shuffle_state").unwrap_or(false);
    let repeat = match value.get_str("repeat_state").unwrap_or("off") {
        "context" => RepeatMode::Context,
        "track" => RepeatMode::Track,
        _ => RepeatMode::Off,
    };
    let active_device = value.get("device").and_then(parse_device);
    let volume_percent = active_device.as_ref().map_or(50, |d| d.volume_percent);
    let current_track = value.get("item").and_then(parse_track);

    if value.as_object().is_none() {
        return Err(ApiError::Parse(String::from(
            "playback state payload must be a JSON object",
        )));
    }

    Ok(PlaybackState {
        current_track,
        is_playing,
        position_ms,
        shuffle,
        repeat,
        volume_percent,
        muted: volume_percent == 0,
        speed_percent: 100,
        active_device,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use vynl_core::model::RepeatMode;

    use super::{
        ApiError, HttpMethod, HttpResponse, MockSpotifyTransport, PlaylistTracksOutcome,
        SpotifyApiClient, parse_raw_http_response,
    };

    #[test]
    fn parses_curl_http_responses_including_intermediate_100_continue() -> Result<(), String> {
        let raw = "HTTP/1.1 100 Continue\r\n\r\nHTTP/2 200\r\ncontent-type: application/json\r\netag: \"snap-1\"\r\n\r\n{\"ok\":true}";
        let parsed = parse_raw_http_response(raw).map_err(|e| e.to_string())?;
        assert_eq!(parsed.status, 200);
        assert_eq!(parsed.header("ETag"), Some("\"snap-1\""));
        assert_eq!(parsed.body, "{\"ok\":true}");
        Ok(())
    }

    #[test]
    fn short_circuits_playlist_tracks_on_304_and_matching_snapshot_id() -> Result<(), String> {
        let mock = MockSpotifyTransport::new();
        mock.push_response(HttpResponse::new(304, ""));
        mock.push_json(200, r#"{"snapshot_id":"snap-abc","tracks":{"items":[]}}"#);
        mock.push_response(
            HttpResponse::new(
                200,
                r#"{"snapshot_id":"snap-def","tracks":{"items":[{"track":{"id":"t1","name":"Instant Crush","duration_ms":337000,"explicit":false,"artists":[{"name":"Daft Punk"}],"album":{"name":"Random Access Memories","images":[{"url":"https://i.scdn.co/ram","width":640,"height":640}]}}}]}}"#,
            )
            .with_header("ETag", "\"snap-def\""),
        );

        let client = SpotifyApiClient::new(mock, "tok-1").with_retry_sleep(false);

        let first = client
            .playlist_tracks("pl_1", Some("snap-abc"))
            .map_err(|e| e.to_string())?;
        assert_eq!(
            first,
            PlaylistTracksOutcome::NotModified {
                snapshot_id: String::from("snap-abc")
            }
        );

        let second = client
            .playlist_tracks("pl_1", Some("snap-abc"))
            .map_err(|e| e.to_string())?;
        assert_eq!(
            second,
            PlaylistTracksOutcome::NotModified {
                snapshot_id: String::from("snap-abc")
            }
        );

        let third = client
            .playlist_tracks("pl_1", Some("snap-abc"))
            .map_err(|e| e.to_string())?;
        match third {
            PlaylistTracksOutcome::Updated {
                snapshot_id,
                etag,
                tracks,
            } => {
                assert_eq!(snapshot_id, "snap-def");
                assert_eq!(etag.as_deref(), Some("\"snap-def\""));
                assert_eq!(tracks.len(), 1);
                assert_eq!(tracks[0].title, "Instant Crush");
            }
            PlaylistTracksOutcome::NotModified { .. } => {
                return Err(String::from("expected Updated playlist outcome"));
            }
        }

        let requests = client.transport().recorded_requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].header("If-None-Match"), Some("\"snap-abc\""));
        assert_eq!(requests[0].header("Authorization"), Some("Bearer tok-1"));
        Ok(())
    }

    #[test]
    fn handles_http_429_retry_after_and_exhaustion() -> Result<(), String> {
        let mock = MockSpotifyTransport::new();
        mock.push_response(
            HttpResponse::new(429, r#"{"error":{"message":"slow down"}}"#)
                .with_header("Retry-After", "5"),
        );
        mock.push_json(
            200,
            r#"{"id":"u1","display_name":"Alex","product":"premium"}"#,
        );

        let client = SpotifyApiClient::new(mock, "tok-2")
            .with_max_retries(1)
            .with_retry_sleep(false);
        let profile = client.me().map_err(|e| e.to_string())?;
        assert_eq!(profile.display_name, "Alex");
        assert_eq!(client.transport().recorded_requests().len(), 2);

        // Exhaust retries and verify Retry-After delay propagation
        let mock_exhausted = MockSpotifyTransport::new();
        mock_exhausted.push_response(
            HttpResponse::new(429, r#"{"error":{"message":"rate limit hit"}}"#)
                .with_header("Retry-After", "12"),
        );
        let client_exhausted = SpotifyApiClient::new(mock_exhausted, "tok-2")
            .with_max_retries(0)
            .with_retry_sleep(false);
        let err = client_exhausted.me();
        assert_eq!(
            err,
            Err(ApiError::RateLimited {
                retry_after: Duration::from_secs(12),
                message: String::from("rate limit hit"),
            })
        );
        Ok(())
    }

    #[test]
    fn degrades_gracefully_on_http_403_restricted_endpoint() {
        let mock = MockSpotifyTransport::new();
        mock.push_json(
            403,
            r#"{"error":{"status":403,"message":"Premium required for playback control"}}"#,
        );

        let client = SpotifyApiClient::new(mock, "tok-free").with_retry_sleep(false);
        let err = client.play();
        match err {
            Err(api_err @ ApiError::RestrictedEndpoint { .. }) => {
                assert!(api_err.is_gracefully_degradable());
            }
            other => {
                assert_eq!(other, Err(ApiError::Unauthorized));
            }
        }
    }

    #[test]
    fn issues_expected_playback_and_library_endpoints() -> Result<(), String> {
        let mock = MockSpotifyTransport::new();
        // 1: playback_state (204), 2: play, 3: pause, 4: next, 5: prev, 6: seek,
        // 7: set_volume, 8: set_shuffle, 9: set_repeat, 10: transfer, 11: toggle_like(true),
        // 12: toggle_like(false), 13: queue_track, 14: search
        mock.push_response(HttpResponse::new(204, ""));
        for _ in 0..12 {
            mock.push_response(HttpResponse::new(204, ""));
        }
        mock.push_json(
            200,
            r#"{"tracks":{"items":[{"id":"t9","name":"Time","duration_ms":413000,"explicit":false,"artists":[{"name":"Pink Floyd"}],"album":{"name":"The Dark Side of the Moon","images":[]}}]}}"#,
        );

        let client = SpotifyApiClient::new(mock, "tok-3").with_retry_sleep(false);
        assert_eq!(client.playback_state().map_err(|e| e.to_string())?, None);
        client.play().map_err(|e| e.to_string())?;
        client.pause().map_err(|e| e.to_string())?;
        client.next_track().map_err(|e| e.to_string())?;
        client.prev_track().map_err(|e| e.to_string())?;
        client.seek(42_000).map_err(|e| e.to_string())?;
        client.set_volume(140).map_err(|e| e.to_string())?;
        client.set_shuffle(true).map_err(|e| e.to_string())?;
        client
            .set_repeat(RepeatMode::Track)
            .map_err(|e| e.to_string())?;
        client
            .transfer_playback("dev-1")
            .map_err(|e| e.to_string())?;
        client.toggle_like("t1", true).map_err(|e| e.to_string())?;
        client.toggle_like("t1", false).map_err(|e| e.to_string())?;
        client
            .queue_track("spotify:track:t1")
            .map_err(|e| e.to_string())?;
        let search = client.search("Pink Floyd").map_err(|e| e.to_string())?;
        assert_eq!(search.tracks.len(), 1);
        assert_eq!(search.tracks[0].title, "Time");

        let reqs = client.transport().recorded_requests();
        assert_eq!(reqs.len(), 14);
        assert_eq!(
            reqs[6].url,
            "https://api.spotify.com/v1/me/player/volume?volume_percent=100"
        );
        assert_eq!(
            reqs[8].url,
            "https://api.spotify.com/v1/me/player/repeat?state=track"
        );
        assert_eq!(reqs[10].method, HttpMethod::Put);
        assert_eq!(reqs[11].method, HttpMethod::Delete);
        assert_eq!(
            reqs[12].url,
            "https://api.spotify.com/v1/me/player/queue?uri=spotify%3Atrack%3At1"
        );
        Ok(())
    }

    #[test]
    fn parses_playback_devices_library_recent_and_oauth_tokens() -> Result<(), String> {
        let mock = MockSpotifyTransport::new();
        mock.push_json(
            200,
            r#"{"is_playing":true,"progress_ms":12345,"shuffle_state":true,"repeat_state":"context","device":{"id":"d1","name":"vynl","type":"Computer","is_active":true,"volume_percent":80},"item":{"id":"t1","name":"So What","duration_ms":562000,"explicit":false,"artists":[{"name":"Miles Davis"}],"album":{"name":"Kind of Blue","images":[{"url":"https://i.scdn.co/kob","width":640,"height":640}]}}}"#,
        );
        mock.push_json(
            200,
            r#"{"devices":[{"id":"d1","name":"vynl","type":"Computer","is_active":true,"volume_percent":80}]}"#,
        );
        mock.push_json(
            200,
            r#"{"items":[{"track":{"id":"t2","name":"Dreams","duration_ms":257800,"explicit":false,"artists":[{"name":"Fleetwood Mac"}],"album":{"name":"Rumours","images":[]}}}]}"#,
        );
        mock.push_json(
            200,
            r#"{"items":[{"id":"pl1","name":"Focus","description":"Deep work","owner":{"display_name":"Alex"},"tracks":{"total":12},"snapshot_id":"s1","images":[]}]}"#,
        );
        mock.push_json(
            200,
            r#"{"items":[{"track":{"id":"t3","name":"Reckoner","duration_ms":290213,"explicit":false,"artists":[{"name":"Radiohead"}],"album":{"name":"In Rainbows","images":[]}}}]}"#,
        );
        mock.push_json(
            200,
            r#"{"access_token":"new-acc","refresh_token":"new-ref","expires_in":3600}"#,
        );
        mock.push_json(200, r#"{"access_token":"refreshed-acc","expires_in":3600}"#);
        mock.push_json(401, r#"{"error":{"message":"expired"}}"#);

        let mut client = SpotifyApiClient::new(mock, "tok-init").with_retry_sleep(false);
        let pb = client
            .playback_state()
            .map_err(|e| e.to_string())?
            .ok_or_else(|| String::from("expected playback state"))?;
        assert!(pb.is_playing);
        assert_eq!(pb.position_ms, 12_345);
        assert_eq!(pb.repeat, RepeatMode::Context);
        assert_eq!(
            pb.current_track.as_ref().map(|t| t.title.as_str()),
            Some("So What")
        );

        let devices = client.devices().map_err(|e| e.to_string())?;
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].name, "vynl");

        let liked = client.liked_tracks().map_err(|e| e.to_string())?;
        assert_eq!(liked.len(), 1);
        assert!(liked[0].is_liked);

        let playlists = client.playlists().map_err(|e| e.to_string())?;
        assert_eq!(playlists.len(), 1);
        assert_eq!(playlists[0].snapshot_id, "s1");

        let recent = client.recently_played().map_err(|e| e.to_string())?;
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].title, "Reckoner");

        let bundle = client
            .exchange_pkce_code(
                "cid",
                "code1",
                "http://127.0.0.1:8888/callback",
                "ver",
                1_000,
            )
            .map_err(|e| e.to_string())?;
        assert_eq!(bundle.access_token, "new-acc");
        assert_eq!(bundle.expires_at_epoch_secs, 4_600);

        let refreshed = client
            .refresh_access_token("cid", &bundle.refresh_token, 5_000)
            .map_err(|e| e.to_string())?;
        assert_eq!(refreshed.access_token, "refreshed-acc");
        assert_eq!(refreshed.refresh_token, "new-ref");

        client.set_access_token(refreshed.access_token);
        assert_eq!(client.me(), Err(ApiError::Unauthorized));
        Ok(())
    }

    #[test]
    fn play_uris_sends_put_with_json_uris_payload() -> Result<(), String> {
        let mock = MockSpotifyTransport::new();
        mock.push_response(HttpResponse::new(204, ""));

        let client = SpotifyApiClient::new(mock, "tok-play").with_retry_sleep(false);
        client
            .play_uris(&["spotify:track:trk_01", "spotify:track:trk_02"])
            .map_err(|e| e.to_string())?;

        let reqs = client.transport().recorded_requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, HttpMethod::Put);
        assert_eq!(reqs[0].url, "https://api.spotify.com/v1/me/player/play");
        assert_eq!(reqs[0].header("Content-Type"), Some("application/json"));
        assert_eq!(reqs[0].header("Authorization"), Some("Bearer tok-play"));
        assert_eq!(
            reqs[0].body.as_deref(),
            Some(r#"{"uris":["spotify:track:trk_01","spotify:track:trk_02"]}"#)
        );
        Ok(())
    }

    #[test]
    fn reqwest_transport_builds_and_round_trips_local_http() -> Result<(), String> {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        use super::ReqwestHttpTransport;

        let default_transport = ReqwestHttpTransport::new().map_err(|e| e.to_string())?;
        assert_eq!(default_transport.timeout(), Duration::from_secs(15));

        let custom_transport = ReqwestHttpTransport::with_timeout(Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        assert_eq!(custom_transport.timeout(), Duration::from_secs(5));

        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();

        let server = thread::spawn(move || -> Result<String, String> {
            let (mut stream, _) = listener.accept().map_err(|e| e.to_string())?;
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .map_err(|e| e.to_string())?;
            let mut buf = [0_u8; 4096];
            let n = stream.read(&mut buf).map_err(|e| e.to_string())?;
            let captured = String::from_utf8_lossy(&buf[..n]).into_owned();
            let body = r#"{"id":"u_req","display_name":"Reqwest User","product":"premium"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Test-Header: vynl\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(resp.as_bytes())
                .map_err(|e| e.to_string())?;
            stream.flush().map_err(|e| e.to_string())?;
            Ok(captured)
        });

        let client = SpotifyApiClient::new_reqwest("tok-req")
            .map_err(|e| e.to_string())?
            .with_base_url(format!("http://127.0.0.1:{port}"))
            .with_retry_sleep(false);
        let profile = client.me().map_err(|e| e.to_string())?;
        assert_eq!(profile.id, "u_req");
        assert_eq!(profile.display_name, "Reqwest User");

        let captured = server
            .join()
            .map_err(|_| String::from("local HTTP test server panicked"))??;
        assert!(captured.starts_with("GET /me HTTP/1.1"));
        assert!(
            captured
                .to_ascii_lowercase()
                .contains("authorization: bearer tok-req")
        );
        Ok(())
    }

    #[test]
    fn parses_saved_albums_and_album_tracks() -> Result<(), String> {
        let mock = MockSpotifyTransport::new();
        mock.push_json(
            200,
            r#"{"items":[{"added_at":"2026-01-01T00:00:00Z","album":{"id":"alb_1","name":"Random Access Memories","artists":[{"name":"Daft Punk"}],"release_date":"2013-05-17","album_type":"album","total_tracks":13,"images":[{"url":"https://i.scdn.co/ram","width":640,"height":640}]}}]}"#,
        );
        mock.push_json(
            200,
            r#"{"items":[{"id":"t_1","name":"Give Life Back to Music","artists":[{"name":"Daft Punk"}],"duration_ms":274000,"explicit":false},{"id":"t_2","name":"Giorgio by Moroder","artists":[{"name":"Daft Punk"}],"duration_ms":544000,"explicit":true}]}"#,
        );

        let client = SpotifyApiClient::new(mock, "tok-lib").with_retry_sleep(false);

        let albums = client.saved_albums().map_err(|e| e.to_string())?;
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].id, "alb_1");
        assert_eq!(albums[0].title, "Random Access Memories");
        assert_eq!(albums[0].artists, vec![String::from("Daft Punk")]);
        assert_eq!(albums[0].release_year, "2013");
        assert_eq!(albums[0].album_type, "album");
        assert_eq!(albums[0].total_tracks, 13);
        assert_eq!(
            albums[0].cover.as_ref().map(|c| c.url.as_str()),
            Some("https://i.scdn.co/ram")
        );

        let tracks = client.album_tracks("alb_1").map_err(|e| e.to_string())?;
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].id, "t_1");
        assert_eq!(tracks[0].title, "Give Life Back to Music");
        assert_eq!(tracks[0].album, "");
        assert_eq!(tracks[0].cover, None);
        assert!(!tracks[0].explicit);
        assert_eq!(tracks[1].id, "t_2");
        assert_eq!(tracks[1].duration_ms, 544_000);
        assert!(tracks[1].explicit);

        let reqs = client.transport().recorded_requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].method, HttpMethod::Get);
        assert_eq!(reqs[0].url, "https://api.spotify.com/v1/me/albums?limit=50");
        assert_eq!(reqs[0].header("Authorization"), Some("Bearer tok-lib"));
        assert_eq!(reqs[1].method, HttpMethod::Get);
        assert_eq!(
            reqs[1].url,
            "https://api.spotify.com/v1/albums/alb_1/tracks?limit=50"
        );
        Ok(())
    }

    #[test]
    fn parses_followed_artists_artist_top_tracks_and_saved_episodes() -> Result<(), String> {
        let mock = MockSpotifyTransport::new();
        mock.push_json(
            200,
            r#"{"artists":{"items":[{"id":"art_1","name":"Daft Punk","followers":{"total":10500000},"genres":["french house","electronic"],"images":[{"url":"https://i.scdn.co/dp","width":640,"height":640}]}]}}"#,
        );
        mock.push_json(
            200,
            r#"{"tracks":[{"id":"t_top","name":"Get Lucky","artists":[{"name":"Daft Punk"},{"name":"Pharrell Williams"}],"album":{"name":"Random Access Memories","images":[{"url":"https://i.scdn.co/ram","width":640,"height":640}]},"duration_ms":369000,"explicit":false}]}"#,
        );
        mock.push_json(
            200,
            r#"{"items":[{"added_at":"2026-02-01T00:00:00Z","episode":{"id":"ep_1","name":"Rust Async Deep Dive","show":{"name":"Rustacean Station"},"duration_ms":3600000,"resume_point":{"resume_position_ms":420000,"fully_played":false},"images":[{"url":"https://i.scdn.co/rs","width":300,"height":300}]}}]}"#,
        );

        let client = SpotifyApiClient::new(mock, "tok-art").with_retry_sleep(false);

        let artists = client.followed_artists().map_err(|e| e.to_string())?;
        assert_eq!(artists.len(), 1);
        assert_eq!(artists[0].id, "art_1");
        assert_eq!(artists[0].name, "Daft Punk");
        assert_eq!(artists[0].followers, 10_500_000);
        assert_eq!(
            artists[0].genres,
            vec![String::from("french house"), String::from("electronic")]
        );
        assert!(artists[0].following);
        assert_eq!(
            artists[0].avatar.as_ref().map(|a| a.url.as_str()),
            Some("https://i.scdn.co/dp")
        );

        let top_tracks = client
            .artist_top_tracks("art_1")
            .map_err(|e| e.to_string())?;
        assert_eq!(top_tracks.len(), 1);
        assert_eq!(top_tracks[0].id, "t_top");
        assert_eq!(top_tracks[0].title, "Get Lucky");
        assert_eq!(
            top_tracks[0].artists,
            vec![String::from("Daft Punk"), String::from("Pharrell Williams")]
        );
        assert_eq!(top_tracks[0].album, "Random Access Memories");
        assert_eq!(top_tracks[0].duration_ms, 369_000);

        let episodes = client.saved_episodes().map_err(|e| e.to_string())?;
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].id, "ep_1");
        assert_eq!(episodes[0].title, "Rust Async Deep Dive");
        assert_eq!(episodes[0].show_name, "Rustacean Station");
        assert_eq!(episodes[0].duration_ms, 3_600_000);
        assert_eq!(episodes[0].resume_position_ms, 420_000);
        assert!(!episodes[0].fully_played);
        assert_eq!(
            episodes[0].cover.as_ref().map(|c| c.url.as_str()),
            Some("https://i.scdn.co/rs")
        );

        let reqs = client.transport().recorded_requests();
        assert_eq!(reqs.len(), 3);
        assert_eq!(reqs[0].method, HttpMethod::Get);
        assert_eq!(
            reqs[0].url,
            "https://api.spotify.com/v1/me/following?type=artist&limit=50"
        );
        assert_eq!(reqs[1].method, HttpMethod::Get);
        assert_eq!(
            reqs[1].url,
            "https://api.spotify.com/v1/artists/art_1/top-tracks"
        );
        assert_eq!(reqs[2].method, HttpMethod::Get);
        assert_eq!(
            reqs[2].url,
            "https://api.spotify.com/v1/me/episodes?limit=50"
        );
        Ok(())
    }

    #[test]
    fn add_tracks_to_playlist_sends_post_with_json_uris() -> Result<(), String> {
        let mock = MockSpotifyTransport::new();
        mock.push_json(201, r#"{"snapshot_id":"snap-2"}"#);

        let client = SpotifyApiClient::new(mock, "tok-pl").with_retry_sleep(false);
        client
            .add_tracks_to_playlist("pl_42", &["spotify:track:t1", "spotify:track:t2"])
            .map_err(|e| e.to_string())?;

        let reqs = client.transport().recorded_requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, HttpMethod::Post);
        assert_eq!(
            reqs[0].url,
            "https://api.spotify.com/v1/playlists/pl_42/tracks"
        );
        assert_eq!(reqs[0].header("Content-Type"), Some("application/json"));
        assert_eq!(reqs[0].header("Authorization"), Some("Bearer tok-pl"));
        assert_eq!(
            reqs[0].body.as_deref(),
            Some(r#"{"uris":["spotify:track:t1","spotify:track:t2"]}"#)
        );
        Ok(())
    }

    #[test]
    fn phase3_endpoints_degrade_gracefully_on_http_403() {
        let mock = MockSpotifyTransport::new();
        for _ in 0..6 {
            mock.push_json(
                403,
                r#"{"error":{"status":403,"message":"Insufficient scope or restricted endpoint"}}"#,
            );
        }

        let client = SpotifyApiClient::new(mock, "tok-limited").with_retry_sleep(false);

        let checks: Vec<(Result<(), ApiError>, &str)> = vec![
            (client.saved_albums().map(|_| ()), "/me/albums?limit=50"),
            (
                client.album_tracks("alb_1").map(|_| ()),
                "/albums/alb_1/tracks?limit=50",
            ),
            (
                client.followed_artists().map(|_| ()),
                "/me/following?type=artist&limit=50",
            ),
            (
                client.artist_top_tracks("art_1").map(|_| ()),
                "/artists/art_1/top-tracks",
            ),
            (client.saved_episodes().map(|_| ()), "/me/episodes?limit=50"),
            (
                client.add_tracks_to_playlist("pl_1", &["spotify:track:t1"]),
                "/playlists/pl_1/tracks",
            ),
        ];

        for (result, expected_endpoint) in checks {
            assert_eq!(
                result,
                Err(ApiError::RestrictedEndpoint {
                    endpoint: String::from(expected_endpoint),
                    message: String::from("Insufficient scope or restricted endpoint"),
                })
            );
            if let Err(err) = result {
                assert!(err.is_gracefully_degradable());
            }
        }
    }
}
