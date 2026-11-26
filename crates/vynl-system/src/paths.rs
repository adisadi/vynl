//! Pure-Rust cross-platform application directory resolver for `vynl`.
//!
//! Honors the XDG Base Directory Specification (`XDG_CONFIG_HOME`, `XDG_CACHE_HOME`,
//! `XDG_STATE_HOME`, `XDG_RUNTIME_DIR`) on Unix platforms and `APPDATA` / `LOCALAPPDATA`
//! on Windows, with clean fallbacks to `HOME` and the OS temporary directory.

use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

const APP_DIR_NAME: &str = "vynl";
const SOCKET_FILE_NAME: &str = "vynl.sock";
const LOG_FILE_NAME: &str = "vynl.log";
const CONFIG_FILE_NAME: &str = "config.toml";
const WINDOWS_NAMED_PIPE: &str = r"\\.\pipe\vynl";

/// Snapshot of environment variables relevant to resolving `vynl` directories.
///
/// Capturing environment variables into an explicit value struct allows deterministic,
/// thread-safe unit testing of Linux, macOS, and Windows path resolution without mutating
/// process-global environment state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvSnapshot {
    /// Value of `XDG_CONFIG_HOME`, if set and non-empty.
    pub xdg_config_home: Option<PathBuf>,
    /// Value of `XDG_CACHE_HOME`, if set and non-empty.
    pub xdg_cache_home: Option<PathBuf>,
    /// Value of `XDG_STATE_HOME`, if set and non-empty.
    pub xdg_state_home: Option<PathBuf>,
    /// Value of `XDG_RUNTIME_DIR`, if set and non-empty.
    pub xdg_runtime_dir: Option<PathBuf>,
    /// Value of `HOME`, if set and non-empty.
    pub home: Option<PathBuf>,
    /// Value of `APPDATA` (Windows roaming config root), if set and non-empty.
    pub appdata: Option<PathBuf>,
    /// Value of `LOCALAPPDATA` (Windows local cache/state root), if set and non-empty.
    pub local_appdata: Option<PathBuf>,
    /// System temporary directory fallback (e.g., `/tmp` or `%TEMP%`).
    pub temp_dir: PathBuf,
}

impl EnvSnapshot {
    /// Captures the relevant environment variables from the current process.
    #[must_use]
    pub fn capture() -> Self {
        Self {
            xdg_config_home: read_non_empty_env("XDG_CONFIG_HOME"),
            xdg_cache_home: read_non_empty_env("XDG_CACHE_HOME"),
            xdg_state_home: read_non_empty_env("XDG_STATE_HOME"),
            xdg_runtime_dir: read_non_empty_env("XDG_RUNTIME_DIR"),
            home: read_non_empty_env("HOME"),
            appdata: read_non_empty_env("APPDATA"),
            local_appdata: read_non_empty_env("LOCALAPPDATA"),
            temp_dir: env::temp_dir(),
        }
    }
}

/// Resolved filesystem locations used by `vynl` at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    /// Root configuration directory (contains `config.toml`).
    pub config_dir: PathBuf,
    /// Directory containing user-defined TOML theme files.
    pub themes_dir: PathBuf,
    /// Root cache directory.
    pub cache_dir: PathBuf,
    /// Cached album artwork and avatar directory.
    pub image_cache_dir: PathBuf,
    /// Cached audio chunk directory.
    pub audio_cache_dir: PathBuf,
    /// Persistent state directory (logs, local playback history, indices).
    pub state_dir: PathBuf,
    /// Default diagnostic log file path.
    pub log_file: PathBuf,
    /// Local IPC endpoint path (Unix domain socket or Windows Named Pipe).
    pub ipc_socket_path: PathBuf,
}

impl AppPaths {
    /// Resolves `AppPaths` from the current process environment and target OS.
    #[must_use]
    pub fn resolve() -> Self {
        Self::from_env(&EnvSnapshot::capture(), cfg!(windows))
    }

    /// Resolves `AppPaths` from an explicit [`EnvSnapshot`] and target platform flag.
    ///
    /// When `is_windows` is `true`, `APPDATA` and `LOCALAPPDATA` are consulted after
    /// explicit `XDG_*` overrides, and `ipc_socket_path` resolves to `\\.\pipe\vynl`.
    /// When `is_windows` is `false`, standard XDG Base Directory rules apply with
    /// `$HOME` and `temp_dir` fallbacks.
    #[must_use]
    pub fn from_env(env: &EnvSnapshot, is_windows: bool) -> Self {
        let config_dir = resolve_config_dir(env, is_windows);
        let themes_dir = config_dir.join("themes");

        let cache_dir = resolve_cache_dir(env, is_windows);
        let image_cache_dir = cache_dir.join("images");
        let audio_cache_dir = cache_dir.join("audio");

        let state_dir = resolve_state_dir(env, is_windows);
        let log_file = state_dir.join(LOG_FILE_NAME);

        let ipc_socket_path = resolve_ipc_socket_path(env, is_windows);

        Self {
            config_dir,
            themes_dir,
            cache_dir,
            image_cache_dir,
            audio_cache_dir,
            state_dir,
            log_file,
            ipc_socket_path,
        }
    }

    /// Returns the path to the primary `config.toml` file inside `config_dir`.
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join(CONFIG_FILE_NAME)
    }

    /// Ensures that all configuration, cache, and state directories exist on disk.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if any directory cannot be created.
    pub fn ensure_dirs(&self) -> io::Result<()> {
        for dir in [
            &self.config_dir,
            &self.themes_dir,
            &self.cache_dir,
            &self.image_cache_dir,
            &self.audio_cache_dir,
            &self.state_dir,
        ] {
            fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

fn read_non_empty_env(key: &str) -> Option<PathBuf> {
    env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn non_empty(path: Option<&PathBuf>) -> Option<&PathBuf> {
    path.filter(|candidate| !candidate.as_os_str().is_empty())
}

fn resolve_config_dir(env: &EnvSnapshot, is_windows: bool) -> PathBuf {
    if let Some(xdg) = non_empty(env.xdg_config_home.as_ref()) {
        return xdg.join(APP_DIR_NAME);
    }
    if is_windows && let Some(appdata) = non_empty(env.appdata.as_ref()) {
        return appdata.join(APP_DIR_NAME);
    }
    if let Some(home) = non_empty(env.home.as_ref()) {
        return home.join(".config").join(APP_DIR_NAME);
    }
    env.temp_dir.join(APP_DIR_NAME).join("config")
}

fn resolve_cache_dir(env: &EnvSnapshot, is_windows: bool) -> PathBuf {
    if let Some(xdg) = non_empty(env.xdg_cache_home.as_ref()) {
        return xdg.join(APP_DIR_NAME);
    }
    if is_windows && let Some(local_appdata) = non_empty(env.local_appdata.as_ref()) {
        return local_appdata.join(APP_DIR_NAME).join("cache");
    }
    if let Some(home) = non_empty(env.home.as_ref()) {
        return home.join(".cache").join(APP_DIR_NAME);
    }
    env.temp_dir.join(APP_DIR_NAME).join("cache")
}

fn resolve_state_dir(env: &EnvSnapshot, is_windows: bool) -> PathBuf {
    if let Some(xdg) = non_empty(env.xdg_state_home.as_ref()) {
        return xdg.join(APP_DIR_NAME);
    }
    if is_windows && let Some(local_appdata) = non_empty(env.local_appdata.as_ref()) {
        return local_appdata.join(APP_DIR_NAME).join("state");
    }
    if let Some(home) = non_empty(env.home.as_ref()) {
        return home.join(".local").join("state").join(APP_DIR_NAME);
    }
    env.temp_dir.join(APP_DIR_NAME).join("state")
}

fn resolve_ipc_socket_path(env: &EnvSnapshot, is_windows: bool) -> PathBuf {
    if is_windows {
        return PathBuf::from(WINDOWS_NAMED_PIPE);
    }
    if let Some(runtime_dir) = non_empty(env.xdg_runtime_dir.as_ref()) {
        return runtime_dir.join(SOCKET_FILE_NAME);
    }
    env.temp_dir.join(SOCKET_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{AppPaths, EnvSnapshot};

    #[test]
    fn honors_all_xdg_overrides_on_unix() {
        let env = EnvSnapshot {
            xdg_config_home: Some(PathBuf::from("/custom/config")),
            xdg_cache_home: Some(PathBuf::from("/custom/cache")),
            xdg_state_home: Some(PathBuf::from("/custom/state")),
            xdg_runtime_dir: Some(PathBuf::from("/run/user/1000")),
            home: Some(PathBuf::from("/home/alice")),
            appdata: None,
            local_appdata: None,
            temp_dir: PathBuf::from("/tmp"),
        };

        let paths = AppPaths::from_env(&env, false);

        assert_eq!(paths.config_dir, PathBuf::from("/custom/config/vynl"));
        assert_eq!(
            paths.themes_dir,
            PathBuf::from("/custom/config/vynl/themes")
        );
        assert_eq!(
            paths.config_file(),
            PathBuf::from("/custom/config/vynl/config.toml")
        );
        assert_eq!(paths.cache_dir, PathBuf::from("/custom/cache/vynl"));
        assert_eq!(
            paths.image_cache_dir,
            PathBuf::from("/custom/cache/vynl/images")
        );
        assert_eq!(
            paths.audio_cache_dir,
            PathBuf::from("/custom/cache/vynl/audio")
        );
        assert_eq!(paths.state_dir, PathBuf::from("/custom/state/vynl"));
        assert_eq!(paths.log_file, PathBuf::from("/custom/state/vynl/vynl.log"));
        assert_eq!(
            paths.ipc_socket_path,
            PathBuf::from("/run/user/1000/vynl.sock")
        );
    }

    #[test]
    fn falls_back_to_home_and_temp_dir_when_xdg_unset_or_empty() {
        let env = EnvSnapshot {
            xdg_config_home: Some(PathBuf::new()),
            xdg_cache_home: None,
            xdg_state_home: Some(PathBuf::new()),
            xdg_runtime_dir: Some(PathBuf::new()),
            home: Some(PathBuf::from("/home/bob")),
            appdata: None,
            local_appdata: None,
            temp_dir: PathBuf::from("/var/tmp"),
        };

        let paths = AppPaths::from_env(&env, false);

        assert_eq!(paths.config_dir, PathBuf::from("/home/bob/.config/vynl"));
        assert_eq!(paths.cache_dir, PathBuf::from("/home/bob/.cache/vynl"));
        assert_eq!(
            paths.state_dir,
            PathBuf::from("/home/bob/.local/state/vynl")
        );
        assert_eq!(paths.ipc_socket_path, PathBuf::from("/var/tmp/vynl.sock"));
    }

    #[test]
    fn resolves_windows_appdata_and_named_pipe() {
        let env = EnvSnapshot {
            xdg_config_home: None,
            xdg_cache_home: None,
            xdg_state_home: None,
            xdg_runtime_dir: None,
            home: Some(PathBuf::from("/Users/carol")),
            appdata: Some(PathBuf::from("C:/Users/carol/AppData/Roaming")),
            local_appdata: Some(PathBuf::from("C:/Users/carol/AppData/Local")),
            temp_dir: PathBuf::from("C:/Temp"),
        };

        let paths = AppPaths::from_env(&env, true);

        assert_eq!(
            paths.config_dir,
            PathBuf::from("C:/Users/carol/AppData/Roaming/vynl")
        );
        assert_eq!(
            paths.themes_dir,
            PathBuf::from("C:/Users/carol/AppData/Roaming/vynl/themes")
        );
        assert_eq!(
            paths.cache_dir,
            PathBuf::from("C:/Users/carol/AppData/Local/vynl/cache")
        );
        assert_eq!(
            paths.image_cache_dir,
            PathBuf::from("C:/Users/carol/AppData/Local/vynl/cache/images")
        );
        assert_eq!(
            paths.audio_cache_dir,
            PathBuf::from("C:/Users/carol/AppData/Local/vynl/cache/audio")
        );
        assert_eq!(
            paths.state_dir,
            PathBuf::from("C:/Users/carol/AppData/Local/vynl/state")
        );
        assert_eq!(
            paths.log_file,
            PathBuf::from("C:/Users/carol/AppData/Local/vynl/state/vynl.log")
        );
        assert_eq!(paths.ipc_socket_path, PathBuf::from(r"\\.\pipe\vynl"));
    }

    #[test]
    fn ensure_dirs_creates_directory_tree() -> Result<(), String> {
        let unique = format!(
            "vynl-paths-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|err| err.to_string())?
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let env = EnvSnapshot {
            xdg_config_home: Some(root.join("cfg")),
            xdg_cache_home: Some(root.join("cache")),
            xdg_state_home: Some(root.join("state")),
            xdg_runtime_dir: Some(root.join("run")),
            home: None,
            appdata: None,
            local_appdata: None,
            temp_dir: root.clone(),
        };

        let paths = AppPaths::from_env(&env, false);
        paths.ensure_dirs().map_err(|err| err.to_string())?;

        assert!(paths.config_dir.is_dir());
        assert!(paths.themes_dir.is_dir());
        assert!(paths.cache_dir.is_dir());
        assert!(paths.image_cache_dir.is_dir());
        assert!(paths.audio_cache_dir.is_dir());
        assert!(paths.state_dir.is_dir());

        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }
}
