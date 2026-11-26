//! Local OS integrations for `vynl`: XDG/Windows path resolution, media keys
//! (MPRIS2 / SMTC / `MediaRemote`), desktop track notifications, and local IPC
//! scripting (`vynl status --json`, `vynl next`, `vynl play-pause`).
//!
//! All integrations operate strictly locally without third-party cloud APIs (ADR-0003).

pub mod ipc;
pub mod media;
pub mod notify;
pub mod paths;

pub use ipc::{
    IpcClient, IpcCommand, IpcError, IpcResponse, IpcServer, StatusSnapshot, ipc_transport_name,
};
pub use media::{
    LocalMediaBridge, MediaControlBridge, MediaControlEvent, MediaMetadata, MediaPlaybackStatus,
    media_control_backend_name,
};
pub use notify::{
    DispatchStatus, LocalOsNotifier, NotificationDispatcher, NotificationError,
    NotificationPayload, RecordingNotifier, format_notification_summary,
};
pub use paths::{AppPaths, EnvSnapshot};

#[cfg(test)]
mod tests {
    use vynl_core::model::TrackSummary;

    use super::{
        IpcCommand, format_notification_summary, ipc_transport_name, media_control_backend_name,
    };

    #[test]
    fn preserves_existing_public_api_surface() {
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
