//! Pure-Rust OAuth 2.0 PKCE (`S256`) cryptography, Spotify authorization URL
//! builder, local `127.0.0.1` HTTP callback server, and `0600` token store.

use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::REQUIRED_SCOPES;
use crate::json::{JsonValue, parse_json, to_json_string};

/// Official Spotify OAuth 2.0 authorization endpoint.
pub const SPOTIFY_AUTHORIZE_URL: &str = "https://accounts.spotify.com/authorize";

/// Official Spotify OAuth 2.0 token exchange and refresh endpoint.
pub const SPOTIFY_TOKEN_URL: &str = "https://accounts.spotify.com/api/token";

/// Standard FIPS 180-4 SHA-256 initial hash values.
const SHA256_H0: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// Standard FIPS 180-4 SHA-256 round constants.
const SHA256_K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// URL-safe Base64 alphabet (RFC 4648 Section 5, without padding).
const BASE64URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

static ENTROPY_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Computes the FIPS 180-4 `SHA-256` digest of `data` in pure Rust.
#[must_use]
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut state = SHA256_H0;

    let full_blocks = data.len() / 64;
    for block_idx in 0..full_blocks {
        let start = block_idx * 64;
        let mut block = [0_u8; 64];
        block.copy_from_slice(&data[start..start + 64]);
        sha256_compress(&mut state, &block);
    }

    let rem = &data[full_blocks * 64..];
    let mut pad = [0_u8; 128];
    pad[..rem.len()].copy_from_slice(rem);
    pad[rem.len()] = 0x80;

    let pad_len = if rem.len() < 56 { 64 } else { 128 };
    let bit_len = (data.len() as u64).wrapping_mul(8);
    pad[pad_len - 8..pad_len].copy_from_slice(&bit_len.to_be_bytes());

    let mut first = [0_u8; 64];
    first.copy_from_slice(&pad[..64]);
    sha256_compress(&mut state, &first);

    if pad_len == 128 {
        let mut second = [0_u8; 64];
        second.copy_from_slice(&pad[64..128]);
        sha256_compress(&mut state, &second);
    }

    let mut out = [0_u8; 32];
    for (idx, word) in state.iter().enumerate() {
        out[idx * 4..idx * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn sha256_compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut schedule = [0_u32; 64];
    for (idx, chunk) in block.as_chunks::<4>().0.iter().enumerate() {
        schedule[idx] = u32::from_be_bytes(*chunk);
    }
    for i in 16..64 {
        let s0 = schedule[i - 15].rotate_right(7)
            ^ schedule[i - 15].rotate_right(18)
            ^ (schedule[i - 15] >> 3);
        let s1 = schedule[i - 2].rotate_right(17)
            ^ schedule[i - 2].rotate_right(19)
            ^ (schedule[i - 2] >> 10);
        schedule[i] = schedule[i - 16]
            .wrapping_add(s0)
            .wrapping_add(schedule[i - 7])
            .wrapping_add(s1);
    }

    let [
        mut va,
        mut vb,
        mut vc,
        mut vd,
        mut ve,
        mut vf,
        mut vg,
        mut vh,
    ] = *state;

    for i in 0..64 {
        let s1 = ve.rotate_right(6) ^ ve.rotate_right(11) ^ ve.rotate_right(25);
        let ch = (ve & vf) ^ ((!ve) & vg);
        let temp1 = vh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(SHA256_K[i])
            .wrapping_add(schedule[i]);
        let s0 = va.rotate_right(2) ^ va.rotate_right(13) ^ va.rotate_right(22);
        let maj = (va & vb) ^ (va & vc) ^ (vb & vc);
        let temp2 = s0.wrapping_add(maj);

        vh = vg;
        vg = vf;
        vf = ve;
        ve = vd.wrapping_add(temp1);
        vd = vc;
        vc = vb;
        vb = va;
        va = temp1.wrapping_add(temp2);
    }

    state[0] = state[0].wrapping_add(va);
    state[1] = state[1].wrapping_add(vb);
    state[2] = state[2].wrapping_add(vc);
    state[3] = state[3].wrapping_add(vd);
    state[4] = state[4].wrapping_add(ve);
    state[5] = state[5].wrapping_add(vf);
    state[6] = state[6].wrapping_add(vg);
    state[7] = state[7].wrapping_add(vh);
}

/// Computes the lowercase hexadecimal `SHA-256` digest of `data`.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = sha256(data);
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Encodes `bytes` using URL-safe Base64 (RFC 4648 Section 5) without `=` padding.
#[must_use]
pub fn base64url_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() * 4).div_ceil(3));
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(char::from(
            BASE64URL_ALPHABET[((triple >> 18) & 0x3F) as usize],
        ));
        out.push(char::from(
            BASE64URL_ALPHABET[((triple >> 12) & 0x3F) as usize],
        ));
        if chunk.len() > 1 {
            out.push(char::from(
                BASE64URL_ALPHABET[((triple >> 6) & 0x3F) as usize],
            ));
        }
        if chunk.len() > 2 {
            out.push(char::from(BASE64URL_ALPHABET[(triple & 0x3F) as usize]));
        }
    }
    out
}

/// Computes an RFC 7636 `S256` PKCE code challenge from `verifier`.
#[must_use]
pub fn compute_s256_challenge(verifier: &str) -> String {
    base64url_encode(&sha256(verifier.as_bytes()))
}

/// RFC 3986 percent-encoding for URL query parameter values.
#[must_use]
pub fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Decodes a percent-encoded URL query component (treating `+` as space).
#[must_use]
pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        match bytes[idx] {
            b'+' => {
                out.push(b' ');
                idx += 1;
            }
            b'%' if idx + 2 < bytes.len() => {
                if let (Some(hi), Some(lo)) =
                    (hex_nibble(bytes[idx + 1]), hex_nibble(bytes[idx + 2]))
                {
                    out.push((hi << 4) | lo);
                    idx += 3;
                } else {
                    out.push(b'%');
                    idx += 1;
                }
            }
            other => {
                out.push(other);
                idx += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Generated OAuth 2.0 PKCE verifier, `S256` challenge, and CSRF `state` token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkceChallenge {
    /// High-entropy PKCE `code_verifier` (RFC 7636).
    pub verifier: String,
    /// `BASE64URL(SHA256(verifier))` sent to `/authorize`.
    pub challenge: String,
    /// Opaque CSRF protection `state` parameter validated on callback.
    pub state: String,
}

impl PkceChallenge {
    /// Generates a fresh cryptographic PKCE verifier, `S256` challenge, and CSRF state.
    ///
    /// # Errors
    /// Returns an error if OS entropy collection fails unexpectedly.
    pub fn generate() -> Result<Self, String> {
        let verifier_bytes = random_bytes::<48>()?;
        let state_bytes = random_bytes::<24>()?;
        let verifier = base64url_encode(&verifier_bytes);
        let state = base64url_encode(&state_bytes);
        Ok(Self::from_verifier_and_state(verifier, state))
    }

    /// Constructs a [`PkceChallenge`] deterministically from explicit `verifier` and `state`.
    #[must_use]
    pub fn from_verifier_and_state(verifier: impl Into<String>, state: impl Into<String>) -> Self {
        let verifier = verifier.into();
        let challenge = compute_s256_challenge(&verifier);
        Self {
            verifier,
            challenge,
            state: state.into(),
        }
    }
}

fn random_bytes<const N: usize>() -> Result<[u8; N], String> {
    #[cfg(unix)]
    {
        let mut buf = [0_u8; N];
        let mut file = fs::File::open("/dev/urandom")
            .map_err(|err| format!("failed to open /dev/urandom: {err}"))?;
        file.read_exact(&mut buf)
            .map_err(|err| format!("failed to read /dev/urandom: {err}"))?;
        Ok(buf)
    }

    #[cfg(not(unix))]
    {
        let mut out = [0_u8; N];
        let mut offset = 0;
        let pid = std::process::id();
        while offset < N {
            let counter = ENTROPY_COUNTER.fetch_add(1, Ordering::Relaxed);
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0_u128, |d| d.as_nanos());
            let mut seed = Vec::with_capacity(32);
            seed.extend_from_slice(&now.to_le_bytes());
            seed.extend_from_slice(&pid.to_le_bytes());
            seed.extend_from_slice(&counter.to_le_bytes());
            let hash = sha256(&seed);
            let take = (N - offset).min(hash.len());
            out[offset..offset + take].copy_from_slice(&hash[..take]);
            offset += take;
        }
        Ok(out)
    }
}

/// Builds the official Spotify OAuth 2.0 PKCE authorization URL.
#[must_use]
pub fn build_authorize_url(
    client_id: &str,
    redirect_uri: &str,
    challenge: &PkceChallenge,
) -> String {
    let scope = REQUIRED_SCOPES.join(" ");
    format!(
        "{SPOTIFY_AUTHORIZE_URL}?client_id={}&response_type=code&redirect_uri={}&code_challenge_method=S256&code_challenge={}&state={}&scope={}",
        percent_encode(client_id.trim()),
        percent_encode(redirect_uri),
        percent_encode(&challenge.challenge),
        percent_encode(&challenge.state),
        percent_encode(&scope),
    )
}

/// Builds the `application/x-www-form-urlencoded` body for PKCE authorization code exchange.
#[must_use]
pub fn build_token_exchange_body(
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> String {
    format!(
        "grant_type=authorization_code&client_id={}&code={}&redirect_uri={}&code_verifier={}",
        percent_encode(client_id.trim()),
        percent_encode(code),
        percent_encode(redirect_uri),
        percent_encode(verifier),
    )
}

/// Builds the `application/x-www-form-urlencoded` body for refreshing an OAuth access token.
#[must_use]
pub fn build_token_refresh_body(client_id: &str, refresh_token: &str) -> String {
    format!(
        "grant_type=refresh_token&client_id={}&refresh_token={}",
        percent_encode(client_id.trim()),
        percent_encode(refresh_token),
    )
}

/// Validated OAuth authorization callback parameters returned by [`CallbackServer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackResult {
    /// One-time authorization code to exchange for an OAuth token bundle.
    pub code: String,
    /// Echoed CSRF state string verified against the PKCE challenge.
    pub state: String,
}

/// Single-use local HTTP server listening on `127.0.0.1` for the Spotify OAuth callback.
#[derive(Debug)]
pub struct CallbackServer {
    listener: TcpListener,
}

impl CallbackServer {
    /// Binds a local `TcpListener` to `127.0.0.1:{port}` (`0` chooses an ephemeral port).
    ///
    /// # Errors
    /// Returns an error if the local TCP port cannot be bound.
    pub fn bind(port: u16) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|err| {
            format!("failed to bind OAuth callback server on 127.0.0.1:{port}: {err}")
        })?;
        listener
            .set_nonblocking(true)
            .map_err(|err| format!("failed to configure callback listener: {err}"))?;
        Ok(Self { listener })
    }

    /// Returns the actual bound TCP port on `127.0.0.1`.
    ///
    /// # Errors
    /// Returns an error if querying the local socket address fails.
    pub fn local_port(&self) -> Result<u16, String> {
        self.listener
            .local_addr()
            .map(|addr| addr.port())
            .map_err(|err| format!("failed to query callback port: {err}"))
    }

    /// Returns the canonical `http://127.0.0.1:<port>/callback` redirect URI.
    ///
    /// # Errors
    /// Returns an error if querying the bound port fails.
    pub fn redirect_uri(&self) -> Result<String, String> {
        let port = self.local_port()?;
        Ok(format!("http://127.0.0.1:{port}/callback"))
    }

    /// Waits up to `timeout` for a single `GET /callback?code=...&state=...` request,
    /// validates `state`, and serves a clean HTML response page.
    ///
    /// # Errors
    /// Returns an error on timeout, malformed HTTP request, Spotify error parameter,
    /// missing `code`, or CSRF `state` mismatch.
    pub fn wait_for_callback(
        &self,
        expected_state: &str,
        timeout: Duration,
    ) -> Result<CallbackResult, String> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.listener.accept() {
                Ok((mut stream, _addr)) => {
                    if let Some(outcome) = handle_callback_connection(&mut stream, expected_state) {
                        return outcome;
                    }
                }
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(String::from("timed out waiting for Spotify OAuth callback"));
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => {
                    return Err(format!("OAuth callback accept error: {err}"));
                }
            }
        }
    }
}

struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn parse_callback_query(query: &str) -> CallbackQuery {
    let mut parsed = CallbackQuery {
        code: None,
        state: None,
        error: None,
        error_description: None,
    };
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let val = percent_decode(v);
        match k {
            "code" => parsed.code = Some(val),
            "state" => parsed.state = Some(val),
            "error" => parsed.error = Some(val),
            "error_description" => parsed.error_description = Some(val),
            _ => {}
        }
    }
    parsed
}

fn handle_callback_connection(
    stream: &mut TcpStream,
    expected_state: &str,
) -> Option<Result<CallbackResult, String>> {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let mut buf = [0_u8; 4096];
    let n = match stream.read(&mut buf) {
        Ok(0) => return None,
        Ok(n) => n,
        Err(err) => return Some(Err(format!("failed reading callback HTTP request: {err}"))),
    };

    let request = String::from_utf8_lossy(&buf[..n]);
    let Some(first_line) = request.lines().next() else {
        write_html_response(
            stream,
            "400 Bad Request",
            "Invalid OAuth Request",
            "Empty HTTP request.",
        );
        return Some(Err(String::from("empty HTTP callback request")));
    };

    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    if method != "GET" {
        write_html_response(
            stream,
            "405 Method Not Allowed",
            "Method Not Allowed",
            "Only GET callbacks are supported.",
        );
        return Some(Err(format!("unexpected callback HTTP method `{method}`")));
    }

    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != "/callback" {
        write_html_response(
            stream,
            "404 Not Found",
            "Not Found",
            "Waiting for /callback.",
        );
        return None;
    }

    let params = parse_callback_query(query);
    if let Some(err_code) = params.error {
        let detail = params.error_description.unwrap_or_else(|| err_code.clone());
        write_html_response(
            stream,
            "400 Bad Request",
            "Spotify Authorization Declined",
            "You may close this tab and return to vynl.",
        );
        return Some(Err(format!(
            "Spotify authorization error `{err_code}`: {detail}"
        )));
    }

    let Some(received_state) = params.state else {
        write_html_response(
            stream,
            "400 Bad Request",
            "Missing State Parameter",
            "OAuth state verification failed. Please retry login from vynl.",
        );
        return Some(Err(String::from(
            "OAuth callback missing `state` parameter",
        )));
    };

    if !constant_time_eq(received_state.as_bytes(), expected_state.as_bytes()) {
        write_html_response(
            stream,
            "400 Bad Request",
            "Invalid State Parameter",
            "OAuth CSRF state mismatch. Please retry login from vynl.",
        );
        return Some(Err(String::from("OAuth callback `state` mismatch")));
    }

    let Some(auth_code) = params.code.filter(|c| !c.is_empty()) else {
        write_html_response(
            stream,
            "400 Bad Request",
            "Missing Authorization Code",
            "Spotify did not return an authorization code.",
        );
        return Some(Err(String::from("OAuth callback missing `code` parameter")));
    };

    write_html_response(
        stream,
        "200 OK",
        "vynl Connected to Spotify",
        "Authentication succeeded. You can close this browser tab and return to your terminal.",
    );

    Some(Ok(CallbackResult {
        code: auth_code,
        state: received_state,
    }))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn write_html_response(stream: &mut TcpStream, status: &str, heading: &str, body_text: &str) {
    let html = format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>{heading}</title>\
         <style>body{{font-family:system-ui,-apple-system,sans-serif;background:#121212;color:#f5f5f5;\
         display:flex;align-items:center;justify-content:center;min-height:100vh;margin:0;}}\
         .card{{background:#181818;border:1px solid #282828;border-radius:12px;padding:32px 40px;\
         max-width:460px;text-align:center;}}h1{{color:#1db954;font-size:1.4rem;margin:0 0 12px;}}\
         p{{color:#b3b3b3;line-height:1.5;margin:0;}}</style></head>\
         <body><div class=\"card\"><h1>{heading}</h1><p>{body_text}</p></div></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// OAuth 2.0 access and refresh tokens with UNIX expiration timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenBundle {
    /// Short-lived Spotify Bearer access token.
    pub access_token: String,
    /// Long-lived OAuth refresh token for silent renewal.
    pub refresh_token: String,
    /// UNIX epoch timestamp (in seconds) when `access_token` expires.
    pub expires_at_epoch_secs: u64,
}

impl TokenBundle {
    /// Returns `true` when `access_token` is expired at `now_epoch_secs`.
    #[must_use]
    pub const fn is_expired(&self, now_epoch_secs: u64) -> bool {
        now_epoch_secs >= self.expires_at_epoch_secs
    }

    /// Returns `true` when `access_token` expires within `leeway_secs` of `now_epoch_secs`.
    #[must_use]
    pub const fn is_expired_with_leeway(&self, now_epoch_secs: u64, leeway_secs: u64) -> bool {
        now_epoch_secs.saturating_add(leeway_secs) >= self.expires_at_epoch_secs
    }

    /// Serializes this token bundle into a [`JsonValue`].
    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        JsonValue::object([
            ("access_token", JsonValue::from(self.access_token.as_str())),
            (
                "refresh_token",
                JsonValue::from(self.refresh_token.as_str()),
            ),
            (
                "expires_at_epoch_secs",
                JsonValue::from(self.expires_at_epoch_secs),
            ),
        ])
    }

    /// Deserializes a persisted [`TokenBundle`] from a [`JsonValue`].
    ///
    /// # Errors
    /// Returns an error if any required field is missing or has an invalid type.
    pub fn from_json(value: &JsonValue) -> Result<Self, String> {
        let access_token = value
            .get_str("access_token")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| String::from("token bundle missing `access_token`"))?
            .to_owned();
        let refresh_token = value
            .get_str("refresh_token")
            .ok_or_else(|| String::from("token bundle missing `refresh_token`"))?
            .to_owned();
        let expires_at_epoch_secs = value
            .get_u64("expires_at_epoch_secs")
            .ok_or_else(|| String::from("token bundle missing `expires_at_epoch_secs`"))?;
        Ok(Self {
            access_token,
            refresh_token,
            expires_at_epoch_secs,
        })
    }

    /// Parses a Spotify `/api/token` JSON response into a [`TokenBundle`].
    ///
    /// # Errors
    /// Returns an error when `access_token` or `expires_in` is absent, or when neither
    /// the response nor `fallback_refresh_token` provides a refresh token.
    pub fn from_token_response(
        value: &JsonValue,
        now_epoch_secs: u64,
        fallback_refresh_token: Option<&str>,
    ) -> Result<Self, String> {
        let access_token = value
            .get_str("access_token")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| String::from("Spotify token response missing `access_token`"))?
            .to_owned();
        let expires_in = value
            .get_u64("expires_in")
            .ok_or_else(|| String::from("Spotify token response missing `expires_in`"))?;
        let refresh_token = value
            .get_str("refresh_token")
            .filter(|s| !s.is_empty())
            .or(fallback_refresh_token)
            .ok_or_else(|| String::from("Spotify token response missing `refresh_token`"))?
            .to_owned();
        Ok(Self {
            access_token,
            refresh_token,
            expires_at_epoch_secs: now_epoch_secs.saturating_add(expires_in),
        })
    }
}

/// Persistent local OAuth token store enforcing owner-only `0600` permissions on Unix.
#[derive(Debug, Clone)]
pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    /// Creates a new token store backed by `path`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the filesystem path backing this store.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Saves `bundle` atomically to disk with `0600` permissions on Unix targets.
    ///
    /// # Errors
    /// Returns an error if parent directories cannot be created, permissions cannot be set,
    /// or atomic write/rename fails.
    pub fn save(&self, bundle: &TokenBundle) -> Result<(), String> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create token directory: {err}"))?;
        }

        let tmp_path = temp_path_for(&self.path);
        let serialized = to_json_string(&bundle.to_json());

        let write_result = write_secret_file(&tmp_path, serialized.as_bytes());
        if let Err(err) = write_result {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }

        if let Err(err) = fs::rename(&tmp_path, &self.path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(format!("failed to atomically replace token file: {err}"));
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))
                .map_err(|err| format!("failed to set 0600 permissions on token file: {err}"))?;
        }

        Ok(())
    }

    /// Loads the persisted [`TokenBundle`], returning `Ok(None)` if no token file exists.
    ///
    /// # Errors
    /// Returns an error if the token file exists but cannot be read or parsed.
    pub fn load(&self) -> Result<Option<TokenBundle>, String> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(format!("failed to read token file: {err}")),
        };
        let parsed = parse_json(&raw)?;
        TokenBundle::from_json(&parsed).map(Some)
    }

    /// Deletes the persisted token file if present.
    ///
    /// # Errors
    /// Returns an error if removal fails for reasons other than file absence.
    pub fn clear(&self) -> Result<(), String> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!("failed to remove token file: {err}")),
        }
    }
}

fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }

    let mut file = opts
        .open(path)
        .map_err(|err| format!("failed to open temp token file: {err}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|err| format!("failed to enforce 0600 permissions: {err}"))?;
    }

    file.write_all(bytes)
        .map_err(|err| format!("failed to write token payload: {err}"))?;
    file.sync_all()
        .map_err(|err| format!("failed to sync token file: {err}"))?;
    Ok(())
}

fn temp_path_for(path: &Path) -> PathBuf {
    let seq = ENTROPY_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("tokens.json");
    path.with_file_name(format!(".{file_name}.tmp.{pid}.{seq}"))
}

/// Returns the current UNIX timestamp in whole seconds.
#[must_use]
pub fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::thread;
    use std::time::Duration;

    use super::{
        CallbackServer, PkceChallenge, TokenBundle, TokenStore, base64url_encode,
        build_authorize_url, compute_s256_challenge, sha256_hex,
    };

    #[test]
    fn matches_nist_sha256_test_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            sha256_hex(
                b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmno\
                  ijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"
            ),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
    }

    #[test]
    fn computes_rfc7636_s256_challenge() {
        // RFC 7636 Appendix B official test vector
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected_challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(compute_s256_challenge(verifier), expected_challenge);
        assert_eq!(base64url_encode(b"f"), "Zg");
        assert_eq!(base64url_encode(b"fo"), "Zm8");
        assert_eq!(base64url_encode(b"foo"), "Zm9v");

        let pkce = PkceChallenge::from_verifier_and_state(verifier, "state-123");
        assert_eq!(pkce.challenge, expected_challenge);
        let url = build_authorize_url("my_client", "http://127.0.0.1:8888/callback", &pkce);
        assert!(url.starts_with("https://accounts.spotify.com/authorize?"));
        assert!(url.contains("client_id=my_client"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(expected_challenge));
        assert!(url.contains("state=state-123"));
    }

    #[test]
    fn generates_high_entropy_pkce_challenge() -> Result<(), String> {
        let a = PkceChallenge::generate()?;
        let b = PkceChallenge::generate()?;
        assert_ne!(a.verifier, b.verifier);
        assert_ne!(a.state, b.state);
        assert!(a.verifier.len() >= 43);
        assert_eq!(a.challenge, compute_s256_challenge(&a.verifier));
        Ok(())
    }

    #[test]
    fn local_callback_server_round_trips_and_validates_state() -> Result<(), String> {
        let server = CallbackServer::bind(0)?;
        let port = server.local_port()?;
        let expected_state = "csrf_state_token_42";

        let handle = thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port))
                .map_err(|err| format!("connect failed: {err}"))?;
            let req = format!(
                "GET /callback?code=auth_code_99&state={expected_state} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"
            );
            stream
                .write_all(req.as_bytes())
                .map_err(|err| format!("write failed: {err}"))?;
            let mut resp = String::new();
            stream
                .read_to_string(&mut resp)
                .map_err(|err| format!("read failed: {err}"))?;
            Ok::<String, String>(resp)
        });

        let result = server.wait_for_callback(expected_state, Duration::from_secs(3))?;
        assert_eq!(result.code, "auth_code_99");
        assert_eq!(result.state, expected_state);

        let response = handle
            .join()
            .map_err(|_| String::from("client thread failed"))??;
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("vynl Connected to Spotify"));
        Ok(())
    }

    #[test]
    fn local_callback_server_rejects_state_mismatch() -> Result<(), String> {
        let server = CallbackServer::bind(0)?;
        let port = server.local_port()?;

        let handle = thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port))
                .map_err(|err| format!("connect failed: {err}"))?;
            stream
                .write_all(
                    b"GET /callback?code=abc&state=wrong_state HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
                )
                .map_err(|err| format!("write failed: {err}"))?;
            let mut resp = String::new();
            let _ = stream.read_to_string(&mut resp);
            Ok::<String, String>(resp)
        });

        let res = server.wait_for_callback("expected_state", Duration::from_secs(3));
        assert!(res.is_err());

        let response = handle
            .join()
            .map_err(|_| String::from("client thread failed"))??;
        assert!(response.starts_with("HTTP/1.1 400 Bad Request"));
        Ok(())
    }

    #[test]
    fn token_store_round_trips_with_0600_permissions() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("vynl-token-test-{}", std::process::id()));
        let path = dir.join("tokens.json");
        let store = TokenStore::new(&path);

        let bundle = TokenBundle {
            access_token: String::from("BQA-access-token"),
            refresh_token: String::from("AQD-refresh-token"),
            expires_at_epoch_secs: 1_900_000_000,
        };
        store.save(&bundle)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }

        let loaded = store.load()?;
        assert_eq!(loaded, Some(bundle));

        store.clear()?;
        assert_eq!(store.load()?, None);
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
