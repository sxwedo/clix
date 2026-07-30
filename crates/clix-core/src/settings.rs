//! External configuration loaded from `~/.config/clix/config.toml`.
//!
//! Credential resolution priority (highest first):
//! 1. CLI flags (for example `--token`, `--auth-token`)
//! 2. Values declared in this file
//! 3. Environment variables (`X_AUTH_TOKEN`, `GITHUB_TOKEN`, ...)
//! 4. GitHub autodetect via the `gh` / `git` CLIs

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const DEFAULT_CONFIG_TEMPLATE: &str = "\
# clix configuration (~/.config/clix/config.toml)
#
# Credential resolution priority (highest first):
#   1. CLI flags         (e.g. --token, --auth-token, --ct0)
#   2. This config file  (the values below)
#   3. Environment vars  (X_AUTH_TOKEN, X_CT0, GITHUB_TOKEN, GH_TOKEN, ...)
#   4. GitHub autodetect (`gh auth token` / `gh api user`)
#
# Fill in only what you need, then remove the leading `#`.

[github]
# GitHub personal access token. Leave unset to fall back to `gh auth token`.
# token = \"ghp_xxxxxxxxxxxxxxxxxxxx\"
# GitHub username. Leave unset to fall back to `gh api user` / git config.
# username = \"your-name\"

[x]
# X (Twitter) login cookie `auth_token`.
# auth_token = \"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"
# X (Twitter) CSRF cookie `ct0`.
# ct0 = \"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"
";

/// Top-level configuration mirror of `config.toml`.
#[derive(Debug, Default, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub github: GitHubSettings,
    #[serde(default)]
    pub x: XSettings,
}

/// `[github]` section: explicit GitHub credentials.
#[derive(Debug, Default, Deserialize)]
pub struct GitHubSettings {
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
}

/// `[x]` section: explicit X (Twitter) cookies.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct XSettings {
    #[serde(default)]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub ct0: Option<String>,
}

impl Settings {
    /// Load settings from the config file, returning defaults when it is absent.
    ///
    /// # Errors
    ///
    /// Returns an error when the file exists but cannot be read or parsed.
    pub fn load() -> Result<Self> {
        let path = config_path();
        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        toml::from_str(&contents).with_context(|| format!("failed to parse {}", path.display()))
    }
}

/// Resolve the clix config directory (`~/.config/clix`).
///
/// Honors `XDG_CONFIG_HOME` when set, otherwise uses `~/.config/clix`
/// on every platform. We intentionally avoid `dirs::config_dir()`, which
/// diverges to `~/Library/Application Support` on macOS and would contradict
/// the documented path in our help text and error messages.
#[must_use]
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("clix")
}

/// Resolve the config file path (`~/.config/clix/config.toml`).
#[must_use]
pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Write a commented template to the config path with restrictive `0600` perms.
///
/// # Errors
///
/// Returns an error when the file already exists, the parent directory cannot
/// be created, or the file cannot be written.
pub fn ensure_default_config() -> Result<PathBuf> {
    let path = config_path();
    if path.exists() {
        bail!("config file already exists: {}", path.display());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    write_private(&path, DEFAULT_CONFIG_TEMPLATE)?;
    Ok(path)
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    file.write_all(contents.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &str) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_parses_to_defaults() {
        let settings: Settings = toml::from_str("").expect("empty toml");
        assert!(settings.github.token.is_none());
        assert!(settings.github.username.is_none());
        assert!(settings.x.auth_token.is_none());
        assert!(settings.x.ct0.is_none());
    }

    #[test]
    fn partial_config_keeps_absent_fields_as_none() {
        let text = "\
[x]
auth_token = \"abc\"
";
        let settings: Settings = toml::from_str(text).expect("partial toml");
        assert_eq!(settings.x.auth_token.as_deref(), Some("abc"));
        assert!(settings.x.ct0.is_none());
        assert!(settings.github.token.is_none());
    }

    #[test]
    fn missing_section_uses_serde_default() {
        let text = "[github]\ntoken = \"t\"\n";
        let settings: Settings = toml::from_str(text).expect("github-only toml");
        assert_eq!(settings.github.token.as_deref(), Some("t"));
        assert!(settings.x.auth_token.is_none());
    }
}
