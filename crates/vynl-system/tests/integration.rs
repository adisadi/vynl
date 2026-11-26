//! Integration tests for `vynl-system` OS integrations and live Unix Domain Socket IPC.

use std::path::PathBuf;

use vynl_core::model::{ConnectDevice, PlaybackState, TrackSummary};
use vynl_system::media::{LocalMediaBridge, MediaControlBridge, MediaPlaybackStatus};
use vynl_system::notify::{
    DispatchStatus, NotificationDispatcher, NotificationPayload, RecordingNotifier,
};
use vynl_system::paths::{AppPaths, EnvSnapshot};

fn sample_playback_state() -> PlaybackState {
    PlaybackState {
        current_track: Some(TrackSummary {
            id: String::from("track-42"),
            title: String::from("Midnight City"),
            artists: vec![String::from("M83")],
            album: String::from("Hurry Up, We're Dreaming"),
            duration_ms: 243_000,
            explicit: false,
            is_liked: true,
            cover: None,
        }),
        is_playing: true,
        position_ms: 64_500,
        volume_percent: 75,
        muted: false,
        active_device: Some(ConnectDevice {
            id: String::from("dev-vynl"),
            name: String::from("vynl"),
            device_type: String::from("Computer"),
            is_active: true,
            volume_percent: 75,
        }),
        ..PlaybackState::default()
    }
}

#[test]
fn app_paths_xdg_overrides_and_windows_resolution() {
    let unix_env = EnvSnapshot {
        xdg_config_home: Some(PathBuf::from("/xdg/cfg")),
        xdg_cache_home: Some(PathBuf::from("/xdg/cache")),
        xdg_state_home: Some(PathBuf::from("/xdg/state")),
        xdg_runtime_dir: Some(PathBuf::from("/xdg/run")),
        home: Some(PathBuf::from("/home/user")),
        appdata: None,
        local_appdata: None,
        temp_dir: PathBuf::from("/tmp"),
    };
    let unix_paths = AppPaths::from_env(&unix_env, false);
    assert_eq!(unix_paths.config_dir, PathBuf::from("/xdg/cfg/vynl"));
    assert_eq!(unix_paths.themes_dir, PathBuf::from("/xdg/cfg/vynl/themes"));
    assert_eq!(unix_paths.cache_dir, PathBuf::from("/xdg/cache/vynl"));
    assert_eq!(
        unix_paths.image_cache_dir,
        PathBuf::from("/xdg/cache/vynl/images")
    );
    assert_eq!(
        unix_paths.audio_cache_dir,
        PathBuf::from("/xdg/cache/vynl/audio")
    );
    assert_eq!(unix_paths.state_dir, PathBuf::from("/xdg/state/vynl"));
    assert_eq!(
        unix_paths.log_file,
        PathBuf::from("/xdg/state/vynl/vynl.log")
    );
    assert_eq!(
        unix_paths.ipc_socket_path,
        PathBuf::from("/xdg/run/vynl.sock")
    );

    let win_env = EnvSnapshot {
        appdata: Some(PathBuf::from("C:/Users/dev/AppData/Roaming")),
        local_appdata: Some(PathBuf::from("C:/Users/dev/AppData/Local")),
        temp_dir: PathBuf::from("C:/Temp"),
        ..EnvSnapshot::default()
    };
    let win_paths = AppPaths::from_env(&win_env, true);
    assert_eq!(
        win_paths.config_dir,
        PathBuf::from("C:/Users/dev/AppData/Roaming/vynl")
    );
    assert_eq!(
        win_paths.cache_dir,
        PathBuf::from("C:/Users/dev/AppData/Local/vynl/cache")
    );
    assert_eq!(
        win_paths.state_dir,
        PathBuf::from("C:/Users/dev/AppData/Local/vynl/state")
    );
    assert_eq!(win_paths.ipc_socket_path, PathBuf::from(r"\\.\pipe\vynl"));
}

#[test]
fn recording_notifier_and_media_bridge_integration() -> Result<(), String> {
    let playback = sample_playback_state();
    let Some(track) = playback.current_track.as_ref() else {
        return Err(String::from("expected sample track"));
    };

    let notifier = RecordingNotifier::new();
    let payload = NotificationPayload::from_track(track, Some(PathBuf::from("/tmp/m83.jpg")));
    assert_eq!(
        notifier.notify(&payload).map_err(|err| err.to_string())?,
        DispatchStatus::Dispatched
    );
    assert_eq!(notifier.recorded(), vec![payload]);

    let mut media_bridge = LocalMediaBridge::new();
    media_bridge.sync_from_playback_state(&playback);
    assert_eq!(media_bridge.status(), MediaPlaybackStatus::Playing);
    assert_eq!(media_bridge.position_ms(), 64_500);
    assert_eq!(media_bridge.volume_percent(), 75);
    assert_ne!(media_bridge.backend_name(), "");
    assert_eq!(
        media_bridge.metadata().map(|meta| meta.title.as_str()),
        Some("Midnight City")
    );
    Ok(())
}

#[cfg(unix)]
fn apply_test_ipc_command(
    cmd: vynl_system::ipc::IpcCommand,
    playback: &mut PlaybackState,
    opened_uris: &mut Vec<String>,
) -> vynl_system::ipc::IpcResponse {
    use vynl_system::ipc::{IpcCommand, IpcResponse, StatusSnapshot};

    match cmd {
        IpcCommand::Status { .. } => {
            IpcResponse::with_status("ok", StatusSnapshot::from_playback_state(playback))
        }
        IpcCommand::PlayPause => {
            playback.is_playing = !playback.is_playing;
            IpcResponse::ok(if playback.is_playing {
                "playing"
            } else {
                "paused"
            })
        }
        IpcCommand::Next => {
            if let Some(track) = playback.current_track.as_mut() {
                track.title = String::from("Wait");
            }
            playback.position_ms = 0;
            IpcResponse::ok("skipped to next track")
        }
        IpcCommand::Prev => {
            playback.position_ms = 0;
            IpcResponse::ok("skipped to previous track")
        }
        IpcCommand::VolumeDelta(delta) => {
            let current = i16::from(playback.volume_percent);
            let adjusted = (current + i16::from(delta)).clamp(0, 100);
            playback.volume_percent = u8::try_from(adjusted).unwrap_or(0);
            IpcResponse::ok(format!("volume set to {}%", playback.volume_percent))
        }
        IpcCommand::OpenUri(uri) => {
            opened_uris.push(uri.clone());
            IpcResponse::ok(format!("opened {uri}"))
        }
    }
}

#[cfg(unix)]
fn unique_temp_dir(prefix: &str) -> Result<PathBuf, String> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| err.to_string())?
        .as_nanos();
    Ok(std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id())))
}

#[cfg(unix)]
#[test]
fn live_unix_domain_socket_round_trip() -> Result<(), String> {
    use std::fs;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use vynl_system::ipc::{IpcClient, IpcCommand, IpcServer, StatusSnapshot};

    let temp_dir = unique_temp_dir("vynl-ipc-it")?;
    let socket_path = temp_dir.join("vynl.sock");

    // Pre-create a stale file at the socket path to verify IpcServer cleans it up on bind.
    fs::create_dir_all(&temp_dir).map_err(|err| err.to_string())?;
    fs::write(&socket_path, b"stale").map_err(|err| err.to_string())?;

    let shared = Arc::new(Mutex::new((sample_playback_state(), Vec::<String>::new())));
    let server_shared = Arc::clone(&shared);

    let server = IpcServer::bind(&socket_path, move |cmd| {
        let mut guard = match server_shared.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let (playback, uris) = &mut *guard;
        apply_test_ipc_command(cmd, playback, uris)
    })
    .map_err(|err| err.to_string())?;

    assert_eq!(server.socket_path(), socket_path.as_path());
    assert!(socket_path.exists());

    // Binding a second server to the active socket must fail without unlinking it.
    let duplicate_bind = IpcServer::bind(&socket_path, |_| vynl_system::ipc::IpcResponse::ok(""));
    assert!(duplicate_bind.is_err());

    let client = IpcClient::with_timeout(&socket_path, Duration::from_secs(2));

    // 1. Status { json: true }
    let status_resp = client
        .send(&IpcCommand::Status { json: true })
        .map_err(|err| err.to_string())?;
    assert!(status_resp.ok);
    let Some(snapshot) = status_resp.status.as_ref() else {
        return Err(String::from("expected status snapshot in Status response"));
    };
    assert!(snapshot.is_playing);
    assert_eq!(snapshot.title, "Midnight City");
    assert_eq!(snapshot.artist, "M83");
    assert_eq!(snapshot.album, "Hurry Up, We're Dreaming");
    assert_eq!(snapshot.position_ms, 64_500);
    assert_eq!(snapshot.duration_ms, 243_000);
    assert_eq!(snapshot.volume_percent, 75);
    assert_eq!(snapshot.device_name, "vynl");

    // Verify `vynl status --json` output is valid round-trippable JSON.
    let cli_json = status_resp.format_cli_output(true);
    let reparsed = StatusSnapshot::from_json(&cli_json).map_err(|err| err.to_string())?;
    assert_eq!(reparsed, *snapshot);

    // 2. PlayPause, Next, Prev, VolumeDelta, OpenUri
    let toggle_resp = client
        .send(&IpcCommand::PlayPause)
        .map_err(|err| err.to_string())?;
    assert!(toggle_resp.ok);
    assert_eq!(toggle_resp.message, "paused");

    let next_resp = client
        .send(&IpcCommand::Next)
        .map_err(|err| err.to_string())?;
    assert!(next_resp.ok);
    assert_eq!(next_resp.message, "skipped to next track");

    let prev_resp = client
        .send(&IpcCommand::Prev)
        .map_err(|err| err.to_string())?;
    assert!(prev_resp.ok);
    assert_eq!(prev_resp.message, "skipped to previous track");

    let vol_resp = client
        .send(&IpcCommand::VolumeDelta(-10))
        .map_err(|err| err.to_string())?;
    assert!(vol_resp.ok);
    assert_eq!(vol_resp.message, "volume set to 65%");

    let uri = String::from("spotify:track:6y0igZArWVi6Iz0rj35c1Y");
    let open_resp = client
        .send(&IpcCommand::OpenUri(uri.clone()))
        .map_err(|err| err.to_string())?;
    assert!(open_resp.ok);
    assert_eq!(open_resp.message, format!("opened {uri}"));

    // Confirm updated status reflects PlayPause + Next + VolumeDelta.
    let updated_resp = client
        .send(&IpcCommand::Status { json: true })
        .map_err(|err| err.to_string())?;
    let Some(updated) = updated_resp.status.as_ref() else {
        return Err(String::from("expected updated status snapshot"));
    };
    assert!(!updated.is_playing);
    assert_eq!(updated.title, "Wait");
    assert_eq!(updated.position_ms, 0);
    assert_eq!(updated.volume_percent, 65);

    let recorded_uris = match shared.lock() {
        Ok(guard) => guard.1.clone(),
        Err(poisoned) => poisoned.into_inner().1.clone(),
    };
    assert_eq!(recorded_uris, vec![uri]);

    // Dropping the server must cleanly remove the Unix socket file.
    drop(server);
    assert!(!socket_path.exists());

    let _ = fs::remove_dir_all(&temp_dir);
    Ok(())
}
