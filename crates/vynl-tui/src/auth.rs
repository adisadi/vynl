//! Live interactive `vynl auth` OAuth 2.0 PKCE authentication flow.

use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

use vynl_spotify::api::SpotifyApiClient;
use vynl_spotify::pkce::{CallbackServer, PkceChallenge, build_authorize_url, current_epoch_secs};
use vynl_system::paths::AppPaths;

use crate::storage;

/// Default localhost port for the Spotify OAuth 2.0 PKCE redirect callback server.
pub const DEFAULT_AUTH_PORT: u16 = 8888;

/// Maximum time to wait for the browser OAuth callback before timing out.
pub const DEFAULT_AUTH_TIMEOUT: Duration = Duration::from_secs(180);

/// Parsed options for the `vynl auth` CLI subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthOptions {
    /// Optional Spotify Developer Client ID override (saved to `config.toml`).
    pub client_id: Option<String>,
    /// Localhost TCP port for `http://127.0.0.1:<port>/callback`.
    pub port: u16,
    /// When `true`, clears stored OAuth tokens instead of running PKCE login.
    pub logout: bool,
}

impl Default for AuthOptions {
    fn default() -> Self {
        Self {
            client_id: None,
            port: DEFAULT_AUTH_PORT,
            logout: false,
        }
    }
}

/// Runs the live `vynl auth` OAuth 2.0 PKCE workflow (or `vynl auth logout`).
///
/// 1. Resolves `AppPaths` and loads `~/.config/vynl/config.toml`.
/// 2. Persists a supplied or interactively entered Spotify Developer `client_id`.
/// 3. Binds a local loopback HTTP callback server (`127.0.0.1:<port>/callback`).
/// 4. Generates a high-entropy PKCE `S256` challenge and opens the authorize URL.
/// 5. Exchanges the authorization code via `reqwest` (`rustls-tls`) and stores the
///    resulting [`vynl_spotify::pkce::TokenBundle`] in the OS keyring (`0600` file fallback).
///
/// # Errors
/// Returns a clear English error message if configuration, binding, callback validation,
/// token exchange, or token storage fails.
pub fn run_auth_command(options: &AuthOptions) -> Result<String, String> {
    let paths = AppPaths::resolve();
    paths
        .ensure_dirs()
        .map_err(|err| format!("failed to create application directories: {err}"))?;

    if options.logout {
        storage::clear_tokens(&paths)?;
        return Ok(String::from(
            "Cleared stored Spotify OAuth tokens successfully.",
        ));
    }

    let mut config = storage::load_config(&paths)?;
    let client_id = resolve_or_prompt_client_id(options.client_id.as_deref(), &mut config, &paths)?;

    let server = CallbackServer::bind(options.port)?;
    let redirect_uri = server.redirect_uri()?;
    let challenge = PkceChallenge::generate()?;
    let authorize_url = build_authorize_url(&client_id, &redirect_uri, &challenge);

    println!("Spotify OAuth 2.0 PKCE Login");
    println!("----------------------------");
    println!("Redirect URI: {redirect_uri}");
    println!("Opening your browser to authorize vynl...");
    println!("If your browser does not open automatically, visit:\n\n  {authorize_url}\n");
    println!(
        "Waiting up to {} seconds for Spotify callback...",
        DEFAULT_AUTH_TIMEOUT.as_secs()
    );

    let _ = try_open_browser(&authorize_url);

    let callback = server.wait_for_callback(&challenge.state, DEFAULT_AUTH_TIMEOUT)?;

    let mut client = SpotifyApiClient::new_reqwest("")
        .map_err(|err| format!("failed to initialize HTTPS client: {err}"))?;
    let bundle = client
        .exchange_pkce_code(
            &client_id,
            &callback.code,
            &redirect_uri,
            &challenge.verifier,
            current_epoch_secs(),
        )
        .map_err(|err| format!("Spotify OAuth token exchange failed: {err}"))?;

    storage::save_tokens(&paths, &bundle)?;

    client.set_access_token(bundle.access_token.clone());
    match client.me() {
        Ok(profile) => Ok(format!(
            "Authenticated as {} ({}) — OAuth tokens saved securely.",
            profile.display_name, profile.id
        )),
        Err(_) => Ok(String::from(
            "Spotify OAuth tokens saved securely. Launch `vynl` to start listening.",
        )),
    }
}

fn resolve_or_prompt_client_id(
    cli_override: Option<&str>,
    config: &mut vynl_core::config::AppConfig,
    paths: &AppPaths,
) -> Result<String, String> {
    if let Some(id) = cli_override {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            return Err(String::from("--client-id cannot be empty"));
        }
        config.spotify.client_id = Some(trimmed.to_owned());
        storage::save_config(paths, config)?;
        return Ok(trimmed.to_owned());
    }

    if let Some(ref existing) = config.spotify.client_id {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_owned());
        }
    }

    println!("No Spotify Developer Client ID is configured yet.");
    println!("1. Create an app at https://developer.spotify.com/dashboard");
    println!("2. Add Redirect URI: http://127.0.0.1:8888/callback");
    print!("Enter your Spotify Client ID: ");
    io::stdout()
        .flush()
        .map_err(|err| format!("failed to flush stdout: {err}"))?;

    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|err| format!("failed to read Client ID from stdin: {err}"))?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(String::from(
            "aborted: a Spotify Developer Client ID is required (pass --client-id <ID>)",
        ));
    }

    config.spotify.client_id = Some(trimmed.to_owned());
    storage::save_config(paths, config)?;
    Ok(trimmed.to_owned())
}

fn try_open_browser(url: &str) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(url);
        c
    };

    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    };

    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };

    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::{AuthOptions, DEFAULT_AUTH_PORT};

    #[test]
    fn auth_options_default_uses_standard_pkce_port() {
        let opts = AuthOptions::default();
        assert_eq!(opts.client_id, None);
        assert_eq!(opts.port, DEFAULT_AUTH_PORT);
        assert!(!opts.logout);
    }
}
