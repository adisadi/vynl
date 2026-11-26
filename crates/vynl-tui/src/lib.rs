//! Interactive TUI, CLI parser, and diagnostics for the `vynl` binary crate.

pub mod app;
pub mod auth;
pub mod buffer;
pub mod input;
pub mod layout;
pub mod render;
pub mod spotify_worker;
pub mod storage;

use std::env;

use vynl_images::ImageProtocol;
use vynl_player::CompiledPlayerFeatures;
use vynl_spotify::AuthState;
use vynl_system::paths::AppPaths;
use vynl_system::{IpcCommand, ipc_transport_name, media_control_backend_name};

use crate::auth::{AuthOptions, DEFAULT_AUTH_PORT};

/// Parsed top-level CLI invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAction {
    /// Launch the interactive TUI (with optional offline demo catalog).
    LaunchTui {
        /// Load built-in demo catalog with procedural album art.
        demo: bool,
    },
    /// Run the interactive Spotify OAuth 2.0 PKCE login (or logout) workflow.
    Auth(AuthOptions),
    /// Render a single demo TUI frame to stdout and exit (useful for testing & VHS demos).
    RenderDemo {
        /// Terminal width in columns.
        cols: u16,
        /// Terminal height in rows.
        rows: u16,
        /// Protocol override (`None` = auto-detect, or Halfblocks for plain ANSI output).
        protocol: Option<ImageProtocol>,
        /// Emit plain text instead of ANSI escape codes.
        plain: bool,
    },
    /// Print environment, terminal image protocol, paths, and compiled feature diagnostics.
    Doctor,
    /// Print CLI usage help.
    Help,
    /// Print version string.
    Version,
    /// Forward a control command to a running `vynl` instance via local IPC.
    Ipc(IpcCommand),
}

/// Parses command-line arguments (excluding `argv[0]`).
///
/// # Errors
/// Returns a human-readable error message when an unknown subcommand or flag is passed.
pub fn parse_args<I, S>(args: I) -> Result<CliAction, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let tokens: Vec<String> = args.into_iter().map(|s| s.as_ref().to_owned()).collect();
    if tokens.first().map(String::as_str) == Some("auth") {
        return parse_auth_args(&tokens[1..]).map(CliAction::Auth);
    }

    match tokens.as_slice() {
        [] => Ok(CliAction::LaunchTui { demo: false }),
        [flag] if flag == "--demo" => Ok(CliAction::LaunchTui { demo: true }),
        [flag] if flag == "--render-demo" => Ok(CliAction::RenderDemo {
            cols: 140,
            rows: 36,
            protocol: Some(ImageProtocol::Halfblocks),
            plain: false,
        }),
        [flag, opt] if flag == "--render-demo" && opt == "--plain" => Ok(CliAction::RenderDemo {
            cols: 140,
            rows: 36,
            protocol: Some(ImageProtocol::Halfblocks),
            plain: true,
        }),
        [flag] if flag == "-h" || flag == "--help" || flag == "help" => Ok(CliAction::Help),
        [flag] if flag == "-V" || flag == "--version" => Ok(CliAction::Version),
        [cmd] if cmd == "doctor" => Ok(CliAction::Doctor),
        [cmd] if cmd == "play-pause" => Ok(CliAction::Ipc(IpcCommand::PlayPause)),
        [cmd] if cmd == "next" => Ok(CliAction::Ipc(IpcCommand::Next)),
        [cmd] if cmd == "prev" => Ok(CliAction::Ipc(IpcCommand::Prev)),
        [cmd] if cmd == "status" => Ok(CliAction::Ipc(IpcCommand::Status { json: false })),
        [cmd, flag] if cmd == "status" && (flag == "--json" || flag == "--format=json") => {
            Ok(CliAction::Ipc(IpcCommand::Status { json: true }))
        }
        [cmd, delta_s] if cmd == "volume" => {
            let delta: i8 = delta_s.parse().map_err(|_| {
                format!("invalid volume delta `{delta_s}` (expected e.g. +5 or -5)")
            })?;
            Ok(CliAction::Ipc(IpcCommand::VolumeDelta(delta)))
        }
        [cmd, uri] if cmd == "open" => Ok(CliAction::Ipc(IpcCommand::OpenUri(uri.clone()))),
        [other, ..] => Err(format!(
            "unknown command `{other}` — run `vynl --help` for usage"
        )),
    }
}

fn parse_auth_args(args: &[String]) -> Result<AuthOptions, String> {
    let mut opts = AuthOptions {
        client_id: None,
        port: DEFAULT_AUTH_PORT,
        logout: false,
    };

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "logout" | "--logout" => {
                opts.logout = true;
                i += 1;
            }
            "--client-id" => {
                let Some(val) = args.get(i + 1) else {
                    return Err(String::from("missing value for `--client-id`"));
                };
                if val.trim().is_empty() {
                    return Err(String::from("`--client-id` cannot be empty"));
                }
                opts.client_id = Some(val.trim().to_owned());
                i += 2;
            }
            "--port" => {
                let Some(val) = args.get(i + 1) else {
                    return Err(String::from("missing value for `--port`"));
                };
                let port: u16 = val
                    .parse()
                    .map_err(|_| format!("invalid port `{val}` (expected 1..=65535)"))?;
                if port == 0 {
                    return Err(String::from("`--port` must be between 1 and 65535"));
                }
                opts.port = port;
                i += 2;
            }
            other => {
                return Err(format!(
                    "unknown `vynl auth` argument `{other}` — run `vynl --help` for usage"
                ));
            }
        }
    }

    Ok(opts)
}

/// Detects the active terminal image protocol from the current process environment.
#[must_use]
pub fn detect_env_protocol() -> ImageProtocol {
    resolve_env_protocol(vynl_core::config::ImageProtocolPreference::Auto)
}

/// Resolves the terminal image protocol honoring a configured [`vynl_core::config::ImageProtocolPreference`].
#[must_use]
pub fn resolve_env_protocol(
    preference: vynl_core::config::ImageProtocolPreference,
) -> ImageProtocol {
    let term_program = env::var("TERM_PROGRAM").ok();
    let term = env::var("TERM").ok();
    let kitty_window = env::var("KITTY_WINDOW_ID").ok();
    let in_tmux = env::var("TMUX").is_ok();
    ImageProtocol::resolve(
        preference,
        term_program.as_deref(),
        term.as_deref(),
        kitty_window.as_deref(),
        in_tmux,
    )
}

/// Formats the `vynl doctor` diagnostic report for bug reports and terminal verification.
#[must_use]
pub fn format_doctor_report() -> String {
    let paths = AppPaths::resolve();
    let (cfg, cfg_status) = match storage::load_config(&paths) {
        Ok(cfg) => (
            cfg,
            if paths.config_file().is_file() {
                "loaded"
            } else {
                "defaults (not created yet)"
            }
            .to_owned(),
        ),
        Err(err) => (
            vynl_core::config::AppConfig::default(),
            format!("error ({err})"),
        ),
    };
    let feats = CompiledPlayerFeatures::current();
    let term_program = env::var("TERM_PROGRAM").ok();
    let term = env::var("TERM").ok();
    let in_tmux = env::var("TMUX").is_ok();

    let protocol = resolve_env_protocol(cfg.images.protocol);
    let list_thumbs = protocol.render_list_thumbnails(cfg.images.list_thumbnails);
    let has_tokens = matches!(storage::load_tokens(&paths), Ok(Some(_)));
    let auth_state = if has_tokens && cfg.spotify.client_id.is_some() {
        AuthState::Authenticated
    } else {
        vynl_spotify::initial_auth_state(&cfg.spotify)
    };
    let custom_themes = storage::discover_custom_themes(&paths);

    format!(
        "vynl {} (GPL-3.0-or-later)\n\
         target:            {} / {}\n\
         TERM:              {}\n\
         TERM_PROGRAM:      {}\n\
         tmux detected:     {}\n\
         image protocol:    {} (list thumbnails: {})\n\
         audio backends:    rodio={}, subprocess={}, visualizer={}\n\
         spotify auth:      {auth_state:?} ({} OAuth scopes)\n\
         active theme:      {} (custom themes: {})\n\
         media controls:    {}\n\
         ipc transport:     {} ({})\n\
         config path:       {} ({cfg_status})\n\
         themes dir:        {}\n\
         metadata cache:    {}\n\
         image cache:       {}\n\
         policy:            strictly Spotify-only (no external third-party APIs)",
        env!("CARGO_PKG_VERSION"),
        env::consts::OS,
        env::consts::ARCH,
        term.as_deref().unwrap_or("<unset>"),
        term_program.as_deref().unwrap_or("<unset>"),
        in_tmux,
        protocol.as_str(),
        list_thumbs,
        feats.rodio,
        feats.subprocess,
        feats.visualizer,
        vynl_spotify::REQUIRED_SCOPES.len(),
        cfg.ui.theme,
        custom_themes.len(),
        media_control_backend_name(),
        ipc_transport_name(),
        paths.ipc_socket_path.display(),
        paths.config_file().display(),
        paths.themes_dir.display(),
        paths.metadata_cache_file().display(),
        paths.image_cache_dir.display(),
    )
}

/// Returns CLI help text (English-only).
#[must_use]
pub const fn usage_text() -> &'static str {
    "vynl — Cross-platform Rust TUI Spotify client with native terminal cover art\n\n\
     USAGE:\n  \
       vynl                                      Launch the interactive TUI\n  \
       vynl --demo                               Launch TUI with built-in demo catalog & cover art\n  \
       vynl auth [--client-id ID] [--port PORT]  Authenticate with Spotify via OAuth 2.0 PKCE\n  \
       vynl auth logout                          Clear stored Spotify OAuth tokens\n  \
       vynl --render-demo                        Render one demo frame (add --plain for plain text)\n  \
       vynl doctor                               Print terminal image, path & platform diagnostics\n  \
       vynl status [--json]                      Print playback status from the running instance\n  \
       vynl play-pause                           Toggle playback via local IPC\n  \
       vynl next | prev                          Skip tracks via local IPC\n  \
       vynl volume <+N|-N>                       Adjust volume via local IPC\n  \
       vynl open <URI>                           Open a Spotify URI via local IPC\n  \
       vynl --version                            Print version\n  \
       vynl --help                               Show this help message"
}

#[cfg(test)]
mod tests {
    use vynl_system::IpcCommand;

    use crate::auth::{AuthOptions, DEFAULT_AUTH_PORT};

    use super::{CliAction, format_doctor_report, parse_args};

    #[test]
    fn parses_core_cli_subcommands() {
        assert_eq!(
            parse_args::<[&str; 0], &str>([]),
            Ok(CliAction::LaunchTui { demo: false })
        );
        assert_eq!(
            parse_args(["--demo"]),
            Ok(CliAction::LaunchTui { demo: true })
        );
        assert_eq!(
            parse_args(["auth"]),
            Ok(CliAction::Auth(AuthOptions::default()))
        );
        assert_eq!(
            parse_args(["auth", "--client-id", "abc123", "--port", "9000"]),
            Ok(CliAction::Auth(AuthOptions {
                client_id: Some(String::from("abc123")),
                port: 9000,
                logout: false,
            }))
        );
        assert_eq!(
            parse_args(["auth", "logout"]),
            Ok(CliAction::Auth(AuthOptions {
                client_id: None,
                port: DEFAULT_AUTH_PORT,
                logout: true,
            }))
        );
        assert!(parse_args(["auth", "--port", "0"]).is_err());
        assert_eq!(parse_args(["doctor"]), Ok(CliAction::Doctor));
        assert_eq!(
            parse_args(["status", "--json"]),
            Ok(CliAction::Ipc(IpcCommand::Status { json: true }))
        );
        assert_eq!(
            parse_args(["play-pause"]),
            Ok(CliAction::Ipc(IpcCommand::PlayPause))
        );
        assert_eq!(
            parse_args(["volume", "+5"]),
            Ok(CliAction::Ipc(IpcCommand::VolumeDelta(5)))
        );
        assert!(parse_args(["nonexistent"]).is_err());
    }

    #[test]
    fn doctor_report_contains_expected_sections() {
        let report = format_doctor_report();
        assert!(report.contains("vynl "));
        assert!(report.contains("image protocol:"));
        assert!(report.contains("strictly Spotify-only"));
        assert!(report.contains("config path:"));
        assert!(report.contains("themes dir:"));
        assert!(report.contains("metadata cache:"));
    }
}
