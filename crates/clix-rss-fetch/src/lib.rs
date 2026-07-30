use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use clix_core::ui;

mod fetch;
mod model;
mod output;
mod subscription;

use fetch::fetch_subscriptions;
use model::RssExport;
use output::{default_output_path, resolve_format, write_output};
use subscription::select_subscriptions;

const DEFAULT_ENTRY_LIMIT: usize = 20;

/// Supported RSS snapshot formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Markdown,
    Json,
}

/// Arguments accepted by `clix rss fetch` and `clix-rss-fetch`.
#[derive(Debug, Args)]
pub struct FetchArgs {
    /// Fetch only named subscriptions; repeat the flag or use comma-separated names
    #[arg(long = "feed", value_name = "NAME", value_delimiter = ',')]
    pub feeds: Vec<String>,

    /// Output path (default: configured `[rss].output` or `rss.md`/`rss.json`)
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Output format; inferred as JSON when the output path ends in .json
    #[arg(short, long, value_enum)]
    pub format: Option<OutputFormat>,

    /// Maximum entries retained per feed (default: configured `[rss].limit` or 20)
    #[arg(short = 'n', long, value_name = "COUNT")]
    pub limit: Option<usize>,
}

/// Fetch configured subscriptions and write one Markdown or JSON snapshot.
///
/// # Errors
///
/// Returns an error when subscriptions are absent or invalid, every selected
/// feed fails, the requested format cannot be rendered, or the output cannot
/// be persisted.
pub async fn run(args: FetchArgs, settings: &clix_core::settings::Settings) -> Result<()> {
    let subscriptions = select_subscriptions(&settings.rss, &args.feeds)?;
    let limit = args
        .limit
        .or(settings.rss.limit)
        .unwrap_or(DEFAULT_ENTRY_LIMIT);
    if limit == 0 {
        bail!("RSS entry limit must be greater than zero");
    }

    let configured_output = args.output.or_else(|| settings.rss.output.clone());
    let format = resolve_format(args.format, configured_output.as_deref());
    let output_path = configured_output.unwrap_or_else(|| default_output_path(format));

    let client = reqwest::Client::builder()
        .user_agent(concat!("clix/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .context("failed to build RSS HTTP client")?;

    let spinner = ui::create_spinner(&format!(
        "fetching {} RSS subscription(s)...",
        subscriptions.len()
    ));
    let (feeds, failures) = fetch_subscriptions(&client, subscriptions, limit).await;
    spinner.finish_and_clear();

    for failure in &failures {
        ui::warn(format!(
            "RSS subscription {} failed: {}",
            failure.subscription, failure.error
        ));
    }
    if feeds.is_empty() {
        let details = failures
            .iter()
            .map(|failure| format!("{}: {}", failure.subscription, failure.error))
            .collect::<Vec<_>>()
            .join("; ");
        bail!("all selected RSS subscriptions failed: {details}");
    }

    let export = RssExport::new(feeds);
    write_output(&export, &output_path, format)?;

    ui::success(format!(
        "fetched {} entries from {} feed(s) to {}{}",
        ui::style_bold(&export.entry_count.to_string()),
        ui::style_bold(&export.feed_count.to_string()),
        ui::style_bold(&output_path.display().to_string()),
        if failures.is_empty() {
            String::new()
        } else {
            format!(" ({} failed)", failures.len())
        }
    ));
    Ok(())
}

#[cfg(test)]
mod tests;
