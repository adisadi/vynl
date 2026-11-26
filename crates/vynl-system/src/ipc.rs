//! Local IPC wire protocol, Unix Domain Socket server, and client for `vynl`.
//!
//! Enables short-lived CLI invocations (`vynl status --json`, `vynl play-pause`,
//! `vynl next`, `vynl volume +5`, `vynl open spotify:track:...`) and status bars
//! (Waybar, Polybar, tmux, `SketchyBar`) to query and control a running `vynl`
//! session using line-delimited JSON frames over a local socket.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use vynl_core::model::{PlaybackState, format_duration_ms};

use crate::paths::AppPaths;

/// Default read/write timeout applied by [`IpcClient`] and server connections.
pub const DEFAULT_IPC_TIMEOUT: Duration = Duration::from_secs(2);

/// Maximum size in bytes of a single line-delimited IPC frame (`64 KiB`).
#[cfg(any(unix, test))]
const MAX_FRAME_BYTES: usize = 64 * 1024;

/// Default local IPC endpoint description for diagnostics.
#[must_use]
pub const fn ipc_transport_name() -> &'static str {
    if cfg!(windows) {
        "Windows Named Pipe"
    } else {
        "Unix Domain Socket"
    }
}

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

impl IpcCommand {
    /// Encodes the command as a single-line JSON wire frame (without trailing newline).
    #[must_use]
    pub fn encode_wire(&self) -> String {
        match self {
            Self::Status { json } => format!(r#"{{"cmd":"status","json":{json}}}"#),
            Self::PlayPause => String::from(r#"{"cmd":"play_pause"}"#),
            Self::Next => String::from(r#"{"cmd":"next"}"#),
            Self::Prev => String::from(r#"{"cmd":"prev"}"#),
            Self::VolumeDelta(delta) => format!(r#"{{"cmd":"volume_delta","delta":{delta}}}"#),
            Self::OpenUri(uri) => {
                let mut out = String::from(r#"{"cmd":"open_uri","uri":"#);
                json::push_escaped_str(&mut out, uri);
                out.push('}');
                out
            }
        }
    }

    /// Decodes an [`IpcCommand`] from a line-delimited JSON wire frame.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::Protocol`] if the input is not valid JSON or lacks required
    /// command fields, or [`IpcError::InvalidCommand`] if the `cmd` discriminator is unknown.
    pub fn decode_wire(line: &str) -> Result<Self, IpcError> {
        let value = json::parse(line)?;
        let fields = value.as_object()?;
        let cmd = json::require_str(fields, "cmd")?;

        match cmd {
            "status" => {
                let json_flag = json::optional_bool(fields, "json")?.unwrap_or(false);
                Ok(Self::Status { json: json_flag })
            }
            "play_pause" => Ok(Self::PlayPause),
            "next" => Ok(Self::Next),
            "prev" => Ok(Self::Prev),
            "volume_delta" => {
                let raw = json::require_int(fields, "delta")?;
                let delta = i8::try_from(raw)
                    .map_err(|_| IpcError::Protocol(format!("volume delta out of range: {raw}")))?;
                Ok(Self::VolumeDelta(delta))
            }
            "open_uri" => {
                let uri = json::require_str(fields, "uri")?;
                Ok(Self::OpenUri(uri.to_owned()))
            }
            other => Err(IpcError::InvalidCommand(other.to_owned())),
        }
    }
}

/// Snapshot of active playback state suitable for Waybar, Polybar, tmux, and `SketchyBar`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatusSnapshot {
    /// Whether audio is currently playing.
    pub is_playing: bool,
    /// Current track title (empty if stopped).
    pub title: String,
    /// Joined artist display string (empty if stopped).
    pub artist: String,
    /// Album title (empty if stopped).
    pub album: String,
    /// Current playback position in milliseconds.
    pub position_ms: u64,
    /// Total track duration in milliseconds.
    pub duration_ms: u64,
    /// Volume percentage (`0..=100`).
    pub volume_percent: u8,
    /// Active Spotify Connect device name (empty if none).
    pub device_name: String,
}

impl StatusSnapshot {
    /// Constructs a [`StatusSnapshot`] from a core [`PlaybackState`].
    #[must_use]
    pub fn from_playback_state(state: &PlaybackState) -> Self {
        let (title, artist, album, duration_ms) = state.current_track.as_ref().map_or_else(
            || (String::new(), String::new(), String::new(), 0),
            |track| {
                (
                    track.title.clone(),
                    track.artist_line(),
                    track.album.clone(),
                    track.duration_ms,
                )
            },
        );

        let device_name = state
            .active_device
            .as_ref()
            .map_or_else(String::new, |device| device.name.clone());

        Self {
            is_playing: state.is_playing,
            title,
            artist,
            album,
            position_ms: state.position_ms,
            duration_ms,
            volume_percent: if state.muted { 0 } else { state.volume_percent },
            device_name,
        }
    }

    /// Serializes this snapshot as compact single-line JSON for status bars.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(192);
        self.write_json_into(&mut out);
        out
    }

    /// Parses a [`StatusSnapshot`] from a JSON object string.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::Protocol`] if the JSON is malformed or missing required fields.
    pub fn from_json(input: &str) -> Result<Self, IpcError> {
        let value = json::parse(input)?;
        Self::from_json_value(&value)
    }

    /// Formats a compact human-readable one-line playback summary.
    #[must_use]
    pub fn format_human(&self) -> String {
        if self.title.is_empty() {
            return String::from("Stopped");
        }
        let icon = if self.is_playing { "▶" } else { "⏸" };
        let pos = format_duration_ms(self.position_ms);
        let dur = format_duration_ms(self.duration_ms);
        if self.artist.is_empty() {
            format!("{icon} {} ({pos} / {dur})", self.title)
        } else {
            format!("{icon} {} — {} ({pos} / {dur})", self.title, self.artist)
        }
    }

    fn write_json_into(&self, out: &mut String) {
        use std::fmt::Write as _;

        let _ = write!(out, r#"{{"is_playing":{},"title":"#, self.is_playing);
        json::push_escaped_str(out, &self.title);
        out.push_str(r#","artist":"#);
        json::push_escaped_str(out, &self.artist);
        out.push_str(r#","album":"#);
        json::push_escaped_str(out, &self.album);
        let _ = write!(
            out,
            r#","position_ms":{},"duration_ms":{},"volume_percent":{},"device_name":"#,
            self.position_ms, self.duration_ms, self.volume_percent
        );
        json::push_escaped_str(out, &self.device_name);
        out.push('}');
    }

    fn from_json_value(value: &json::JsonValue) -> Result<Self, IpcError> {
        let fields = value.as_object()?;
        let is_playing = json::require_bool(fields, "is_playing")?;
        let title = json::require_str(fields, "title")?.to_owned();
        let artist = json::require_str(fields, "artist")?.to_owned();
        let album = json::require_str(fields, "album")?.to_owned();
        let position_ms = json::require_u64(fields, "position_ms")?;
        let duration_ms = json::require_u64(fields, "duration_ms")?;
        let volume_percent = json::require_u8(fields, "volume_percent")?;
        let device_name = json::require_str(fields, "device_name")?.to_owned();

        Ok(Self {
            is_playing,
            title,
            artist,
            album,
            position_ms,
            duration_ms,
            volume_percent,
            device_name,
        })
    }
}

/// Wire response returned by [`IpcServer`] for each processed [`IpcCommand`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpcResponse {
    /// Whether the command succeeded.
    pub ok: bool,
    /// Human-readable status or error message.
    pub message: String,
    /// Optional playback status snapshot (populated for `Status` commands).
    pub status: Option<StatusSnapshot>,
}

impl IpcResponse {
    /// Creates a successful response without a status snapshot.
    #[must_use]
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
            status: None,
        }
    }

    /// Creates a successful response carrying a [`StatusSnapshot`].
    #[must_use]
    pub fn with_status(message: impl Into<String>, status: StatusSnapshot) -> Self {
        Self {
            ok: true,
            message: message.into(),
            status: Some(status),
        }
    }

    /// Creates an error response without a status snapshot.
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
            status: None,
        }
    }

    /// Serializes this response as a single-line JSON wire frame (without trailing newline).
    #[must_use]
    pub fn to_json(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::with_capacity(256);
        let _ = write!(out, r#"{{"ok":{},"message":"#, self.ok);
        json::push_escaped_str(&mut out, &self.message);
        out.push_str(r#","status":"#);
        match &self.status {
            Some(status) => status.write_json_into(&mut out),
            None => out.push_str("null"),
        }
        out.push('}');
        out
    }

    /// Parses an [`IpcResponse`] from a line-delimited JSON wire frame.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::Protocol`] if the JSON frame is malformed or missing required fields.
    pub fn from_json(input: &str) -> Result<Self, IpcError> {
        let value = json::parse(input)?;
        let fields = value.as_object()?;
        let ok = json::require_bool(fields, "ok")?;
        let message = json::require_str(fields, "message")?.to_owned();
        let status = match json::find_field(fields, "status") {
            None | Some(json::JsonValue::Null) => None,
            Some(status_val) => Some(StatusSnapshot::from_json_value(status_val)?),
        };

        Ok(Self {
            ok,
            message,
            status,
        })
    }

    /// Formats the response for CLI stdout (`vynl status --json` or human-readable output).
    #[must_use]
    pub fn format_cli_output(&self, json_mode: bool) -> String {
        if json_mode {
            self.status
                .as_ref()
                .map_or_else(|| self.to_json(), StatusSnapshot::to_json)
        } else {
            self.status
                .as_ref()
                .map_or_else(|| self.message.clone(), StatusSnapshot::format_human)
        }
    }
}

/// Errors produced by local IPC serialization, binding, or transport operations.
#[derive(Debug)]
pub enum IpcError {
    /// Underlying operating-system I/O error.
    Io(io::Error),
    /// Wire framing or JSON decoding error.
    Protocol(String),
    /// Unrecognized IPC command discriminator.
    InvalidCommand(String),
    /// Local IPC server/client transport is not yet implemented for this OS target.
    UnsupportedPlatform,
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "IPC I/O error: {err}"),
            Self::Protocol(msg) => write!(f, "IPC protocol error: {msg}"),
            Self::InvalidCommand(cmd) => write!(f, "unknown IPC command: {cmd}"),
            Self::UnsupportedPlatform => {
                write!(
                    f,
                    "local IPC transport ({}) is not implemented on this platform",
                    ipc_transport_name()
                )
            }
        }
    }
}

impl std::error::Error for IpcError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Protocol(_) | Self::InvalidCommand(_) | Self::UnsupportedPlatform => None,
        }
    }
}

impl From<io::Error> for IpcError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Local IPC server listening for incoming [`IpcCommand`] frames.
///
/// On Unix platforms, binds a `UnixListener` at the configured path, removes stale
/// socket files left behind by crashed processes, dispatches incoming commands on a
/// dedicated background worker thread, and unlinks the socket cleanly on [`Drop`].
/// On non-Unix platforms, compiles cleanly as a stub returning
/// [`IpcError::UnsupportedPlatform`].
pub struct IpcServer {
    socket_path: PathBuf,
    #[cfg(unix)]
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(unix)]
    worker: Option<std::thread::JoinHandle<()>>,
}

impl fmt::Debug for IpcServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IpcServer")
            .field("socket_path", &self.socket_path)
            .finish_non_exhaustive()
    }
}

impl IpcServer {
    /// Binds a local IPC server at `socket_path` and starts serving incoming commands.
    ///
    /// On Unix, if `socket_path` already exists:
    /// - If another process is actively listening on it, returns `AddrInUse`.
    /// - Otherwise treats it as a stale socket file and removes it before binding.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::Io`] if directory creation, socket binding, or worker thread
    /// spawning fails, or [`IpcError::UnsupportedPlatform`] on non-Unix targets.
    #[cfg(unix)]
    pub fn bind<F>(socket_path: impl Into<PathBuf>, handler: F) -> Result<Self, IpcError>
    where
        F: Fn(IpcCommand) -> IpcResponse + Send + Sync + 'static,
    {
        use std::fs;
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let socket_path = socket_path.into();
        if let Some(parent) = socket_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }

        if socket_path.exists() {
            if UnixStream::connect(&socket_path).is_ok() {
                return Err(IpcError::Io(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!(
                        "another vynl IPC server is already listening on {}",
                        socket_path.display()
                    ),
                )));
            }
            fs::remove_file(&socket_path)?;
        }

        let listener = UnixListener::bind(&socket_path)?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);

        let worker = std::thread::Builder::new()
            .name(String::from("vynl-ipc-server"))
            .spawn(move || {
                while !worker_shutdown.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _addr)) => {
                            if worker_shutdown.load(Ordering::Acquire) {
                                break;
                            }
                            handle_unix_connection(stream, &handler);
                        }
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                        Err(_) => {
                            if worker_shutdown.load(Ordering::Acquire) {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(25));
                        }
                    }
                }
            })?;

        Ok(Self {
            socket_path,
            shutdown,
            worker: Some(worker),
        })
    }

    /// Binds a local IPC server at `socket_path` (stub returning unsupported on non-Unix targets).
    ///
    /// # Errors
    ///
    /// Always returns [`IpcError::UnsupportedPlatform`] on non-Unix platforms.
    #[cfg(not(unix))]
    pub fn bind<F>(socket_path: impl Into<PathBuf>, _handler: F) -> Result<Self, IpcError>
    where
        F: Fn(IpcCommand) -> IpcResponse + Send + Sync + 'static,
    {
        let _ = socket_path.into();
        Err(IpcError::UnsupportedPlatform)
    }

    /// Returns the filesystem path bound by this server.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

#[cfg(unix)]
impl Drop for IpcServer {
    fn drop(&mut self) {
        use std::fs;
        use std::os::unix::net::UnixStream;
        use std::sync::atomic::Ordering;

        self.shutdown.store(true, Ordering::Release);
        // Unblock `UnixListener::accept()` with a short local connection probe.
        let _ = UnixStream::connect(&self.socket_path);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
        let _ = fs::remove_file(&self.socket_path);
    }
}

#[cfg(unix)]
fn handle_unix_connection<F>(mut stream: std::os::unix::net::UnixStream, handler: &F)
where
    F: Fn(IpcCommand) -> IpcResponse,
{
    use std::io::Write as _;

    let _ = stream.set_read_timeout(Some(DEFAULT_IPC_TIMEOUT));
    let _ = stream.set_write_timeout(Some(DEFAULT_IPC_TIMEOUT));

    let line = match read_bounded_line(&mut stream, MAX_FRAME_BYTES) {
        Ok(line) if line.is_empty() => return,
        Ok(line) => line,
        Err(err) => {
            let response = IpcResponse::error(err.to_string());
            let _ = writeln!(stream, "{}", response.to_json());
            let _ = stream.flush();
            return;
        }
    };

    let response = match IpcCommand::decode_wire(&line) {
        Ok(command) => handler(command),
        Err(err) => IpcResponse::error(err.to_string()),
    };

    let _ = writeln!(stream, "{}", response.to_json());
    let _ = stream.flush();
}

/// Short-lived IPC client used by CLI commands to talk to a running [`IpcServer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpcClient {
    socket_path: PathBuf,
    timeout: Duration,
}

impl IpcClient {
    /// Creates an [`IpcClient`] targeting `socket_path` with [`DEFAULT_IPC_TIMEOUT`].
    #[must_use]
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            timeout: DEFAULT_IPC_TIMEOUT,
        }
    }

    /// Creates an [`IpcClient`] targeting `socket_path` with a custom I/O timeout.
    #[must_use]
    pub fn with_timeout(socket_path: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            socket_path: socket_path.into(),
            timeout,
        }
    }

    /// Creates an [`IpcClient`] targeting the default resolved [`AppPaths::ipc_socket_path`].
    #[must_use]
    pub fn default_client() -> Self {
        Self::new(AppPaths::resolve().ipc_socket_path)
    }

    /// Returns the socket path targeted by this client.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Returns the configured read/write timeout.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Sends a single [`IpcCommand`] to the running server and parses its [`IpcResponse`].
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::Io`] if connecting, writing, or reading times out or fails,
    /// [`IpcError::Protocol`] if the server returns malformed JSON, or
    /// [`IpcError::UnsupportedPlatform`] on non-Unix targets.
    #[cfg(unix)]
    pub fn send(&self, command: &IpcCommand) -> Result<IpcResponse, IpcError> {
        use std::io::Write as _;
        use std::os::unix::net::UnixStream;

        let mut stream = UnixStream::connect(&self.socket_path)?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;

        let wire = command.encode_wire();
        stream.write_all(wire.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;

        let response_line = read_bounded_line(&mut stream, MAX_FRAME_BYTES)?;
        if response_line.is_empty() {
            return Err(IpcError::Protocol(String::from(
                "IPC server closed connection without a response",
            )));
        }
        IpcResponse::from_json(&response_line)
    }

    /// Sends a single [`IpcCommand`] (stub returning unsupported on non-Unix targets).
    ///
    /// # Errors
    ///
    /// Always returns [`IpcError::UnsupportedPlatform`] on non-Unix platforms.
    #[cfg(not(unix))]
    pub fn send(&self, _command: &IpcCommand) -> Result<IpcResponse, IpcError> {
        Err(IpcError::UnsupportedPlatform)
    }
}

#[cfg(any(unix, test))]
fn read_bounded_line(reader: &mut impl io::Read, max_bytes: usize) -> Result<String, IpcError> {
    use std::io::{BufRead as _, BufReader, Read as _};

    let limit = u64::try_from(max_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut limited = BufReader::new(reader.take(limit));
    let mut buf = Vec::new();
    limited.read_until(b'\n', &mut buf)?;

    if buf.len() > max_bytes {
        return Err(IpcError::Protocol(format!(
            "IPC frame exceeded maximum size of {max_bytes} bytes"
        )));
    }

    while matches!(buf.last(), Some(b'\n' | b'\r')) {
        buf.pop();
    }

    String::from_utf8(buf)
        .map_err(|err| IpcError::Protocol(format!("IPC frame is not valid UTF-8: {err}")))
}

/// Minimal pure-Rust RFC 8259 JSON encoder/decoder scoped to IPC frames.
mod json {
    use super::IpcError;

    const MAX_DEPTH: usize = 8;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(super) enum JsonValue {
        Null,
        Bool(bool),
        Int(i128),
        Str(String),
        Object(Vec<(String, Self)>),
    }

    impl JsonValue {
        pub(super) fn as_object(&self) -> Result<&[(String, Self)], IpcError> {
            match self {
                Self::Object(entries) => Ok(entries),
                _ => Err(IpcError::Protocol(String::from("expected JSON object"))),
            }
        }
    }

    pub(super) fn push_escaped_str(out: &mut String, input: &str) {
        use std::fmt::Write as _;

        out.push('"');
        for ch in input.chars() {
            match ch {
                '"' => out.push_str(r#"\""#),
                '\\' => out.push_str(r"\\"),
                '\x08' => out.push_str(r"\b"),
                '\x0c' => out.push_str(r"\f"),
                '\n' => out.push_str(r"\n"),
                '\r' => out.push_str(r"\r"),
                '\t' => out.push_str(r"\t"),
                c if (c as u32) < 0x20 => {
                    let _ = write!(out, "\\u{:04x}", c as u32);
                }
                c => out.push(c),
            }
        }
        out.push('"');
    }

    pub(super) fn find_field<'a>(
        fields: &'a [(String, JsonValue)],
        key: &str,
    ) -> Option<&'a JsonValue> {
        fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub(super) fn require_str<'a>(
        fields: &'a [(String, JsonValue)],
        key: &str,
    ) -> Result<&'a str, IpcError> {
        match find_field(fields, key) {
            Some(JsonValue::Str(value)) => Ok(value.as_str()),
            Some(_) => Err(IpcError::Protocol(format!(
                "field '{key}' must be a JSON string"
            ))),
            None => Err(IpcError::Protocol(format!(
                "missing required field '{key}'"
            ))),
        }
    }

    pub(super) fn require_bool(
        fields: &[(String, JsonValue)],
        key: &str,
    ) -> Result<bool, IpcError> {
        match find_field(fields, key) {
            Some(JsonValue::Bool(value)) => Ok(*value),
            Some(_) => Err(IpcError::Protocol(format!(
                "field '{key}' must be a JSON boolean"
            ))),
            None => Err(IpcError::Protocol(format!(
                "missing required field '{key}'"
            ))),
        }
    }

    pub(super) fn optional_bool(
        fields: &[(String, JsonValue)],
        key: &str,
    ) -> Result<Option<bool>, IpcError> {
        match find_field(fields, key) {
            None | Some(JsonValue::Null) => Ok(None),
            Some(JsonValue::Bool(value)) => Ok(Some(*value)),
            Some(_) => Err(IpcError::Protocol(format!(
                "field '{key}' must be a JSON boolean"
            ))),
        }
    }

    pub(super) fn require_int(fields: &[(String, JsonValue)], key: &str) -> Result<i128, IpcError> {
        match find_field(fields, key) {
            Some(JsonValue::Int(value)) => Ok(*value),
            Some(_) => Err(IpcError::Protocol(format!(
                "field '{key}' must be a JSON integer"
            ))),
            None => Err(IpcError::Protocol(format!(
                "missing required field '{key}'"
            ))),
        }
    }

    pub(super) fn require_u64(fields: &[(String, JsonValue)], key: &str) -> Result<u64, IpcError> {
        let value = require_int(fields, key)?;
        u64::try_from(value)
            .map_err(|_| IpcError::Protocol(format!("field '{key}' out of u64 range: {value}")))
    }

    pub(super) fn require_u8(fields: &[(String, JsonValue)], key: &str) -> Result<u8, IpcError> {
        let value = require_int(fields, key)?;
        u8::try_from(value)
            .map_err(|_| IpcError::Protocol(format!("field '{key}' out of u8 range: {value}")))
    }

    pub(super) fn parse(input: &str) -> Result<JsonValue, IpcError> {
        let mut parser = Parser {
            bytes: input.as_bytes(),
            pos: 0,
        };
        let value = parser.parse_value(0)?;
        parser.skip_ws();
        if parser.pos != parser.bytes.len() {
            return Err(IpcError::Protocol(String::from(
                "unexpected trailing tokens after JSON value",
            )));
        }
        Ok(value)
    }

    struct Parser<'a> {
        bytes: &'a [u8],
        pos: usize,
    }

    impl Parser<'_> {
        fn skip_ws(&mut self) {
            while let Some(&b) = self.bytes.get(self.pos) {
                if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }

        fn peek(&self) -> Option<u8> {
            self.bytes.get(self.pos).copied()
        }

        fn next_byte(&mut self) -> Result<u8, IpcError> {
            let Some(&byte) = self.bytes.get(self.pos) else {
                return Err(IpcError::Protocol(String::from("unexpected end of JSON")));
            };
            self.pos += 1;
            Ok(byte)
        }

        fn consume_literal(&mut self, literal: &[u8]) -> Result<(), IpcError> {
            let end = self.pos.saturating_add(literal.len());
            if self.bytes.get(self.pos..end) == Some(literal) {
                self.pos = end;
                Ok(())
            } else {
                Err(IpcError::Protocol(String::from("invalid JSON literal")))
            }
        }

        fn parse_value(&mut self, depth: usize) -> Result<JsonValue, IpcError> {
            if depth > MAX_DEPTH {
                return Err(IpcError::Protocol(String::from(
                    "JSON nesting depth exceeded",
                )));
            }
            self.skip_ws();
            match self.peek() {
                Some(b'{') => self.parse_object(depth + 1),
                Some(b'"') => self.parse_string().map(JsonValue::Str),
                Some(b't') => {
                    self.consume_literal(b"true")?;
                    Ok(JsonValue::Bool(true))
                }
                Some(b'f') => {
                    self.consume_literal(b"false")?;
                    Ok(JsonValue::Bool(false))
                }
                Some(b'n') => {
                    self.consume_literal(b"null")?;
                    Ok(JsonValue::Null)
                }
                Some(b'-' | b'0'..=b'9') => self.parse_int().map(JsonValue::Int),
                Some(_) => Err(IpcError::Protocol(String::from(
                    "unexpected token in JSON value",
                ))),
                None => Err(IpcError::Protocol(String::from("empty JSON input"))),
            }
        }

        fn parse_object(&mut self, depth: usize) -> Result<JsonValue, IpcError> {
            let _ = self.next_byte()?; // '{'
            self.skip_ws();

            let mut entries = Vec::new();
            if self.peek() == Some(b'}') {
                self.pos += 1;
                return Ok(JsonValue::Object(entries));
            }

            loop {
                self.skip_ws();
                let key = self.parse_string()?;
                self.skip_ws();
                if self.next_byte()? != b':' {
                    return Err(IpcError::Protocol(String::from(
                        "expected ':' after JSON object key",
                    )));
                }
                let value = self.parse_value(depth)?;
                entries.push((key, value));

                self.skip_ws();
                match self.next_byte()? {
                    b',' => {}
                    b'}' => return Ok(JsonValue::Object(entries)),
                    _ => {
                        return Err(IpcError::Protocol(String::from(
                            "expected ',' or '}' in JSON object",
                        )));
                    }
                }
            }
        }

        fn parse_string(&mut self) -> Result<String, IpcError> {
            if self.next_byte()? != b'"' {
                return Err(IpcError::Protocol(String::from(
                    "expected '\"' to start JSON string",
                )));
            }

            let mut out = String::new();
            loop {
                let start = self.pos;
                while let Some(&b) = self.bytes.get(self.pos) {
                    if b == b'"' || b == b'\\' {
                        break;
                    }
                    if b < 0x20 {
                        return Err(IpcError::Protocol(String::from(
                            "unescaped control character in JSON string",
                        )));
                    }
                    self.pos += 1;
                }

                if self.pos > start {
                    let slice = self
                        .bytes
                        .get(start..self.pos)
                        .ok_or_else(|| IpcError::Protocol(String::from("slice out of bounds")))?;
                    let chunk = std::str::from_utf8(slice).map_err(|_| {
                        IpcError::Protocol(String::from("invalid UTF-8 inside JSON string"))
                    })?;
                    out.push_str(chunk);
                }

                match self.next_byte()? {
                    b'"' => return Ok(out),
                    b'\\' => self.parse_escape(&mut out)?,
                    _ => {
                        return Err(IpcError::Protocol(String::from("unterminated JSON string")));
                    }
                }
            }
        }

        fn parse_escape(&mut self, out: &mut String) -> Result<(), IpcError> {
            match self.next_byte()? {
                b'"' => out.push('"'),
                b'\\' => out.push('\\'),
                b'/' => out.push('/'),
                b'b' => out.push('\x08'),
                b'f' => out.push('\x0c'),
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b't' => out.push('\t'),
                b'u' => {
                    let first = self.parse_hex4()?;
                    let code_point = if (0xD800..=0xDBFF).contains(&first) {
                        if self.next_byte()? != b'\\' || self.next_byte()? != b'u' {
                            return Err(IpcError::Protocol(String::from(
                                "incomplete UTF-16 surrogate pair in JSON string",
                            )));
                        }
                        let second = self.parse_hex4()?;
                        if !(0xDC00..=0xDFFF).contains(&second) {
                            return Err(IpcError::Protocol(String::from(
                                "invalid low UTF-16 surrogate in JSON string",
                            )));
                        }
                        0x10000 + ((u32::from(first) - 0xD800) << 10) + (u32::from(second) - 0xDC00)
                    } else {
                        u32::from(first)
                    };
                    let decoded = char::from_u32(code_point).ok_or_else(|| {
                        IpcError::Protocol(String::from("invalid Unicode escape in JSON string"))
                    })?;
                    out.push(decoded);
                }
                _ => {
                    return Err(IpcError::Protocol(String::from(
                        "invalid escape sequence in JSON string",
                    )));
                }
            }
            Ok(())
        }

        fn parse_hex4(&mut self) -> Result<u16, IpcError> {
            let mut value: u16 = 0;
            for _ in 0..4 {
                let nibble = match self.next_byte()? {
                    b @ b'0'..=b'9' => u16::from(b - b'0'),
                    b @ b'a'..=b'f' => u16::from(b - b'a' + 10),
                    b @ b'A'..=b'F' => u16::from(b - b'A' + 10),
                    _ => {
                        return Err(IpcError::Protocol(String::from(
                            "invalid hex digit in \\u escape",
                        )));
                    }
                };
                value = (value << 4) | nibble;
            }
            Ok(value)
        }

        fn parse_int(&mut self) -> Result<i128, IpcError> {
            let start = self.pos;
            if self.peek() == Some(b'-') {
                self.pos += 1;
            }
            let digits_start = self.pos;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
            if self.pos == digits_start {
                return Err(IpcError::Protocol(String::from("invalid JSON integer")));
            }
            let slice = self
                .bytes
                .get(start..self.pos)
                .ok_or_else(|| IpcError::Protocol(String::from("integer slice out of bounds")))?;
            let text = std::str::from_utf8(slice)
                .map_err(|_| IpcError::Protocol(String::from("invalid UTF-8 in integer")))?;
            text.parse::<i128>()
                .map_err(|_| IpcError::Protocol(format!("invalid JSON integer: {text}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use vynl_core::model::{ConnectDevice, PlaybackState, TrackSummary};

    use super::{IpcCommand, IpcResponse, StatusSnapshot, ipc_transport_name, read_bounded_line};

    #[test]
    fn round_trips_all_ipc_commands_on_wire() -> Result<(), String> {
        let commands = [
            IpcCommand::Status { json: true },
            IpcCommand::Status { json: false },
            IpcCommand::PlayPause,
            IpcCommand::Next,
            IpcCommand::Prev,
            IpcCommand::VolumeDelta(-15),
            IpcCommand::VolumeDelta(25),
            IpcCommand::OpenUri(String::from(
                "spotify:track:4cOdK2wGLETKBW3PvgPWqT?quote=\"yes\"\\slash",
            )),
        ];

        for cmd in commands {
            let encoded = cmd.encode_wire();
            assert!(!encoded.contains('\n'));
            let decoded = IpcCommand::decode_wire(&encoded).map_err(|err| err.to_string())?;
            assert_eq!(decoded, cmd);
        }
        Ok(())
    }

    #[test]
    fn round_trips_status_snapshot_and_response_json() -> Result<(), String> {
        let playback = PlaybackState {
            current_track: Some(TrackSummary {
                id: String::from("1"),
                title: String::from("Veridis Quo \"Live\""),
                artists: vec![String::from("Daft Punk")],
                album: String::from("Discovery"),
                duration_ms: 345_000,
                explicit: false,
                is_liked: true,
                cover: None,
            }),
            is_playing: true,
            position_ms: 91_000,
            volume_percent: 82,
            muted: false,
            active_device: Some(ConnectDevice {
                id: String::from("dev-1"),
                name: String::from("vynl-workstation"),
                device_type: String::from("Computer"),
                is_active: true,
                volume_percent: 82,
            }),
            ..PlaybackState::default()
        };

        let snapshot = StatusSnapshot::from_playback_state(&playback);
        let status_json = snapshot.to_json();
        assert_eq!(
            StatusSnapshot::from_json(&status_json).map_err(|err| err.to_string())?,
            snapshot
        );
        assert_eq!(
            snapshot.format_human(),
            "▶ Veridis Quo \"Live\" — Daft Punk (01:31 / 05:45)"
        );

        let response = IpcResponse::with_status("ok", snapshot.clone());
        let response_json = response.to_json();
        let parsed = IpcResponse::from_json(&response_json).map_err(|err| err.to_string())?;
        assert_eq!(parsed, response);
        assert_eq!(response.format_cli_output(true), status_json);
        assert_eq!(response.format_cli_output(false), snapshot.format_human());
        assert_ne!(ipc_transport_name(), "");
        Ok(())
    }

    #[test]
    fn rejects_oversized_ipc_frames() {
        let oversized = vec![b'a'; 128];
        let mut cursor = Cursor::new(oversized);
        let err = read_bounded_line(&mut cursor, 64);
        assert!(err.is_err());
    }
}
