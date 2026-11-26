//! Disk persistence for `config.toml`, custom `~/.config/vynl/themes/*.toml`,
//! and OS-keyring-backed OAuth tokens (`#[forbid(unsafe_code)]`).

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use vynl_core::config::AppConfig;
use vynl_core::theme::Theme;
use vynl_spotify::pkce::TokenBundle;
use vynl_system::paths::AppPaths;
use vynl_system::secrets::{LocalSecretStore, SecretStore};

/// Account key used to store Spotify OAuth tokens in the OS keyring / fallback file.
pub const SPOTIFY_TOKEN_ACCOUNT: &str = "spotify-oauth-tokens";

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Loads [`AppConfig`] from `paths.config_file()`.
///
/// Returns [`AppConfig::default()`] when the file does not exist yet.
///
/// # Errors
/// Returns a human-readable error if the file exists but cannot be read or parsed.
pub fn load_config(paths: &AppPaths) -> Result<AppConfig, String> {
    let path = paths.config_file();
    match fs::read_to_string(&path) {
        Ok(raw) => AppConfig::from_toml(&raw)
            .map_err(|err| format!("failed to parse {}: {err}", path.display())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(AppConfig::default()),
        Err(err) => Err(format!("failed to read {}: {err}", path.display())),
    }
}

/// Atomically writes [`AppConfig`] to `paths.config_file()`, creating parent directories
/// as needed.
///
/// # Errors
/// Returns a human-readable error if directories or files cannot be created or written.
pub fn save_config(paths: &AppPaths, config: &AppConfig) -> Result<(), String> {
    paths
        .ensure_dirs()
        .map_err(|err| format!("failed to create config directories: {err}"))?;
    let target = paths.config_file();
    atomic_write_string(&target, &config.to_toml())
}

/// Loads a theme by `name`, checking `~/.config/vynl/themes/<name>.toml` first
/// (so custom themes and user overrides take precedence) and falling back to
/// built-in presets.
///
/// # Errors
/// Returns an error if no custom theme file or built-in theme matches `name`,
/// or if a matching custom theme file contains invalid TOML.
pub fn load_theme(paths: &AppPaths, name: &str) -> Result<Theme, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(String::from("theme name cannot be empty"));
    }

    if is_safe_theme_name(trimmed) {
        let custom_path = paths.themes_dir.join(format!("{trimmed}.toml"));
        match fs::read_to_string(&custom_path) {
            Ok(raw) => {
                return Theme::from_toml(trimmed, &raw).map_err(|err| {
                    format!("failed to parse theme {}: {err}", custom_path.display())
                });
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(format!(
                    "failed to read theme {}: {err}",
                    custom_path.display()
                ));
            }
        }
    }

    Theme::builtin(trimmed).ok_or_else(|| format!("unknown theme `{trimmed}`"))
}

/// Lists all available theme names (built-in presets plus custom `*.toml` stems in
/// `paths.themes_dir`), deduplicated while keeping built-ins first.
#[must_use]
pub fn list_available_themes(paths: &AppPaths) -> Vec<String> {
    let mut names: Vec<String> = Theme::builtin_names()
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let mut seen: BTreeSet<String> = names.iter().cloned().collect();

    for custom in discover_custom_themes(paths) {
        if seen.insert(custom.clone()) {
            names.push(custom);
        }
    }
    names
}

/// Returns custom theme names discovered from `*.toml` files inside `paths.themes_dir`,
/// sorted alphabetically and excluding names that duplicate built-in presets.
#[must_use]
pub fn discover_custom_themes(paths: &AppPaths) -> Vec<String> {
    let mut custom = BTreeSet::new();
    let Ok(entries) = fs::read_dir(&paths.themes_dir) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("toml")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            && is_safe_theme_name(stem)
        {
            custom.insert(stem.to_owned());
        }
    }
    custom.into_iter().collect()
}

/// Loads stored Spotify OAuth tokens using [`LocalSecretStore`] (OS keyring with
/// `0600` file fallback at `paths.tokens_file()`).
///
/// # Errors
/// Returns an error if the secret backend fails or the stored payload is malformed.
pub fn load_tokens(paths: &AppPaths) -> Result<Option<TokenBundle>, String> {
    let store = LocalSecretStore::new(paths.tokens_file());
    let Some(raw) = store
        .get_secret(SPOTIFY_TOKEN_ACCOUNT)
        .map_err(|err| format!("failed to load OAuth tokens: {err}"))?
    else {
        return Ok(None);
    };
    TokenBundle::from_json_str(&raw)
        .map(Some)
        .map_err(|err| format!("stored OAuth token payload is invalid: {err}"))
}

/// Persists Spotify OAuth tokens via [`LocalSecretStore`] (OS keyring with `0600`
/// file fallback at `paths.tokens_file()`).
///
/// # Errors
/// Returns an error if directories cannot be created or the secret backend fails.
pub fn save_tokens(paths: &AppPaths, bundle: &TokenBundle) -> Result<(), String> {
    paths
        .ensure_dirs()
        .map_err(|err| format!("failed to create state directories: {err}"))?;
    let store = LocalSecretStore::new(paths.tokens_file());
    let payload = bundle.to_json_string();
    store
        .set_secret(SPOTIFY_TOKEN_ACCOUNT, &payload)
        .map_err(|err| format!("failed to save OAuth tokens: {err}"))
}

/// Removes stored Spotify OAuth tokens from the OS keyring and fallback file.
///
/// # Errors
/// Returns an error if clearing the secret store fails.
pub fn clear_tokens(paths: &AppPaths) -> Result<(), String> {
    let store = LocalSecretStore::new(paths.tokens_file());
    store
        .delete_secret(SPOTIFY_TOKEN_ACCOUNT)
        .map_err(|err| format!("failed to clear OAuth tokens: {err}"))
}

fn is_safe_theme_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && name != "."
}

fn atomic_write_string(target: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }

    let tmp_path = temp_sibling_path(target);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&tmp_path)
        .map_err(|err| format!("failed to create {}: {err}", tmp_path.display()))?;

    if let Err(err) = file
        .write_all(content.as_bytes())
        .and_then(|()| file.sync_all())
    {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!("failed to write {}: {err}", tmp_path.display()));
    }
    drop(file);

    fs::rename(&tmp_path, target).map_err(|err| {
        let _ = fs::remove_file(&tmp_path);
        format!(
            "failed to replace {} with {}: {err}",
            target.display(),
            tmp_path.display()
        )
    })
}

fn temp_sibling_path(target: &Path) -> PathBuf {
    let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config.toml");
    target.with_file_name(format!(".{name}.tmp.{pid}.{seq}"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use vynl_core::config::AppConfig;
    use vynl_core::theme::Rgb;
    use vynl_spotify::pkce::TokenBundle;
    use vynl_system::paths::{AppPaths, EnvSnapshot};

    use super::{
        clear_tokens, discover_custom_themes, list_available_themes, load_config, load_theme,
        load_tokens, save_config, save_tokens,
    };

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_paths(label: &str) -> (PathBuf, AppPaths) {
        let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "vynl-tui-storage-{label}-{}-{seq}",
            std::process::id()
        ));
        let env = EnvSnapshot {
            xdg_config_home: Some(root.join("config")),
            xdg_cache_home: Some(root.join("cache")),
            xdg_state_home: Some(root.join("state")),
            xdg_runtime_dir: Some(root.join("run")),
            home: Some(root.clone()),
            appdata: None,
            local_appdata: None,
            temp_dir: root.join("tmp"),
        };
        (root, AppPaths::from_env(&env, false))
    }

    #[test]
    fn config_loads_default_when_missing_and_round_trips_on_disk() {
        let (root, paths) = temp_paths("cfg");

        let default_loaded = load_config(&paths);
        assert_eq!(default_loaded, Ok(AppConfig::default()));

        let mut cfg = AppConfig::default();
        cfg.spotify.client_id = Some(String::from("my-client-id-123"));
        cfg.ui.theme = String::from("nord");
        cfg.ui.adaptive_cover_colors = false;
        cfg.images.max_memory_cache_items = 96;

        assert_eq!(save_config(&paths, &cfg), Ok(()));
        assert!(paths.config_file().is_file());

        let reloaded = load_config(&paths);
        assert_eq!(reloaded, Ok(cfg));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn custom_themes_load_override_and_discover_from_disk() {
        let (root, paths) = temp_paths("themes");
        assert!(paths.ensure_dirs().is_ok());

        let custom_toml = r##"
background = "#101018"
surface = "#181824"
text_primary = "#f8f8ff"
text_muted = "#a0a0c0"
accent = "#ff55aa"
border = "#303048"
"##;
        assert!(fs::write(paths.themes_dir.join("synthwave.toml"), custom_toml).is_ok());

        let loaded = load_theme(&paths, "synthwave");
        assert!(loaded.is_ok());
        if let Ok(theme) = loaded {
            assert_eq!(theme.name, "synthwave");
            assert_eq!(theme.accent, Rgb::new(0xff, 0x55, 0xaa));
        }

        let builtin = load_theme(&paths, "tokyo-night");
        assert!(builtin.is_ok());

        let discovered = discover_custom_themes(&paths);
        assert_eq!(discovered, vec![String::from("synthwave")]);

        let all = list_available_themes(&paths);
        assert!(all.contains(&String::from("vynl-dark")));
        assert!(all.contains(&String::from("synthwave")));

        assert!(load_theme(&paths, "does-not-exist").is_err());
        assert!(load_theme(&paths, "../escape").is_err());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn tokens_round_trip_and_clear_via_local_secret_store() {
        let (root, paths) = temp_paths("tokens");
        let bundle = TokenBundle {
            access_token: String::from("access-xyz"),
            refresh_token: String::from("refresh-abc"),
            expires_at_epoch_secs: 2_000_000_000,
        };

        assert_eq!(save_tokens(&paths, &bundle), Ok(()));
        let loaded = load_tokens(&paths);
        assert_eq!(loaded, Ok(Some(bundle)));

        assert_eq!(clear_tokens(&paths), Ok(()));
        assert_eq!(load_tokens(&paths), Ok(None));

        let _ = fs::remove_dir_all(&root);
    }
}
