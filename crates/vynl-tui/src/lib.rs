//! CLI parser and terminal diagnostics for the `vynl` binary crate.

use std::env;

use vynl_core::config::AppConfig;
use vynl_images::ImageProtocol;
use vynl_player::CompiledPlayerFeatures;
use vynl_system::{IpcCommand, ipc_transport_name, media_control_backend_name};

/// Parsed top-level CLI invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAction {
    /// Launch the interactive TUI (Phase 1 Ratatui loop).
    LaunchTui,
    /// Print environment, terminal image protocol, and compiled feature diagnostics.
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
    match tokens.as_slice() {
        [] => Ok(CliAction::LaunchTui),
        [flag] if flag == "-h" || flag == "--help" || flag == "help" => Ok(CliAction::Help),
        [flag] if flag == "-V" || flag == "--version" => Ok(CliAction::Version),
        [cmd] if cmd == "doctor" => Ok(CliAction::Doctor),
        [cmd] if cmd == "play-pause" => Ok(CliAction::Ipc(IpcCommand::PlayPause)),
        [cmd] if cmd == "next" => Ok(CliAction::Ipc(IpcCommand::Next)),
        [cmd] if cmd == "prev" => Ok(CliAction::Ipc(IpcCommand::Prev)),
        [cmd] if cmd == "status" => Ok(CliAction::Ipc(IpcCommand::Status { json: false })),
        [cmd, flag] if cmd == "status" && flag == "--json" => {
            Ok(CliAction::Ipc(IpcCommand::Status { json: true }))
        }
        [cmd, uri] if cmd == "open" => Ok(CliAction::Ipc(IpcCommand::OpenUri(uri.clone()))),
        [other, ..] => Err(format!(
            "unknown command `{other}` — run `vynl --help` for usage"
        )),
    }
}

/// Formats the `vynl doctor` diagnostic report for bug reports and terminal verification.
pub fn format_doctor_report() -> String {
    let cfg = AppConfig::default();
    let feats = CompiledPlayerFeatures::current();
    let term_program = env::var("TERM_PROGRAM").ok();
    let term = env::var("TERM").ok();
    let kitty_window = env::var("KITTY_WINDOW_ID").ok();
    let in_tmux = env::var("TMUX").is_ok();

    let protocol = ImageProtocol::detect_from_env(
        term_program.as_deref(),
        term.as_deref(),
        kitty_window.as_deref(),
        in_tmux,
    );
    let list_thumbs = protocol.render_list_thumbnails(cfg.images.list_thumbnails);

    let auth_state = vynl_spotify::initial_auth_state(&cfg.spotify);

    format!(
        "vynl {} (GPL-3.0-or-later)\n\
         target:            {} / {}\n\
         TERM:              {}\n\
         TERM_PROGRAM:      {}\n\
         tmux detected:     {}\n\
         image protocol:    {} (list thumbnails: {})\n\
         audio backends:    rodio={}, subprocess={}, visualizer={}\n\
         spotify auth:      {:?} ({} OAuth scopes)\n\
         media controls:    {}\n\
         ipc transport:     {}\n\
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
        auth_state,
        vynl_spotify::REQUIRED_SCOPES.len(),
        media_control_backend_name(),
        ipc_transport_name(),
    )
}

/// Returns CLI help text (English-only).
pub const fn usage_text() -> &'static str {
    "vynl — Cross-platform Rust TUI Spotify client with native terminal cover art\n\n\
     USAGE:\n  \
       vynl                 Launch the interactive TUI\n  \
       vynl doctor          Print terminal image & platform diagnostics\n  \
       vynl status [--json] Print playback status from the running instance\n  \
       vynl play-pause      Toggle playback via local IPC\n  \
       vynl next | prev     Skip tracks via local IPC\n  \
       vynl open <URI>      Open a Spotify URI via local IPC\n  \
       vynl --version       Print version\n  \
       vynl --help          Show this help message"
}

#[cfg(test)]
mod tests {
    use vynl_system::IpcCommand;

    use super::{CliAction, format_doctor_report, parse_args};

    #[test]
    fn parses_core_cli_subcommands() {
        assert_eq!(parse_args::<[&str; 0], &str>([]), Ok(CliAction::LaunchTui));
        assert_eq!(parse_args(["doctor"]), Ok(CliAction::Doctor));
        assert_eq!(
            parse_args(["status", "--json"]),
            Ok(CliAction::Ipc(IpcCommand::Status { json: true }))
        );
        assert_eq!(
            parse_args(["play-pause"]),
            Ok(CliAction::Ipc(IpcCommand::PlayPause))
        );
        assert!(parse_args(["nonexistent"]).is_err());
    }

    #[test]
    fn doctor_report_contains_expected_sections() {
        let report = format_doctor_report();
        assert!(report.contains("vynl "));
        assert!(report.contains("image protocol:"));
        assert!(report.contains("strictly Spotify-only"));
    }
}
