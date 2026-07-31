//! External configuration loaded from `~/.config/clix/config.toml`.
//!
//! Credential resolution priority (highest first):
//! 1. CLI flags (for example `--token`, `--auth-token`)
//! 2. Values declared in this file
//! 3. Environment variables (`X_AUTH_TOKEN`, `GITHUB_TOKEN`, ...)
//! 4. GitHub autodetect via the `gh` / `git` CLIs

use std::collections::BTreeMap;
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

# [lark.accounts.default]
# Lark custom app credentials.
# app_id = \"cli_xxxxxxxxxxxxxxxx\"
# app_secret = \"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\"

# [lark.bases.rss_news]
# Named Base targets are reusable by RSS and future clix commands.
# account = \"default\"
# app_token = \"bascnxxxxxxxxxxxx\"
# table_id = \"tblxxxxxxxxxxxx\"

[rss]
# Default output path. Relative paths are resolved from the current directory.
# The .json extension selects JSON; every other extension defaults to Markdown.
# output = \"rss.md\"
# Incremental sync database (default: ~/.config/clix/rss.redb).
# state = \"/absolute/path/to/rss.redb\"
# Maximum entries kept from each feed (default: 20).
# limit = 20

# Add one block per subscription. `enabled` defaults to true.
# [[rss.feeds]]
# name = \"Rust Blog\"
# url = \"https://blog.rust-lang.org/feed.xml\"
# enabled = true
";

/// Top-level configuration mirror of `config.toml`.
#[derive(Debug, Default, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub github: GitHubSettings,
    #[serde(default)]
    pub x: XSettings,
    #[serde(default)]
    pub lark: LarkSettings,
    #[serde(default)]
    pub rss: RssSettings,
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

/// Named Lark accounts and Base targets shared by all Lark-backed commands.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct LarkSettings {
    /// Named custom-app credentials.
    #[serde(default)]
    pub accounts: BTreeMap<String, LarkAccountSettings>,
    /// Named Base table targets.
    #[serde(default)]
    pub bases: BTreeMap<String, LarkBaseSettings>,
}

/// One named Lark custom-app account.
#[derive(Debug, Clone, Deserialize)]
pub struct LarkAccountSettings {
    /// Lark custom app ID.
    pub app_id: String,
    /// Lark custom app secret.
    pub app_secret: String,
}

/// One named Lark Base table.
#[derive(Debug, Clone, Deserialize)]
pub struct LarkBaseSettings {
    /// Name of the entry in `[lark.accounts]` used for authentication.
    pub account: String,
    /// Base app token.
    pub app_token: String,
    /// Base table ID.
    pub table_id: String,
}

/// `[rss]` section: RSS subscriptions and fetch defaults.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct RssSettings {
    /// Default export path. Relative paths use the caller's current directory.
    #[serde(default)]
    pub output: Option<PathBuf>,
    /// Incremental sync database. Relative paths use the caller's current directory.
    #[serde(default)]
    pub state: Option<PathBuf>,
    /// Default maximum number of entries retained from each feed.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Configured RSS, Atom, or JSON Feed subscriptions.
    #[serde(default)]
    pub feeds: Vec<RssFeedSettings>,
}

/// One `[[rss.feeds]]` subscription.
#[derive(Debug, Clone, Deserialize)]
pub struct RssFeedSettings {
    /// Stable human-readable name used by `clix rss fetch --feed`.
    pub name: String,
    /// HTTP(S) URL of the RSS, Atom, or JSON Feed document.
    pub url: String,
    /// Whether this subscription participates in fetches.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

const fn enabled_by_default() -> bool {
    true
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
        assert!(settings.rss.output.is_none());
        assert!(settings.rss.state.is_none());
        assert!(settings.rss.limit.is_none());
        assert!(settings.rss.feeds.is_empty());
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
        assert!(settings.rss.feeds.is_empty());
    }

    #[test]
    fn missing_section_uses_serde_default() {
        let text = "[github]\ntoken = \"t\"\n";
        let settings: Settings = toml::from_str(text).expect("github-only toml");
        assert_eq!(settings.github.token.as_deref(), Some("t"));
        assert!(settings.x.auth_token.is_none());
        assert!(settings.rss.feeds.is_empty());
    }

    #[test]
    fn rss_subscriptions_parse_with_defaults_and_overrides() {
        let text = r#"
[rss]
output = "news.json"
state = "/tmp/rss.redb"
limit = 12

[[rss.feeds]]
name = "Rust Blog"
url = "https://blog.rust-lang.org/feed.xml"

[[rss.feeds]]
name = "Paused"
url = "https://example.com/feed.xml"
enabled = false
"#;
        let settings: Settings = toml::from_str(text).expect("rss config");
        assert_eq!(
            settings.rss.output.as_deref(),
            Some(std::path::Path::new("news.json"))
        );
        assert_eq!(
            settings.rss.state.as_deref(),
            Some(std::path::Path::new("/tmp/rss.redb"))
        );
        assert_eq!(settings.rss.limit, Some(12));
        assert_eq!(settings.rss.feeds.len(), 2);
        assert!(settings.rss.feeds[0].enabled);
        assert!(!settings.rss.feeds[1].enabled);
    }

    #[test]
    fn lark_accounts_and_bases_parse_as_reusable_named_resources() {
        let text = r#"
[lark.accounts.default]
app_id = "cli_example"
app_secret = "secret"

[lark.bases.rss_news]
account = "default"
app_token = "bascn_example"
table_id = "tbl_example"
"#;
        let settings: Settings = toml::from_str(text).expect("lark config");
        let account = &settings.lark.accounts["default"];
        assert_eq!(account.app_id, "cli_example");
        assert_eq!(account.app_secret, "secret");

        let base = &settings.lark.bases["rss_news"];
        assert_eq!(base.account, "default");
        assert_eq!(base.app_token, "bascn_example");
        assert_eq!(base.table_id, "tbl_example");
    }

    #[test]
    fn generated_config_template_includes_a_valid_rss_section() {
        let settings: Settings =
            toml::from_str(DEFAULT_CONFIG_TEMPLATE).expect("default template should parse");
        assert!(DEFAULT_CONFIG_TEMPLATE.contains("[rss]"));
        assert!(DEFAULT_CONFIG_TEMPLATE.contains("[[rss.feeds]]"));
        assert!(settings.rss.feeds.is_empty());
    }
}
