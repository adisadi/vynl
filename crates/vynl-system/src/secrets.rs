//! Local credential storage abstractions for `vynl`.
//!
//! Provides a unified [`SecretStore`] trait backed by:
//! - [`OsKeyringStore`] — native OS secure storage (`Secret Service` via pure-Rust D-Bus on
//!   Linux, macOS Keychain, and Windows Credential Manager),
//! - [`FileSecretStore`] — atomic `0600` JSON file fallback for headless environments and
//!   platforms without a running keyring daemon,
//! - [`LocalSecretStore`] — tries [`OsKeyringStore`] first and transparently falls back to
//!   [`FileSecretStore`], and
//! - [`MemorySecretStore`] — thread-safe in-memory store for deterministic unit testing.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

/// Default service namespace used when storing credentials in the OS keyring.
pub const DEFAULT_KEYRING_SERVICE: &str = "vynl";

/// Counter used to generate unique temporary filenames during atomic writes.
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Error returned by [`SecretStore`] operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretError {
    message: String,
}

impl SecretError {
    /// Creates a new [`SecretError`] with a human-readable diagnostic message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the diagnostic error message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for SecretError {}

impl From<io::Error> for SecretError {
    fn from(err: io::Error) -> Self {
        Self::new(err.to_string())
    }
}

/// Platform-neutral secret storage interface for Spotify OAuth tokens and credentials.
pub trait SecretStore: Send + Sync {
    /// Human-readable identifier for the active secret storage backend.
    fn backend_name(&self) -> &'static str;

    /// Retrieves the stored secret for `account`, returning `Ok(None)` if absent.
    ///
    /// # Errors
    ///
    /// Returns [`SecretError`] if the underlying storage backend cannot be queried.
    fn get_secret(&self, account: &str) -> Result<Option<String>, SecretError>;

    /// Persists `secret` for `account`, creating or replacing any existing value.
    ///
    /// # Errors
    ///
    /// Returns [`SecretError`] if the secret cannot be written to the storage backend.
    fn set_secret(&self, account: &str, secret: &str) -> Result<(), SecretError>;

    /// Removes the stored secret for `account`, succeeding if no entry exists.
    ///
    /// # Errors
    ///
    /// Returns [`SecretError`] if the entry cannot be removed from the storage backend.
    fn delete_secret(&self, account: &str) -> Result<(), SecretError>;
}

/// Thread-safe in-memory [`SecretStore`] intended for deterministic tests.
#[derive(Debug, Default)]
pub struct MemorySecretStore {
    entries: Mutex<BTreeMap<String, String>>,
}

impl MemorySecretStore {
    /// Creates an empty [`MemorySecretStore`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemorySecretStore {
    fn backend_name(&self) -> &'static str {
        "in-memory"
    }

    fn get_secret(&self, account: &str) -> Result<Option<String>, SecretError> {
        let guard = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(guard.get(account).cloned())
    }

    fn set_secret(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        let mut guard = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        guard.insert(account.to_owned(), secret.to_owned());
        Ok(())
    }

    fn delete_secret(&self, account: &str) -> Result<(), SecretError> {
        let mut guard = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        guard.remove(account);
        Ok(())
    }
}

/// Pure-Rust file-backed [`SecretStore`] using atomic writes and `0600` Unix permissions.
///
/// Stores account-to-secret pairs as a JSON object at a configured filesystem path.
/// Safe to use on headless Linux, SSH sessions, and targets without a native keyring.
#[derive(Debug)]
pub struct FileSecretStore {
    path: PathBuf,
    io_lock: Mutex<()>,
}

impl FileSecretStore {
    /// Creates a [`FileSecretStore`] that persists secrets to `path`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            io_lock: Mutex::new(()),
        }
    }

    /// Returns the target file path used by this store.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_map(&self) -> Result<BTreeMap<String, String>, SecretError> {
        let content = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(err) => {
                return Err(SecretError::new(format!(
                    "failed to read secret store {}: {err}",
                    self.path.display()
                )));
            }
        };

        if content.trim().is_empty() {
            return Ok(BTreeMap::new());
        }

        json_map::parse(&content).map_err(|err| {
            SecretError::new(format!(
                "failed to parse secret store {}: {err}",
                self.path.display()
            ))
        })
    }

    fn write_map_atomic(&self, map: &BTreeMap<String, String>) -> Result<(), SecretError> {
        let parent = self.path.parent().filter(|p| !p.as_os_str().is_empty());
        if let Some(dir) = parent {
            fs::create_dir_all(dir).map_err(|err| {
                SecretError::new(format!(
                    "failed to create secret store directory {}: {err}",
                    dir.display()
                ))
            })?;
        }

        let payload = json_map::encode(map);
        let temp_path = unique_temp_path(&self.path);

        let write_result = write_private_file(&temp_path, payload.as_bytes());
        if let Err(err) = write_result {
            let _ = fs::remove_file(&temp_path);
            return Err(SecretError::new(format!(
                "failed to write temporary secret file {}: {err}",
                temp_path.display()
            )));
        }

        if let Err(err) = fs::rename(&temp_path, &self.path) {
            let _ = fs::remove_file(&temp_path);
            return Err(SecretError::new(format!(
                "failed to replace secret store {}: {err}",
                self.path.display()
            )));
        }

        Ok(())
    }
}

impl SecretStore for FileSecretStore {
    fn backend_name(&self) -> &'static str {
        "file (0600)"
    }

    fn get_secret(&self, account: &str) -> Result<Option<String>, SecretError> {
        let _guard = self.io_lock.lock().unwrap_or_else(PoisonError::into_inner);
        let map = self.read_map()?;
        Ok(map.get(account).cloned())
    }

    fn set_secret(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        let _guard = self.io_lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut map = self.read_map()?;
        map.insert(account.to_owned(), secret.to_owned());
        self.write_map_atomic(&map)
    }

    fn delete_secret(&self, account: &str) -> Result<(), SecretError> {
        let _guard = self.io_lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut map = self.read_map()?;
        if map.remove(account).is_none() {
            return Ok(());
        }
        self.write_map_atomic(&map)
    }
}

/// Native OS keyring [`SecretStore`] backed by `keyring` (`Secret Service`, macOS Keychain,
/// or Windows Credential Manager).
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[derive(Debug, Clone)]
pub struct OsKeyringStore {
    service: String,
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
impl Default for OsKeyringStore {
    fn default() -> Self {
        Self::new(DEFAULT_KEYRING_SERVICE)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
impl OsKeyringStore {
    /// Creates an [`OsKeyringStore`] scoped to the given `service` name.
    #[must_use]
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    /// Returns the keyring service name used by this store.
    #[must_use]
    pub fn service(&self) -> &str {
        &self.service
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
impl SecretStore for OsKeyringStore {
    fn backend_name(&self) -> &'static str {
        if cfg!(target_os = "linux") {
            "OS Keyring (Secret Service)"
        } else if cfg!(target_os = "macos") {
            "OS Keyring (macOS Keychain)"
        } else {
            "OS Keyring (Windows Credential Manager)"
        }
    }

    fn get_secret(&self, account: &str) -> Result<Option<String>, SecretError> {
        let entry = keyring::Entry::new(&self.service, account)
            .map_err(|err| SecretError::new(err.to_string()))?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(SecretError::new(err.to_string())),
        }
    }

    fn set_secret(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        let entry = keyring::Entry::new(&self.service, account)
            .map_err(|err| SecretError::new(err.to_string()))?;
        entry
            .set_password(secret)
            .map_err(|err| SecretError::new(err.to_string()))
    }

    fn delete_secret(&self, account: &str) -> Result<(), SecretError> {
        let entry = keyring::Entry::new(&self.service, account)
            .map_err(|err| SecretError::new(err.to_string()))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(SecretError::new(err.to_string())),
        }
    }
}

/// Adaptive local [`SecretStore`] that prefers the OS keyring and transparently falls back
/// to a `0600` [`FileSecretStore`].
///
/// On Linux, macOS, and Windows, operations attempt [`OsKeyringStore`] first. When the
/// system keyring daemon is unavailable (for example, in headless CI or an SSH session
/// without `org.freedesktop.secrets`), reads and writes seamlessly fall back to the file
/// store. On targets without native keyring support, [`FileSecretStore`] is used directly.
#[derive(Debug)]
pub struct LocalSecretStore {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    keyring: OsKeyringStore,
    fallback: FileSecretStore,
}

impl LocalSecretStore {
    /// Creates a [`LocalSecretStore`] using [`DEFAULT_KEYRING_SERVICE`] (`"vynl"`) and the
    /// specified fallback file path.
    #[must_use]
    pub fn new(fallback_path: impl Into<PathBuf>) -> Self {
        Self::with_service(DEFAULT_KEYRING_SERVICE, fallback_path)
    }

    /// Creates a [`LocalSecretStore`] with a custom keyring `service` name and fallback file path.
    #[must_use]
    pub fn with_service(service: impl Into<String>, fallback_path: impl Into<PathBuf>) -> Self {
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        let _ = service.into();

        Self {
            #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
            keyring: OsKeyringStore::new(service),
            fallback: FileSecretStore::new(fallback_path),
        }
    }

    /// Returns the filesystem path used by the `0600` fallback store.
    #[must_use]
    pub fn fallback_path(&self) -> &Path {
        self.fallback.path()
    }
}

impl SecretStore for LocalSecretStore {
    fn backend_name(&self) -> &'static str {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            "OS Keyring (with 0600 file fallback)"
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            self.fallback.backend_name()
        }
    }

    fn get_secret(&self, account: &str) -> Result<Option<String>, SecretError> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            if let Ok(Some(secret)) = self.keyring.get_secret(account) {
                return Ok(Some(secret));
            }
        }
        self.fallback.get_secret(account)
    }

    fn set_secret(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            if self.keyring.set_secret(account, secret).is_ok() {
                // Remove any stale fallback copy now that the OS keyring holds the secret.
                let _ = self.fallback.delete_secret(account);
                return Ok(());
            }
        }
        self.fallback.set_secret(account, secret)
    }

    fn delete_secret(&self, account: &str) -> Result<(), SecretError> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            let keyring_result = self.keyring.delete_secret(account);
            let fallback_result = self.fallback.delete_secret(account);
            if keyring_result.is_ok() || fallback_result.is_ok() {
                return Ok(());
            }
            fallback_result
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        {
            self.fallback.delete_secret(account)
        }
    }
}

fn unique_temp_path(target: &Path) -> PathBuf {
    let seq = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let file_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("secrets");
    let tmp_name = format!(".{file_name}.tmp.{}.{seq}", std::process::id());
    match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    }
}

fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }

    let mut file = options.open(path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        // Enforce 0600 explicitly regardless of process umask.
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }

    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// Minimal pure-Rust JSON string-map serializer and parser (RFC 8259) for `FileSecretStore`.
mod json_map {
    use std::collections::BTreeMap;
    use std::fmt::Write as _;

    pub(super) fn encode(map: &BTreeMap<String, String>) -> String {
        let mut out = String::from("{");
        for (idx, (key, value)) in map.iter().enumerate() {
            if idx > 0 {
                out.push(',');
            }
            push_escaped_str(&mut out, key);
            out.push(':');
            push_escaped_str(&mut out, value);
        }
        out.push('}');
        out
    }

    pub(super) fn parse(input: &str) -> Result<BTreeMap<String, String>, String> {
        let mut parser = Parser::new(input);
        let map = parser.parse_object()?;
        parser.skip_ws();
        if !parser.is_eof() {
            return Err(String::from(
                "unexpected trailing characters after JSON object",
            ));
        }
        Ok(map)
    }

    fn push_escaped_str(out: &mut String, input: &str) {
        out.push('"');
        for ch in input.chars() {
            match ch {
                '"' => out.push_str(r#"\""#),
                '\\' => out.push_str(r"\\"),
                '\u{0008}' => out.push_str(r"\b"),
                '\u{000C}' => out.push_str(r"\f"),
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

    struct Parser<'a> {
        bytes: &'a [u8],
        pos: usize,
    }

    impl<'a> Parser<'a> {
        const fn new(input: &'a str) -> Self {
            Self {
                bytes: input.as_bytes(),
                pos: 0,
            }
        }

        const fn is_eof(&self) -> bool {
            self.pos >= self.bytes.len()
        }

        fn peek(&self) -> Option<u8> {
            self.bytes.get(self.pos).copied()
        }

        fn next_byte(&mut self) -> Option<u8> {
            let byte = self.peek()?;
            self.pos += 1;
            Some(byte)
        }

        fn skip_ws(&mut self) {
            while let Some(byte) = self.peek() {
                if matches!(byte, b' ' | b'\t' | b'\r' | b'\n') {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }

        fn parse_object(&mut self) -> Result<BTreeMap<String, String>, String> {
            self.skip_ws();
            if self.next_byte() != Some(b'{') {
                return Err(String::from("expected '{' at start of JSON object"));
            }

            let mut map = BTreeMap::new();
            self.skip_ws();
            if self.peek() == Some(b'}') {
                self.pos += 1;
                return Ok(map);
            }

            loop {
                self.skip_ws();
                let key = self.parse_string()?;
                self.skip_ws();
                if self.next_byte() != Some(b':') {
                    return Err(String::from("expected ':' after object key"));
                }
                self.skip_ws();
                let value = self.parse_string()?;
                map.insert(key, value);

                self.skip_ws();
                match self.next_byte() {
                    Some(b',') => {}
                    Some(b'}') => return Ok(map),
                    _ => return Err(String::from("expected ',' or '}' in JSON object")),
                }
            }
        }

        fn parse_string(&mut self) -> Result<String, String> {
            if self.next_byte() != Some(b'"') {
                return Err(String::from("expected '\"' at start of JSON string"));
            }

            let mut out = String::new();
            while let Some(byte) = self.next_byte() {
                match byte {
                    b'"' => return Ok(out),
                    b'\\' => self.parse_escape(&mut out)?,
                    0x00..=0x1F => {
                        return Err(String::from("unescaped control character in JSON string"));
                    }
                    b if b < 0x80 => out.push(char::from(b)),
                    _ => {
                        // Decode multi-byte UTF-8 sequence directly from the original slice.
                        let start = self.pos - 1;
                        let rest = std::str::from_utf8(&self.bytes[start..])
                            .map_err(|_| String::from("invalid UTF-8 sequence in JSON string"))?;
                        let Some(ch) = rest.chars().next() else {
                            return Err(String::from("unexpected EOF in UTF-8 string"));
                        };
                        out.push(ch);
                        self.pos = start + ch.len_utf8();
                    }
                }
            }
            Err(String::from("unterminated JSON string"))
        }

        fn parse_escape(&mut self, out: &mut String) -> Result<(), String> {
            let esc = self
                .next_byte()
                .ok_or_else(|| String::from("unexpected EOF in escape sequence"))?;
            match esc {
                b'"' => out.push('"'),
                b'\\' => out.push('\\'),
                b'/' => out.push('/'),
                b'b' => out.push('\u{0008}'),
                b'f' => out.push('\u{000C}'),
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b't' => out.push('\t'),
                b'u' => {
                    let code_unit = self.parse_hex4()?;
                    if (0xD800..=0xDBFF).contains(&code_unit) {
                        if self.next_byte() != Some(b'\\') || self.next_byte() != Some(b'u') {
                            return Err(String::from("missing low surrogate escape"));
                        }
                        let low = self.parse_hex4()?;
                        if !(0xDC00..=0xDFFF).contains(&low) {
                            return Err(String::from("invalid UTF-16 low surrogate"));
                        }
                        let combined = 0x10000
                            + ((u32::from(code_unit) - 0xD800) << 10)
                            + (u32::from(low) - 0xDC00);
                        let ch = char::from_u32(combined)
                            .ok_or_else(|| String::from("invalid Unicode scalar value"))?;
                        out.push(ch);
                    } else if (0xDC00..=0xDFFF).contains(&code_unit) {
                        return Err(String::from("unexpected lone UTF-16 low surrogate"));
                    } else {
                        let ch = char::from_u32(u32::from(code_unit))
                            .ok_or_else(|| String::from("invalid Unicode scalar value"))?;
                        out.push(ch);
                    }
                }
                _ => return Err(String::from("invalid escape sequence in JSON string")),
            }
            Ok(())
        }

        fn parse_hex4(&mut self) -> Result<u16, String> {
            let mut value: u16 = 0;
            for _ in 0..4 {
                let digit = self
                    .next_byte()
                    .ok_or_else(|| String::from("truncated \\uXXXX escape"))?;
                let nibble = match digit {
                    b'0'..=b'9' => u16::from(digit - b'0'),
                    b'a'..=b'f' => u16::from(digit - b'a' + 10),
                    b'A'..=b'F' => u16::from(digit - b'A' + 10),
                    _ => return Err(String::from("invalid hex digit in \\uXXXX escape")),
                };
                value = (value << 4) | nibble;
            }
            Ok(value)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        DEFAULT_KEYRING_SERVICE, FileSecretStore, LocalSecretStore, MemorySecretStore, SecretError,
        SecretStore,
    };

    static TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_dir(prefix: &str) -> Result<PathBuf, String> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|err| err.to_string())?
            .as_nanos();
        let seq = TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        Ok(std::env::temp_dir().join(format!("{prefix}-{}-{nanos}-{seq}", std::process::id())))
    }

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn secret_stores_are_send_and_sync() {
        assert_send_sync::<MemorySecretStore>();
        assert_send_sync::<FileSecretStore>();
        assert_send_sync::<LocalSecretStore>();
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        assert_send_sync::<super::OsKeyringStore>();
    }

    #[test]
    fn memory_secret_store_round_trip() -> Result<(), SecretError> {
        let store = MemorySecretStore::new();
        assert_eq!(store.backend_name(), "in-memory");
        assert_eq!(store.get_secret("spotify")?, None);

        store.set_secret("spotify", "token-v1")?;
        assert_eq!(store.get_secret("spotify")?.as_deref(), Some("token-v1"));

        store.set_secret("spotify", "token-v2")?;
        assert_eq!(store.get_secret("spotify")?.as_deref(), Some("token-v2"));

        store.delete_secret("spotify")?;
        assert_eq!(store.get_secret("spotify")?, None);

        // Deleting an absent account is a clean no-op.
        store.delete_secret("missing")?;
        Ok(())
    }

    #[test]
    fn file_secret_store_round_trip_and_permissions() -> Result<(), String> {
        let dir = unique_temp_dir("vynl-secrets-file")?;
        let file_path = dir.join("nested").join("tokens.json");
        let store = FileSecretStore::new(&file_path);

        assert_eq!(store.backend_name(), "file (0600)");
        assert_eq!(store.path(), file_path.as_path());
        assert_eq!(
            store.get_secret("oauth").map_err(|err| err.to_string())?,
            None
        );

        let complex_secret = r#"{"access_token":"abc\"def\\ghi","note":"line1\nline2 🎵"}"#;
        store
            .set_secret("oauth", complex_secret)
            .map_err(|err| err.to_string())?;
        store
            .set_secret("refresh", "rft_123456")
            .map_err(|err| err.to_string())?;

        assert_eq!(
            store.get_secret("oauth").map_err(|err| err.to_string())?,
            Some(complex_secret.to_owned())
        );
        assert_eq!(
            store.get_secret("refresh").map_err(|err| err.to_string())?,
            Some(String::from("rft_123456"))
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let metadata = fs::metadata(&file_path).map_err(|err| err.to_string())?;
            let mode = metadata.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        store
            .delete_secret("oauth")
            .map_err(|err| err.to_string())?;
        assert_eq!(
            store.get_secret("oauth").map_err(|err| err.to_string())?,
            None
        );
        assert_eq!(
            store.get_secret("refresh").map_err(|err| err.to_string())?,
            Some(String::from("rft_123456"))
        );

        store
            .delete_secret("nonexistent")
            .map_err(|err| err.to_string())?;

        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn local_secret_store_round_trip_with_fallback() -> Result<(), String> {
        let dir = unique_temp_dir("vynl-secrets-local")?;
        let fallback_path = dir.join("tokens.json");
        let service = format!("{DEFAULT_KEYRING_SERVICE}-test-{}", std::process::id());
        let account = format!("account-{}", std::process::id());

        let store = LocalSecretStore::with_service(service, &fallback_path);
        assert_ne!(store.backend_name(), "");
        assert_eq!(store.fallback_path(), fallback_path.as_path());

        // Start clean.
        store
            .delete_secret(&account)
            .map_err(|err| err.to_string())?;
        assert_eq!(
            store.get_secret(&account).map_err(|err| err.to_string())?,
            None
        );

        // Pre-seed a fallback entry to verify fallback reads are discovered even if keyring is empty.
        let seeded_fallback = FileSecretStore::new(&fallback_path);
        seeded_fallback
            .set_secret(&account, "seeded-fallback-secret")
            .map_err(|err| err.to_string())?;
        assert_eq!(
            store.get_secret(&account).map_err(|err| err.to_string())?,
            Some(String::from("seeded-fallback-secret"))
        );

        // Overwrite via LocalSecretStore and verify round-trip.
        store
            .set_secret(&account, "updated-secret-value")
            .map_err(|err| err.to_string())?;
        assert_eq!(
            store.get_secret(&account).map_err(|err| err.to_string())?,
            Some(String::from("updated-secret-value"))
        );

        // Delete removes from both keyring and fallback cleanly.
        store
            .delete_secret(&account)
            .map_err(|err| err.to_string())?;
        assert_eq!(
            store.get_secret(&account).map_err(|err| err.to_string())?,
            None
        );

        let default_store = LocalSecretStore::new(dir.join("default-tokens.json"));
        assert_eq!(
            default_store.fallback_path(),
            dir.join("default-tokens.json").as_path()
        );

        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn secret_error_display_and_std_error() {
        let err = SecretError::new("keyring locked");
        assert_eq!(err.message(), "keyring locked");
        assert_eq!(err.to_string(), "keyring locked");
        let dyn_err: &dyn std::error::Error = &err;
        assert!(dyn_err.source().is_none());
    }
}
