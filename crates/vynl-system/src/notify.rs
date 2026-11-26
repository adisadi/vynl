//! Local desktop notification dispatching for `vynl`.
//!
//! Dispatches track-change notifications exclusively to the local OS notification
//! center (pure-Rust D-Bus `org.freedesktop.Notifications` with `notify-send`
//! fallback on Linux, `osascript` on macOS) without any cloud calls, and provides
//! [`RecordingNotifier`] for deterministic automated testing.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use vynl_core::model::TrackSummary;

/// Payload describing a local desktop notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationPayload {
    /// Short notification title (typically the track title or summary).
    pub title: String,
    /// Secondary notification body text (typically artist and album).
    pub body: String,
    /// Optional path to a locally cached album cover image.
    pub icon_path: Option<PathBuf>,
}

impl NotificationPayload {
    /// Creates a new notification payload.
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        body: impl Into<String>,
        icon_path: Option<PathBuf>,
    ) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            icon_path,
        }
    }

    /// Builds a track-change notification payload from a [`TrackSummary`].
    #[must_use]
    pub fn from_track(track: &TrackSummary, icon_path: Option<PathBuf>) -> Self {
        let artist_line = track.artist_line();
        let body = if track.album.is_empty() {
            artist_line
        } else {
            format!("{artist_line} · {}", track.album)
        };
        Self {
            title: track.title.clone(),
            body,
            icon_path,
        }
    }
}

/// Outcome of attempting to dispatch a desktop notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchStatus {
    /// Notification was handed off to the OS notification daemon or recorder.
    Dispatched,
    /// Notification backend was unavailable or disabled; gracefully no-oped.
    Skipped,
}

/// Error returned when a notification dispatcher encounters an unexpected failure.
#[derive(Debug)]
pub struct NotificationError {
    message: String,
    source: Option<io::Error>,
}

impl NotificationError {
    /// Constructs a notification error wrapping an [`io::Error`].
    #[must_use]
    pub fn from_io(context: impl Into<String>, source: io::Error) -> Self {
        Self {
            message: context.into(),
            source: Some(source),
        }
    }
}

impl fmt::Display for NotificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(source) = &self.source {
            write!(f, "{}: {source}", self.message)
        } else {
            f.write_str(&self.message)
        }
    }
}

impl std::error::Error for NotificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|err| err as &(dyn std::error::Error + 'static))
    }
}

/// Trait implemented by desktop notification backends and test doubles.
pub trait NotificationDispatcher: Send + Sync {
    /// Dispatches a local desktop notification.
    ///
    /// # Errors
    ///
    /// Returns [`NotificationError`] only on unexpected dispatch failures; missing
    /// OS notification daemons should degrade gracefully to [`DispatchStatus::Skipped`].
    fn notify(&self, payload: &NotificationPayload) -> Result<DispatchStatus, NotificationError>;
}

/// Local OS notification dispatcher.
///
/// On Linux, dispatches via pure-Rust D-Bus (`org.freedesktop.Notifications`) when
/// a session bus is present and falls back to `notify-send` non-blockingly. On
/// macOS, invokes `osascript` non-blockingly. Any spawned child processes are
/// reaped on a background thread so the TUI event loop is never blocked and no
/// zombie processes accumulate. If no notification daemon or helper binary is
/// available, or if notifications are disabled, this dispatcher gracefully returns
/// [`DispatchStatus::Skipped`].
#[derive(Debug, Clone)]
pub struct LocalOsNotifier {
    enabled: bool,
    app_name: String,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    linux_command: PathBuf,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    use_custom_linux_command: bool,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    macos_command: PathBuf,
}

impl Default for LocalOsNotifier {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalOsNotifier {
    /// Creates an enabled local OS notifier with default helper binary names.
    #[must_use]
    pub fn new() -> Self {
        Self {
            enabled: true,
            app_name: String::from("vynl"),
            linux_command: PathBuf::from("notify-send"),
            use_custom_linux_command: false,
            macos_command: PathBuf::from("osascript"),
        }
    }

    /// Creates a local OS notifier with an explicit enabled toggle.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Overrides the Linux notification command binary (primarily for diagnostics/tests).
    #[must_use]
    pub fn with_linux_command(mut self, command: impl Into<PathBuf>) -> Self {
        self.linux_command = command.into();
        self.use_custom_linux_command = true;
        self
    }

    /// Overrides the macOS `osascript` binary path (primarily for diagnostics/tests).
    #[must_use]
    pub fn with_macos_command(mut self, command: impl Into<PathBuf>) -> Self {
        self.macos_command = command.into();
        self
    }

    /// Returns whether notifications are currently enabled.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Returns the application name reported to the OS notification center.
    #[must_use]
    pub fn app_name(&self) -> &str {
        &self.app_name
    }
}

impl NotificationDispatcher for LocalOsNotifier {
    fn notify(&self, payload: &NotificationPayload) -> Result<DispatchStatus, NotificationError> {
        if !self.enabled {
            return Ok(DispatchStatus::Skipped);
        }
        dispatch_os_notification(self, payload)
    }
}

#[cfg(target_os = "linux")]
fn has_dbus_session_bus() -> bool {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some_and(|val| !val.is_empty()) {
        return true;
    }
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR")
        && !runtime_dir.is_empty()
    {
        return std::path::Path::new(&runtime_dir).join("bus").exists();
    }
    false
}

#[cfg(target_os = "linux")]
fn try_dispatch_dbus_notification(app_name: &str, payload: &NotificationPayload) -> bool {
    if !has_dbus_session_bus() {
        return false;
    }

    let mut notification = notify_rust::Notification::new();
    notification
        .appname(app_name)
        .summary(&payload.title)
        .body(&payload.body);

    if let Some(path) = &payload.icon_path {
        let icon = path.to_string_lossy();
        notification.icon(&icon);
    }

    notification.show().is_ok()
}

#[cfg(target_os = "linux")]
fn dispatch_os_notification(
    notifier: &LocalOsNotifier,
    payload: &NotificationPayload,
) -> Result<DispatchStatus, NotificationError> {
    use std::process::{Command, Stdio};

    if !notifier.use_custom_linux_command
        && try_dispatch_dbus_notification(&notifier.app_name, payload)
    {
        return Ok(DispatchStatus::Dispatched);
    }

    let mut command = Command::new(&notifier.linux_command);
    command
        .arg("--app-name")
        .arg(&notifier.app_name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    if let Some(icon) = &payload.icon_path {
        command.arg("--icon").arg(icon);
    }

    command.arg("--").arg(&payload.title).arg(&payload.body);

    spawn_non_blocking_reaped(command, "failed to launch notify-send")
}

#[cfg(target_os = "macos")]
fn dispatch_os_notification(
    notifier: &LocalOsNotifier,
    payload: &NotificationPayload,
) -> Result<DispatchStatus, NotificationError> {
    use std::process::{Command, Stdio};

    let script = build_macos_applescript(&notifier.app_name, payload);
    let mut command = Command::new(&notifier.macos_command);
    command
        .arg("-e")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    spawn_non_blocking_reaped(command, "failed to launch osascript")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn dispatch_os_notification(
    _notifier: &LocalOsNotifier,
    _payload: &NotificationPayload,
) -> Result<DispatchStatus, NotificationError> {
    Ok(DispatchStatus::Skipped)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn spawn_non_blocking_reaped(
    mut command: std::process::Command,
    context: &str,
) -> Result<DispatchStatus, NotificationError> {
    match command.spawn() {
        Ok(mut child) => {
            // Reap the short-lived child process in the background to prevent zombies.
            let _ = std::thread::Builder::new()
                .name(String::from("vynl-notify-reaper"))
                .spawn(move || {
                    let _ = child.wait();
                });
            Ok(DispatchStatus::Dispatched)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(DispatchStatus::Skipped),
        Err(err) => Err(NotificationError::from_io(context, err)),
    }
}

/// Builds an `AppleScript` `display notification` command with safe string escaping.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn build_macos_applescript(app_name: &str, payload: &NotificationPayload) -> String {
    let body = escape_applescript_string(&payload.body);
    let title = escape_applescript_string(&payload.title);
    let subtitle = escape_applescript_string(app_name);
    format!("display notification \"{body}\" with title \"{title}\" subtitle \"{subtitle}\"")
}

#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn escape_applescript_string(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str(r#"\""#),
            '\n' | '\r' => out.push(' '),
            other => out.push(other),
        }
    }
    out
}

/// Thread-safe in-memory notification recorder for unit and integration tests.
#[derive(Debug, Default)]
pub struct RecordingNotifier {
    entries: Mutex<Vec<NotificationPayload>>,
}

impl RecordingNotifier {
    /// Creates an empty [`RecordingNotifier`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a cloned snapshot of all recorded notifications in dispatch order.
    #[must_use]
    pub fn recorded(&self) -> Vec<NotificationPayload> {
        let guard = match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.clone()
    }

    /// Returns the number of notifications recorded so far.
    #[must_use]
    pub fn len(&self) -> usize {
        let guard = match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.len()
    }

    /// Returns `true` when no notifications have been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clears all recorded notifications.
    pub fn clear(&self) {
        let mut guard = match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.clear();
    }
}

impl NotificationDispatcher for RecordingNotifier {
    fn notify(&self, payload: &NotificationPayload) -> Result<DispatchStatus, NotificationError> {
        let mut guard = match self.entries.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.push(payload.clone());
        Ok(DispatchStatus::Dispatched)
    }
}

/// Formats a track change summary for local desktop notifications.
#[must_use]
pub fn format_notification_summary(track: &TrackSummary) -> String {
    if track.artists.is_empty() {
        track.title.clone()
    } else {
        format!("{} — {}", track.title, track.artists.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use vynl_core::model::TrackSummary;

    use super::{
        DispatchStatus, LocalOsNotifier, NotificationDispatcher, NotificationPayload,
        RecordingNotifier, build_macos_applescript, format_notification_summary,
    };

    fn sample_track() -> TrackSummary {
        TrackSummary {
            id: String::from("track-1"),
            title: String::from("Instant Crush"),
            artists: vec![
                String::from("Daft Punk"),
                String::from("Julian Casablancas"),
            ],
            album: String::from("Random Access Memories"),
            duration_ms: 337_000,
            explicit: false,
            is_liked: true,
            cover: None,
        }
    }

    #[test]
    fn recording_notifier_captures_and_clears_payloads() -> Result<(), String> {
        let recorder = RecordingNotifier::new();
        assert!(recorder.is_empty());

        let track = sample_track();
        let payload =
            NotificationPayload::from_track(&track, Some(PathBuf::from("/tmp/cover.png")));
        let status = recorder.notify(&payload).map_err(|err| err.to_string())?;

        assert_eq!(status, DispatchStatus::Dispatched);
        assert_eq!(recorder.len(), 1);
        assert_eq!(recorder.recorded(), vec![payload]);

        recorder.clear();
        assert_eq!(recorder.recorded(), []);
        Ok(())
    }

    #[test]
    fn local_os_notifier_skips_when_disabled_or_binary_missing() -> Result<(), String> {
        let payload = NotificationPayload::new("Now Playing", "Daft Punk", None);

        let disabled = LocalOsNotifier::new().with_enabled(false);
        assert!(!disabled.is_enabled());
        assert_eq!(
            disabled.notify(&payload).map_err(|err| err.to_string())?,
            DispatchStatus::Skipped
        );

        let missing_bin = LocalOsNotifier::new()
            .with_linux_command("/nonexistent/vynl-notify-send-binary")
            .with_macos_command("/nonexistent/vynl-osascript-binary");
        assert_eq!(
            missing_bin
                .notify(&payload)
                .map_err(|err| err.to_string())?,
            DispatchStatus::Skipped
        );
        Ok(())
    }

    #[test]
    fn escapes_applescript_quotes_and_newlines() {
        let payload = NotificationPayload::new(
            "\"Rock\" \\ Roll",
            "Line 1\nLine 2",
            Some(PathBuf::from("/tmp/art.jpg")),
        );
        let script = build_macos_applescript("vynl", &payload);
        assert_eq!(
            script,
            "display notification \"Line 1 Line 2\" with title \"\\\"Rock\\\" \\\\ Roll\" subtitle \"vynl\""
        );
    }

    #[test]
    fn formats_track_summary_with_and_without_artists() {
        let mut track = sample_track();
        assert_eq!(
            format_notification_summary(&track),
            "Instant Crush — Daft Punk, Julian Casablancas"
        );
        track.artists.clear();
        assert_eq!(format_notification_summary(&track), "Instant Crush");
    }

    #[test]
    fn local_os_notifier_handles_headless_environment_with_and_without_icon() -> Result<(), String>
    {
        let notifier = LocalOsNotifier::new();
        let plain = NotificationPayload::new("Now Playing", "Daft Punk · Discovery", None);
        let with_icon = NotificationPayload::new(
            "Now Playing",
            "Daft Punk · Discovery",
            Some(PathBuf::from("/tmp/vynl-headless-cover.png")),
        );

        let status_plain = notifier.notify(&plain).map_err(|err| err.to_string())?;
        let status_icon = notifier.notify(&with_icon).map_err(|err| err.to_string())?;

        assert!(matches!(
            status_plain,
            DispatchStatus::Dispatched | DispatchStatus::Skipped
        ));
        assert!(matches!(
            status_icon,
            DispatchStatus::Dispatched | DispatchStatus::Skipped
        ));
        Ok(())
    }
}
