//! Lean, pure-Rust Spotify Web API client, OAuth 2.0 PKCE state machine,
//! RFC 8259 JSON parser, HTTP 429 rate-limit backoff policy, offline demo catalog,
//! and atomic metadata cache contracts.
//!
//! Per ADR-0001 and ADR-0003, this crate communicates exclusively with
//! official Spotify endpoints and persists metadata locally without native C
//! library dependencies.

pub mod api;
pub mod cache;
pub mod demo;
pub mod json;
pub mod pkce;

use std::time::Duration;

/// Required Spotify OAuth 2.0 scopes for desktop feature parity.
pub const REQUIRED_SCOPES: &[&str] = &[
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "user-read-recently-played",
    "user-top-read",
    "user-library-read",
    "user-library-modify",
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-public",
    "playlist-modify-private",
    "ugc-image-upload",
    "user-follow-read",
    "user-follow-modify",
];

/// Current state of the BYO-Client-ID OAuth 2.0 PKCE authentication flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthState {
    /// No Spotify Developer `client_id` configured yet (launch setup wizard).
    NeedsClientId,
    /// `client_id` configured; waiting for browser PKCE callback.
    AwaitingCallback {
        /// Local callback URL bound on `127.0.0.1`.
        redirect_uri: String,
    },
    /// Authenticated with a valid OAuth token (shared with `vynl-player`).
    Authenticated,
}

/// Determines the initial authentication state from user configuration.
#[must_use]
pub fn initial_auth_state(config: &vynl_core::config::SpotifyConfig) -> AuthState {
    match config.client_id.as_deref().map(str::trim) {
        Some(id) if !id.is_empty() => AuthState::AwaitingCallback {
            redirect_uri: String::from("http://127.0.0.1:8888/callback"),
        },
        _ => AuthState::NeedsClientId,
    }
}

/// Computes backoff duration when Spotify returns HTTP `429 Too Many Requests`.
///
/// Always honors `Retry-After` (in seconds) when provided, with a minimum safety
/// floor of 1 second and a cap of 120 seconds.
#[must_use]
pub fn rate_limit_delay(retry_after_secs: Option<u64>, attempt: u32) -> Duration {
    if let Some(secs) = retry_after_secs {
        return Duration::from_secs(secs.clamp(1, 120));
    }
    let exp = 1_u64.checked_shl(attempt.min(6)).unwrap_or(64);
    Duration::from_secs(exp.clamp(1, 60))
}

/// Classifies whether a Spotify API error represents a restricted endpoint (`403`)
/// that should degrade gracefully without surfacing an error modal (ADR-0003).
#[must_use]
pub const fn is_gracefully_degradable_status(http_status: u16) -> bool {
    http_status == 403
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{REQUIRED_SCOPES, is_gracefully_degradable_status, rate_limit_delay};

    #[test]
    fn honors_retry_after_and_exponential_fallback() {
        assert_eq!(rate_limit_delay(Some(7), 0), Duration::from_secs(7));
        assert_eq!(rate_limit_delay(Some(0), 0), Duration::from_secs(1));
        assert_eq!(rate_limit_delay(Some(999), 0), Duration::from_secs(120));
        assert_eq!(rate_limit_delay(None, 0), Duration::from_secs(1));
        assert_eq!(rate_limit_delay(None, 3), Duration::from_secs(8));
    }

    #[test]
    fn includes_cover_upload_and_playback_scopes() {
        assert!(REQUIRED_SCOPES.contains(&"ugc-image-upload"));
        assert!(REQUIRED_SCOPES.contains(&"user-modify-playback-state"));
        assert!(is_gracefully_degradable_status(403));
        assert!(!is_gracefully_degradable_status(500));
    }
}
