//! Reading `~/.config/redis-pane/config.json` (ADR-0002, ADR-0003, PLAN M0.7).
//!
//! The app only ever reads this file. Parsing and validation live in
//! `redis_pane_core::config`; this module does I/O and the permission check.

use std::path::{Path, PathBuf};

use redis_pane_core::config::{Config, ConfigError, parse};

/// Why the config could not be used. Each variant names the file, because a
/// diagnostic you cannot act on without guessing is not a diagnostic (R1.7).
#[derive(Debug)]
pub enum LoadError {
    /// The file is group- or world-readable and may carry a literal password.
    TooReadable {
        path: PathBuf,
        mode: u32,
    },
    Unreadable {
        path: PathBuf,
        source: std::io::Error,
    },
    Invalid {
        path: PathBuf,
        source: ConfigError,
    },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::TooReadable { path, mode } => write!(
                f,
                "{}: refusing to read a config file with mode {:04o}; it may contain a password.\n\
                 Fix it with:  chmod 600 {}",
                path.display(),
                mode,
                path.display()
            ),
            LoadError::Unreadable { path, source } => {
                write!(f, "{}: {source}", path.display())
            }
            LoadError::Invalid { path, source } => {
                write!(f, "{}: {source}", path.display())
            }
        }
    }
}

/// `$XDG_CONFIG_HOME/redis-pane/config.json`, falling back to
/// `~/.config/redis-pane/config.json` (ADR-0002).
pub fn default_path() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("redis-pane/config.json"));
    }
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(".config/redis-pane/config.json"))
}

/// Load the config, if there is one.
///
/// A missing file is not an error — zero configuration is a supported way to
/// run (R1.1). A file that exists but cannot be trusted is.
pub fn load(path: &Path) -> Result<Option<Config>, LoadError> {
    if !path.exists() {
        return Ok(None);
    }
    refuse_if_too_readable(path)?;
    let text = std::fs::read_to_string(path).map_err(|source| LoadError::Unreadable {
        path: path.to_path_buf(),
        source,
    })?;
    parse(&text).map(Some).map_err(|source| LoadError::Invalid {
        path: path.to_path_buf(),
        source,
    })
}

/// Follows SSH's StrictModes precedent for private keys. Refusing to load,
/// rather than warning, is deliberate: a warning in a TUI scrolls away, and
/// this file predictably ends up inside a symlinked dotfiles repo (ADR-0002).
#[cfg(unix)]
fn refuse_if_too_readable(path: &Path) -> Result<(), LoadError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).map_err(|source| LoadError::Unreadable {
        path: path.to_path_buf(),
        source,
    })?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(LoadError::TooReadable {
            path: path.to_path_buf(),
            mode,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn refuse_if_too_readable(_path: &Path) -> Result<(), LoadError> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write_with_mode(name: &str, text: &str, mode: u32) -> PathBuf {
        let path = std::env::temp_dir().join(format!("redis-pane-test-{name}.json"));
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let path = std::env::temp_dir().join("redis-pane-does-not-exist.json");
        let _ = std::fs::remove_file(&path);
        assert!(load(&path).unwrap().is_none());
    }

    #[test]
    fn a_private_file_loads() {
        let path = write_with_mode("private", r#"{"profiles":{"p":{"host":"h"}}}"#, 0o600);
        assert!(load(&path).unwrap().is_some());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_group_readable_file_is_refused() {
        let path = write_with_mode("group", r#"{"profiles":{}}"#, 0o640);
        match load(&path) {
            Err(LoadError::TooReadable { mode, .. }) => assert_eq!(mode, 0o640),
            other => panic!("expected refusal, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_world_readable_file_is_refused() {
        let path = write_with_mode("world", r#"{"profiles":{}}"#, 0o644);
        assert!(matches!(load(&path), Err(LoadError::TooReadable { .. })));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_refusal_message_says_how_to_fix_it() {
        let path = write_with_mode("msg", r#"{}"#, 0o644);
        let msg = load(&path).unwrap_err().to_string();
        assert!(msg.contains("chmod 600"), "got: {msg}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_invalid_file_names_the_path_and_the_location() {
        let path = write_with_mode("bad", "{\n  \"profiles\": {\n    oops\n  }\n}", 0o600);
        let msg = load(&path).unwrap_err().to_string();
        assert!(msg.contains("line 3"), "got: {msg}");
        assert!(msg.contains("redis-pane-test-bad"), "got: {msg}");
        let _ = std::fs::remove_file(&path);
    }
}
