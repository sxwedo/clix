use std::{env, time::Duration};
use tokio::process::Command;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn resolve_token(
    explicit_token: Option<String>,
    config: &crate::settings::GitHubSettings,
) -> Option<String> {
    if let Some(token) = nonblank(explicit_token) {
        return Some(token);
    }

    if let Some(token) = nonblank(config.token.clone()) {
        return Some(token);
    }

    for name in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Some(token) = nonblank(env::var(name).ok()) {
            return Some(token);
        }
    }

    command_stdout("gh", &["auth", "token"]).await
}

pub async fn resolve_username(
    explicit_user: Option<String>,
    config: &crate::settings::GitHubSettings,
) -> Option<String> {
    if explicit_user.is_some() {
        return nonblank(explicit_user).filter(|user| is_valid_github_login(user));
    }

    if let Some(user) = nonblank(config.username.clone())
        && is_valid_github_login(&user)
    {
        return Some(user);
    }

    if let Some(user) = command_stdout("gh", &["api", "user", "-q", ".login"]).await
        && is_valid_github_login(&user)
    {
        return Some(user);
    }

    if let Some(user) = command_stdout("git", &["config", "github.user"]).await
        && is_valid_github_login(&user)
    {
        return Some(user);
    }

    None
}

async fn command_stdout(program: &str, args: &[&str]) -> Option<String> {
    let mut command = Command::new(program);
    command.args(args).kill_on_drop(true);
    let output = tokio::time::timeout(COMMAND_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    nonblank(String::from_utf8(output.stdout).ok())
}

fn nonblank(value: Option<String>) -> Option<String> {
    let value = value?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Return whether `value` is a syntactically valid GitHub login.
#[must_use]
pub fn is_valid_github_login(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 39
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        && !value.contains("--")
}

#[cfg(test)]
mod tests {
    use super::{is_valid_github_login, nonblank};

    #[test]
    fn blank_values_are_rejected_after_trimming() {
        assert_eq!(nonblank(Some("  ".into())), None);
        assert_eq!(nonblank(Some(" octocat ".into())), Some("octocat".into()));
    }

    #[test]
    fn github_login_validation_rejects_display_names_and_bad_hyphens() {
        assert!(is_valid_github_login("octocat"));
        assert!(is_valid_github_login("github-user-1"));
        assert!(!is_valid_github_login("John Doe"));
        assert!(!is_valid_github_login("-octocat"));
        assert!(!is_valid_github_login("octo--cat"));
    }
}
