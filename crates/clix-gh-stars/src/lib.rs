use std::{fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use clix_core::{config, ui};
use reqwest::header::{ACCEPT, AUTHORIZATION};
use serde::{Deserialize, Serialize};

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

    /// Output file path (default: <username>_starred_repos.md)
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Output format: markdown, urls, or json
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Markdown)]
    pub format: OutputFormat,

    /// GitHub token (auto-detected via GH_TOKEN env or `gh auth token`)
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

pub async fn run(args: StarsArgs) -> Result<()> {
    let username = match config::resolve_username(args.username).await {
        Some(user) => user,
        None => bail!(
            "could not detect GitHub username. Please pass it as an argument: `clix gh stars <username>`"
        ),
    };

    let token = config::resolve_token(args.token).await;

    let output_path = args.output.unwrap_or_else(|| match args.format {
        OutputFormat::Markdown => PathBuf::from(format!("{username}_starred_repos.md")),
        OutputFormat::Urls => PathBuf::from(format!("{username}_starred_urls.txt")),
        OutputFormat::Json => PathBuf::from(format!("{username}_starred_repos.json")),
    });

    let client = reqwest::Client::builder()
        .user_agent("clix-cli")
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

        let url = format!(
            "https://api.github.com/users/{username}/starred?per_page={per_page}&page={page}"
        );
        let mut req = client
            .get(&url)
            .header(ACCEPT, "application/vnd.github.v3+json");

        if let Some(tok) = &token {
            req = req.header(AUTHORIZATION, format!("bearer {tok}"));
        }

        let resp = req
            .send()
            .await
            .context("failed to send GitHub API request")?;
        let status = resp.status();

        if !status.is_success() {
            spinner.finish_and_clear();
            let body = resp.text().await.unwrap_or_default();
            if status.as_u16() == 403 && body.to_lowercase().contains("rate limit") {
                bail!(
                    "GitHub API rate limit exceeded. Set GITHUB_TOKEN or authenticate via `gh auth login`."
                );
            } else if status.as_u16() == 404 {
                bail!("GitHub user @{username} not found.");
            } else {
                bail!("GitHub API error (HTTP {status}): {body}");
            }
        }

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

        if fetched_count < per_page {
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

fn write_output(
    repos: &[StarredRepo],
    username: &str,
    path: &PathBuf,
    format: OutputFormat,
) -> Result<()> {
    match format {
        OutputFormat::Markdown => {
            let mut content = String::new();
            content.push_str(&format!("# GitHub Starred Repositories (@{username})\n\n"));
            content.push_str(&format!("Total: {} repositories\n\n", repos.len()));
            content.push_str("| Repository | Language | Description |\n");
            content.push_str("| --- | --- | --- |\n");
            for repo in repos {
                let lang = if repo.language.is_empty() {
                    "-"
                } else {
                    &repo.language
                };
                let desc = repo.description.replace('\n', " ").replace('|', "\\|");
                content.push_str(&format!(
                    "| [{}]({}) | {} | {} |\n",
                    repo.name, repo.url, lang, desc
                ));
            }
            fs::write(path, content).context("failed to write Markdown output")?;
        }
        OutputFormat::Urls => {
            let mut content = String::new();
            for repo in repos {
                content.push_str(&format!("{}\n", repo.url));
            }
            fs::write(path, content).context("failed to write URLs output")?;
        }
        OutputFormat::Json => {
            let json =
                serde_json::to_string_pretty(repos).context("failed to serialize JSON output")?;
            fs::write(path, json).context("failed to write JSON output")?;
        }
    }
    Ok(())
}
