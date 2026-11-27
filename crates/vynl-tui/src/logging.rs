//! Lightweight, zero-unsafe structured file logger writing timestamped diagnostic
//! records to `~/.local/state/vynl/vynl.log` without polluting the TUI terminal.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum log file size before truncating on open (`2 MiB`).
pub const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

/// Severity level for diagnostic log entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// Informational lifecycle message.
    Info,
    /// Recoverable warning.
    Warn,
    /// Non-fatal runtime error.
    Error,
}

impl LogLevel {
    /// Returns the uppercase ASCII tag for this level.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

/// Thread-safe append-only file logger backed by `AppPaths::log_file`.
#[derive(Debug)]
pub struct FileLogger {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileLogger {
    /// Creates a new [`FileLogger`] targeting `path`.
    ///
    /// Automatically rotates (truncates) the file if it already exceeds [`MAX_LOG_BYTES`].
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        if let Ok(meta) = fs::metadata(&path)
            && meta.len() > MAX_LOG_BYTES
        {
            let _ = fs::write(&path, b"");
        }
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    /// Returns the destination log file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends a single timestamped line to the log file (best-effort; never panics).
    pub fn log(&self, level: LogLevel, target: &str, message: &str) {
        let Ok(_guard) = self.lock.lock() else {
            return;
        };
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        else {
            return;
        };

        let epoch_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let sanitized = message.replace(['\r', '\n'], " ");
        let _ = writeln!(
            file,
            "[{epoch_secs}] [{}] [{target}] {sanitized}",
            level.as_str()
        );
    }

    /// Convenience helper for [`LogLevel::Info`].
    pub fn info(&self, target: &str, message: &str) {
        self.log(LogLevel::Info, target, message);
    }

    /// Convenience helper for [`LogLevel::Warn`].
    pub fn warn(&self, target: &str, message: &str) {
        self.log(LogLevel::Warn, target, message);
    }

    /// Convenience helper for [`LogLevel::Error`].
    pub fn error(&self, target: &str, message: &str) {
        self.log(LogLevel::Error, target, message);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{FileLogger, LogLevel};

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn writes_sanitized_timestamped_lines_to_disk() {
        let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("vynl-logger-test-{}-{seq}", std::process::id()));
        let path = dir.join("vynl.log");
        let logger = FileLogger::new(&path);

        logger.info("startup", "launched live TUI");
        logger.warn("spotify", "rate limited\nretrying");
        logger.log(LogLevel::Error, "player", "device offline");

        let content = fs::read_to_string(&path);
        assert!(content.is_ok());
        if let Ok(text) = content {
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(lines.len(), 3);
            assert!(lines[0].contains("[INFO] [startup] launched live TUI"));
            assert!(lines[1].contains("[WARN] [spotify] rate limited retrying"));
            assert!(lines[2].contains("[ERROR] [player] device offline"));
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
