use std::env;
use tokio::process::Command;

pub async fn resolve_token(explicit_token: Option<String>) -> Option<String> {
    if let Some(token) = explicit_token
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Some(token);
    }

    if let Ok(token) = env::var("GITHUB_TOKEN").or_else(|_| env::var("GH_TOKEN")) {
        let trimmed = token.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }

    // Attempt to get token from `gh auth token`
    if let Ok(output) = Command::new("gh").args(["auth", "token"]).output().await {
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if output.status.success() && !token.is_empty() {
            return Some(token);
        }
    }

    None
}

pub async fn resolve_username(explicit_user: Option<String>) -> Option<String> {
    if let Some(user) = explicit_user
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return Some(user);
    }

    // Try `gh api user -q .login`
    if let Ok(output) = Command::new("gh")
        .args(["api", "user", "-q", ".login"])
        .output()
        .await
    {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if output.status.success() && !stdout.is_empty() {
            return Some(stdout);
        }
    }

    // Fallback: `git config user.name`
    if let Ok(output) = Command::new("git")
        .args(["config", "user.name"])
        .output()
        .await
    {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if output.status.success() && !stdout.is_empty() {
            return Some(stdout);
        }
    }

    None
}
