use std::{
    borrow::Cow,
    fmt::Write as _,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use clix_core::{config, fs::atomic_write, ui};
use reqwest::{
    StatusCode,
    header::{ACCEPT, AUTHORIZATION, HeaderMap, LINK, RETRY_AFTER},
};
use serde::{Deserialize, Serialize};

const ERROR_BODY_CHARACTER_LIMIT: usize = 1_024;
const MAX_REQUEST_ATTEMPTS: u32 = 3;
const MAX_RETRY_AFTER_SECONDS: u64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Markdown,
    Urls,
    Json,
}

#[derive(Debug, Args)]
pub struct StarsArgs {
    /// GitHub username (auto-detected via `gh` CLI if omitted)
    pub username: Option<String>,

    /// Output file path (default: `<username>_starred_repos.md`)
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Output format: markdown, urls, or json
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Markdown)]
    pub format: OutputFormat,

    /// GitHub token (auto-detected via `GITHUB_TOKEN`, `GH_TOKEN`, or `gh auth token`)
    #[arg(short, long)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StarredRepo {
    pub name: String,
    pub url: String,
    pub description: String,
    pub language: String,
    pub stars: u64,
}

#[derive(Debug, Deserialize)]
struct GitHubRepo {
    full_name: String,
    html_url: String,
    description: Option<String>,
    language: Option<String>,
    stargazers_count: u64,
}

/// Export the selected user's starred repositories.
///
/// # Errors
///
/// Returns an error when the username or HTTP client cannot be resolved, the
/// GitHub API request or response fails, or the selected output cannot be
/// rendered and written.
pub async fn run(args: StarsArgs, settings: &clix_core::settings::Settings) -> Result<()> {
    validate_explicit_username(args.username.as_deref())?;
    let Some(username) = config::resolve_username(args.username, &settings.github).await else {
        bail!(
            "could not detect GitHub username. Provide it via `clix gh stars <username>`, \
             [github] username in ~/.config/clix/config.toml, or `gh auth login`."
        );
    };

    let token = config::resolve_token(args.token, &settings.github).await;

    let output_path = args.output.unwrap_or_else(|| match args.format {
        OutputFormat::Markdown => PathBuf::from(format!("{username}_starred_repos.md")),
        OutputFormat::Urls => PathBuf::from(format!("{username}_starred_urls.txt")),
        OutputFormat::Json => PathBuf::from(format!("{username}_starred_repos.json")),
    });

    let client = reqwest::Client::builder()
        .user_agent(concat!("clix/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(45))
        .build()
        .context("failed to build HTTP client")?;

    let spinner = ui::create_spinner(&format!("fetching starred repos for @{username}..."));

    let mut repos = Vec::new();
    let mut page = 1;
    let per_page = 100;

    loop {
        spinner.set_message(format!(
            "fetching page {page} for @{username} ({})",
            ui::style_bold(&format!("{} fetched", repos.len()))
        ));

        let mut url = reqwest::Url::parse("https://api.github.com")
            .context("GitHub API base URL should be valid")?;
        url.path_segments_mut()
            .map_err(|()| anyhow::anyhow!("GitHub API base URL cannot accept path segments"))?
            .extend(["users", username.as_str(), "starred"]);
        url.query_pairs_mut()
            .append_pair("per_page", &per_page.to_string())
            .append_pair("page", &page.to_string());
        let resp = send_with_retry(|| {
            let mut request = client
                .get(url.clone())
                .header(ACCEPT, "application/vnd.github+json")
                .header("x-github-api-version", "2022-11-28");
            if let Some(token) = &token {
                request = request.header(AUTHORIZATION, format!("Bearer {token}"));
            }
            request
        })
        .await
        .context("failed to send GitHub API request")?;
        let status = resp.status();

        if !status.is_success() {
            spinner.finish_and_clear();
            let body = read_error_response(resp).await;
            bail!("{}", github_api_error(status, &username, &body));
        }

        let has_next_page = has_next_link(resp.headers());
        let items: Vec<GitHubRepo> = resp
            .json()
            .await
            .context("failed to parse GitHub API JSON response")?;
        let fetched_count = items.len();

        for item in items {
            repos.push(StarredRepo {
                name: item.full_name,
                url: item.html_url,
                description: item.description.unwrap_or_default(),
                language: item.language.unwrap_or_default(),
                stars: item.stargazers_count,
            });
        }

        if !has_next_page || fetched_count == 0 {
            break;
        }
        page += 1;
    }

    spinner.finish_and_clear();

    write_output(&repos, &username, &output_path, args.format)?;

    ui::success(format!(
        "exported {} repos for @{} to {}",
        ui::style_bold(&repos.len().to_string()),
        ui::style_bold(&username),
        ui::style_bold(&output_path.display().to_string())
    ));

    Ok(())
}

async fn read_error_response(mut response: reqwest::Response) -> String {
    let byte_limit = ERROR_BODY_CHARACTER_LIMIT * 4 + 1;
    let mut bytes = Vec::with_capacity(byte_limit);
    let mut was_truncated = false;

    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let remaining = byte_limit.saturating_sub(bytes.len());
                if chunk.len() > remaining {
                    bytes.extend_from_slice(&chunk[..remaining]);
                    was_truncated = true;
                    break;
                }
                bytes.extend_from_slice(&chunk);
                if bytes.len() == byte_limit {
                    was_truncated = true;
                    break;
                }
            }
            Ok(None) => break,
            Err(error) if bytes.is_empty() => {
                return format!("<failed to read GitHub error response: {error}>");
            }
            Err(_) => {
                was_truncated = true;
                break;
            }
        }
    }

    let body = String::from_utf8_lossy(&bytes);
    let mut excerpt = truncate_error_body(&body).into_owned();
    if was_truncated && !excerpt.ends_with('…') {
        excerpt.push('…');
    }
    excerpt
}

async fn send_with_retry(
    request: impl Fn() -> reqwest::RequestBuilder,
) -> reqwest::Result<reqwest::Response> {
    let mut attempt = 1;
    loop {
        match request().send().await {
            Ok(response) => {
                let delay = (attempt < MAX_REQUEST_ATTEMPTS)
                    .then(|| retry_delay(response.status(), response.headers(), attempt))
                    .flatten();
                if let Some(delay) = delay {
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                    continue;
                }
                return Ok(response);
            }
            Err(error)
                if attempt < MAX_REQUEST_ATTEMPTS && (error.is_connect() || error.is_timeout()) =>
            {
                tokio::time::sleep(exponential_backoff(attempt)).await;
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

fn retry_delay(status: StatusCode, headers: &HeaderMap, attempt: u32) -> Option<Duration> {
    if status == StatusCode::TOO_MANY_REQUESTS {
        let seconds = headers
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(1);
        return (seconds <= MAX_RETRY_AFTER_SECONDS).then(|| Duration::from_secs(seconds));
    }

    matches!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
    .then(|| exponential_backoff(attempt))
}

const fn exponential_backoff(attempt: u32) -> Duration {
    Duration::from_millis(250 * (1_u64 << attempt.saturating_sub(1)))
}

fn validate_explicit_username(username: Option<&str>) -> Result<()> {
    let Some(username) = username else {
        return Ok(());
    };
    if config::is_valid_github_login(username.trim()) {
        return Ok(());
    }

    bail!(
        "invalid GitHub username `{username}`: expected 1–39 ASCII letters, digits, or single hyphens, without leading or trailing hyphens"
    )
}

fn write_output(
    repos: &[StarredRepo],
    username: &str,
    path: &Path,
    format: OutputFormat,
) -> Result<()> {
    let content = render_output(repos, username, format)?;
    atomic_write(path, content.as_bytes())
        .with_context(|| format!("failed to write GitHub stars output {}", path.display()))
}

fn render_output(repos: &[StarredRepo], username: &str, format: OutputFormat) -> Result<String> {
    let content = match format {
        OutputFormat::Markdown => {
            let mut content = String::with_capacity(repos.len().saturating_mul(128) + 128);
            writeln!(content, "# GitHub Starred Repositories (@{username})\n")
                .context("failed to render Markdown heading")?;
            writeln!(content, "Total: {} repositories\n", repos.len())
                .context("failed to render Markdown total")?;
            content.push_str("| Repository | Language | Description |\n");
            content.push_str("| --- | --- | --- |\n");
            for repo in repos {
                let lang = if repo.language.is_empty() {
                    "-"
                } else {
                    &repo.language
                };
                writeln!(
                    content,
                    "| [{}]({}) | {} | {} |",
                    escape_markdown_link_label(&repo.name),
                    repo.url,
                    escape_markdown_table_cell(lang),
                    escape_markdown_table_cell(&repo.description)
                )
                .context("failed to render Markdown repository row")?;
            }
            content
        }
        OutputFormat::Urls => {
            let mut content = String::with_capacity(repos.len().saturating_mul(64));
            for repo in repos {
                writeln!(content, "{}", repo.url).context("failed to render URL output")?;
            }
            content
        }
        OutputFormat::Json => {
            serde_json::to_string_pretty(repos).context("failed to serialize JSON output")?
        }
    };
    Ok(content)
}

fn github_api_error(status: StatusCode, username: &str, body: &str) -> String {
    if status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::FORBIDDEN && contains_rate_limit(body))
    {
        return "GitHub API rate limit exceeded. Set GITHUB_TOKEN or GH_TOKEN, or authenticate via `gh auth login`.".to_string();
    }
    if status == StatusCode::NOT_FOUND {
        return format!("GitHub user @{username} not found.");
    }
    format!(
        "GitHub API error (HTTP {status}): {}",
        truncate_error_body(body)
    )
}

fn contains_rate_limit(body: &str) -> bool {
    const NEEDLE: &[u8] = b"rate limit";

    body.as_bytes()
        .windows(NEEDLE.len())
        .any(|window| window.eq_ignore_ascii_case(NEEDLE))
}

fn truncate_error_body(body: &str) -> Cow<'_, str> {
    let Some((cut_index, _)) = body.char_indices().nth(ERROR_BODY_CHARACTER_LIMIT) else {
        return Cow::Borrowed(body);
    };

    let mut excerpt = String::with_capacity(cut_index + '…'.len_utf8());
    excerpt.push_str(&body[..cut_index]);
    excerpt.push('…');
    Cow::Owned(excerpt)
}

fn has_next_link(headers: &HeaderMap) -> bool {
    headers
        .get_all(LINK)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|link| link.split(';').any(|part| part.trim() == r#"rel="next""#))
}

fn escape_markdown_link_label(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '[' => escaped.push_str("\\["),
            ']' => escaped.push_str("\\]"),
            '|' => escaped.push_str("\\|"),
            '\r' | '\n' => escaped.push(' '),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn escape_markdown_table_cell(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '|' => escaped.push_str("\\|"),
            '\r' | '\n' => escaped.push(' '),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use std::fs;

    use reqwest::StatusCode;
    use reqwest::header::{HeaderMap, HeaderValue, LINK, RETRY_AFTER};

    use super::{
        ERROR_BODY_CHARACTER_LIMIT, OutputFormat, StarredRepo, github_api_error, has_next_link,
        render_output, retry_delay, truncate_error_body, validate_explicit_username, write_output,
    };

    fn repository() -> StarredRepo {
        StarredRepo {
            name: "owner/repo[one]|two".into(),
            url: "https://github.com/owner/repo".into(),
            description: "first line\nsecond | line".into(),
            language: "Rust | C".into(),
            stars: 42,
        }
    }

    #[test]
    fn parses_next_relation_from_github_link_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            LINK,
            HeaderValue::from_static(
                r#"<https://api.github.com/resource?page=2>; rel="next", <https://api.github.com/resource?page=4>; rel="last""#,
            ),
        );
        assert!(has_next_link(&headers));

        headers.insert(
            LINK,
            HeaderValue::from_static(r#"<https://api.github.com/resource?page=1>; rel="prev""#),
        );
        assert!(!has_next_link(&headers));
    }

    #[test]
    fn retries_only_short_transient_failures() {
        let mut headers = HeaderMap::new();
        assert_eq!(
            retry_delay(StatusCode::SERVICE_UNAVAILABLE, &headers, 1),
            Some(std::time::Duration::from_millis(250))
        );
        assert_eq!(retry_delay(StatusCode::BAD_REQUEST, &headers, 1), None);

        headers.insert(RETRY_AFTER, HeaderValue::from_static("2"));
        assert_eq!(
            retry_delay(StatusCode::TOO_MANY_REQUESTS, &headers, 1),
            Some(std::time::Duration::from_secs(2))
        );
        headers.insert(RETRY_AFTER, HeaderValue::from_static("60"));
        assert_eq!(
            retry_delay(StatusCode::TOO_MANY_REQUESTS, &headers, 1),
            None,
            "the CLI must not sleep for an unexpectedly long server delay"
        );
    }

    #[test]
    fn classifies_api_errors_and_truncates_large_unicode_bodies() {
        let rate_limit =
            github_api_error(StatusCode::FORBIDDEN, "octocat", "API RATE LIMIT exceeded");
        assert!(rate_limit.starts_with("GitHub API rate limit exceeded."));
        assert!(
            github_api_error(StatusCode::TOO_MANY_REQUESTS, "octocat", "")
                .starts_with("GitHub API rate limit exceeded.")
        );
        assert_eq!(
            github_api_error(StatusCode::NOT_FOUND, "octocat", "ignored"),
            "GitHub user @octocat not found."
        );

        let oversized = "界".repeat(ERROR_BODY_CHARACTER_LIMIT + 5);
        let excerpt = truncate_error_body(&oversized);
        assert_eq!(excerpt.chars().count(), ERROR_BODY_CHARACTER_LIMIT + 1);
        assert!(excerpt.ends_with('…'));
        let error = github_api_error(StatusCode::INTERNAL_SERVER_ERROR, "octocat", &oversized);
        assert!(error.ends_with('…'));
        assert!(!error.contains(&"界".repeat(ERROR_BODY_CHARACTER_LIMIT + 1)));
    }

    #[test]
    fn rejects_an_explicit_invalid_username_without_falling_back() {
        assert!(validate_explicit_username(None).is_ok());
        assert!(validate_explicit_username(Some(" octocat ")).is_ok());

        let error = validate_explicit_username(Some("John Doe"))
            .expect_err("a display name must not be accepted as a GitHub login");
        assert!(error.to_string().starts_with("invalid GitHub username"));
    }

    #[test]
    fn renders_all_formats_and_escapes_markdown_cells() {
        let repository = repository();

        let markdown = render_output(
            std::slice::from_ref(&repository),
            "octocat",
            OutputFormat::Markdown,
        )
        .expect("Markdown should render");
        assert_eq!(
            markdown,
            "# GitHub Starred Repositories (@octocat)\n\n\
             Total: 1 repositories\n\n\
             | Repository | Language | Description |\n\
             | --- | --- | --- |\n\
             | [owner/repo\\[one\\]\\|two](https://github.com/owner/repo) | Rust \\| C | first line second \\| line |\n"
        );

        let urls = render_output(
            std::slice::from_ref(&repository),
            "octocat",
            OutputFormat::Urls,
        )
        .expect("URLs should render");
        assert_eq!(urls, "https://github.com/owner/repo\n");

        let json = render_output(&[repository], "octocat", OutputFormat::Json)
            .expect("JSON should render");
        let value: serde_json::Value = serde_json::from_str(&json).expect("JSON should parse");
        assert_eq!(value[0]["stars"], 42);
    }

    #[test]
    fn writes_the_rendered_output_to_the_selected_path() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join("stars.md");
        write_output(&[repository()], "octocat", &path, OutputFormat::Markdown)
            .expect("Markdown should be written");

        let written = fs::read_to_string(path).expect("Markdown should be readable");
        assert!(written.starts_with("# GitHub Starred Repositories (@octocat)"));
    }
}
