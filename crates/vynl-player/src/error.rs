//! Error types for the `vynl-player` audio engine.

use std::fmt;
use std::io;

/// Unified error type returned by audio sinks, stream cache operations,
/// playback coordination, and FFT analysis.
#[derive(Debug)]
pub enum PlayerError {
    /// Underlying filesystem or process pipe I/O error.
    Io(io::Error),
    /// Requested audio backend is unavailable or not compiled into this binary.
    BackendUnavailable(String),
    /// Audio stream format parameters (sample rate or channel count) are invalid.
    InvalidFormat(String),
    /// An external PCM subprocess exited unexpectedly while streaming.
    SubprocessExited {
        /// Command name that was spawned.
        command: String,
        /// Exit status code if the process terminated normally.
        status: Option<i32>,
    },
    /// Stream disk cache operation failed or received invalid configuration.
    CacheError(String),
    /// Playback engine or visualizer received an invalid state or configuration.
    InvalidState(String),
}

impl fmt::Display for PlayerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "audio I/O error: {err}"),
            Self::BackendUnavailable(msg) => write!(f, "audio backend unavailable: {msg}"),
            Self::InvalidFormat(msg) => write!(f, "invalid audio format: {msg}"),
            Self::SubprocessExited { command, status } => match status {
                Some(code) => {
                    write!(f, "audio subprocess `{command}` exited with status {code}")
                }
                None => write!(f, "audio subprocess `{command}` terminated by signal"),
            },
            Self::CacheError(msg) => write!(f, "audio stream cache error: {msg}"),
            Self::InvalidState(msg) => write!(f, "invalid player state: {msg}"),
        }
    }
}

impl std::error::Error for PlayerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for PlayerError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}
