use std::path::PathBuf;

use anyhow::{Result, bail};
use chrono::{SecondsFormat, Utc};
use clap::Args;
use clix_core::ui;
use clix_rss_api::{DEFAULT_ENTRY_LIMIT, build_client, fetch_subscriptions, select_subscriptions};
use clix_rss_store::{RssStore, default_state_path};

/// Arguments accepted by `clix rss sync` and `clix-rss-sync`.
#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Sync only named subscriptions; repeat the flag or use comma-separated names
    #[arg(long = "feed", value_name = "NAME", value_delimiter = ',')]
    pub feeds: Vec<String>,

    /// redb database path (default: configured `[rss].state` or `~/.config/clix/rss.redb`)
    #[arg(long, value_name = "PATH")]
    pub state: Option<PathBuf>,

    /// Maximum recent entries read per feed (default: configured `[rss].limit` or 20)
    #[arg(short = 'n', long, value_name = "COUNT")]
    pub limit: Option<usize>,
}

/// Fetch configured subscriptions and incrementally upsert normalized entries into redb.
///
/// # Errors
///
/// Returns an error when subscriptions are invalid, every selected feed fails,
/// the state database cannot be opened, or the atomic upsert fails.
pub async fn run(args: SyncArgs, settings: &clix_core::settings::Settings) -> Result<()> {
    let subscriptions = select_subscriptions(&settings.rss, &args.feeds)?;
    let limit = args
        .limit
        .or(settings.rss.limit)
        .unwrap_or(DEFAULT_ENTRY_LIMIT);
    if limit == 0 {
        bail!("RSS entry limit must be greater than zero");
    }
    let state_path = args
        .state
        .or_else(|| settings.rss.state.clone())
        .unwrap_or_else(default_state_path);

    let client = build_client()?;
    let spinner = ui::create_spinner(&format!(
        "syncing {} RSS subscription(s)...",
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

    let synced_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let store = RssStore::open_or_create(&state_path)?;
    let stats = store.upsert_feeds(&feeds, &synced_at)?;
    ui::success(format!(
        "synced {} new, {} updated, {} unchanged RSS entries to {} ({} total){}",
        ui::style_bold(&stats.inserted.to_string()),
        ui::style_bold(&stats.updated.to_string()),
        ui::style_bold(&stats.unchanged.to_string()),
        ui::style_bold(&state_path.display().to_string()),
        ui::style_bold(&stats.total.to_string()),
        if failures.is_empty() {
            String::new()
        } else {
            format!(" ({} feed(s) failed)", failures.len())
        }
    ));
    Ok(())
}

#[cfg(test)]
mod tests;
