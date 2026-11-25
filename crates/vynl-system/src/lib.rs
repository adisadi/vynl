//! Local OS integrations for `vynl`: media keys (MPRIS2 / SMTC / `MediaRemote`),
//! desktop track notifications, and local IPC scripting (`vynl status`, `vynl next`).
//!
//! All integrations operate strictly locally without third-party cloud APIs (ADR-0003).

/// Local IPC command sent from a short-lived CLI invocation to the running `vynl` TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IpcCommand {
    /// Query current playback status (human-readable or JSON for Waybar/tmux/`SketchyBar`).
    Status {
        /// Output machine-readable JSON when `true`.
        json: bool,
    },
    /// Toggle play/pause.
    PlayPause,
    /// Skip to the next track.
    Next,
    /// Skip to the previous track.
    Prev,
    /// Adjust volume by a signed delta (`-100..=100`).
    VolumeDelta(i8),
    /// Open a Spotify URI (`spotify:track:...`, `spotify:album:...`, etc.).
    OpenUri(String),
}

/// Platform media-control backend compiled for the current target OS.
pub const fn media_control_backend_name() -> &'static str {
    if cfg!(target_os = "linux") {
        "MPRIS2 (D-Bus / zbus)"
    } else if cfg!(target_os = "macos") {
        "macOS MediaRemote"
    } else if cfg!(target_os = "windows") {
        "Windows SMTC"
    } else {
        "none (unsupported OS)"
    }
}

/// Default local IPC endpoint description for diagnostics.
pub const fn ipc_transport_name() -> &'static str {
    if cfg!(windows) {
        "Windows Named Pipe"
    } else {
        "Unix Domain Socket"
    }
}

/// Formats a track change summary for local desktop notifications.
pub fn format_notification_summary(track: &vynl_core::model::TrackSummary) -> String {
    if track.artists.is_empty() {
        track.title.clone()
    } else {
        format!("{} — {}", track.title, track.artists.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use vynl_core::model::TrackSummary;

    use super::{
        IpcCommand, format_notification_summary, ipc_transport_name, media_control_backend_name,
    };

    #[test]
    fn reports_non_empty_platform_integration_names() {
        assert_ne!(media_control_backend_name(), "");
        assert_ne!(ipc_transport_name(), "");
        assert_eq!(
            IpcCommand::Status { json: true },
            IpcCommand::Status { json: true }
        );

        let track = TrackSummary {
            id: String::from("1"),
            title: String::from("Instant Crush"),
            artists: vec![String::from("Daft Punk")],
            album: String::from("Random Access Memories"),
            duration_ms: 337_000,
            explicit: false,
            is_liked: true,
            cover: None,
        };
        assert_eq!(
            format_notification_summary(&track),
            "Instant Crush — Daft Punk"
        );
    }
}
