use std::{collections::HashSet, path::PathBuf};

use anyhow::{Result, bail};
use chrono::{SecondsFormat, Utc};
use clap::Args;
use clix_core::ui;
use clix_rss_api::{DEFAULT_ENTRY_LIMIT, build_client, fetch_subscriptions, select_subscriptions};
use clix_rss_push::PushArgs;
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

    /// Push to these destinations after local sync; overrides configured `[rss].push_to`
    #[arg(long = "push-to", value_name = "DESTINATION", value_delimiter = ',')]
    pub push_to: Vec<String>,

    /// Skip every configured or explicitly requested remote push
    #[arg(long, conflicts_with = "push_to")]
    pub no_push: bool,
}

/// Fetch configured subscriptions and incrementally upsert normalized entries into redb.
///
/// # Errors
///
/// Returns an error when subscriptions are invalid, every selected feed fails,
/// the state database cannot be opened, the atomic upsert fails, or any
/// configured destination push fails after the local commit.
pub async fn run(args: SyncArgs, settings: &clix_core::settings::Settings) -> Result<()> {
    let push_destinations = select_push_destinations(&args, settings);
    let push_feeds = args.feeds.clone();
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
    drop(store);

    push_synced_entries(push_destinations, push_feeds, state_path, settings).await
}

fn select_push_destinations(
    args: &SyncArgs,
    settings: &clix_core::settings::Settings,
) -> Vec<String> {
    if args.no_push {
        return Vec::new();
    }
    let configured = if args.push_to.is_empty() {
        &settings.rss.push_to
    } else {
        &args.push_to
    };
    let mut seen = HashSet::new();
    configured
        .iter()
        .map(|name| name.trim().to_string())
        .filter(|name| seen.insert(name.clone()))
        .collect()
}

async fn push_synced_entries(
    destinations: Vec<String>,
    feeds: Vec<String>,
    state_path: PathBuf,
    settings: &clix_core::settings::Settings,
) -> Result<()> {
    let mut failures = Vec::new();
    for destination in destinations {
        let result = clix_rss_push::run(
            PushArgs {
                destination: destination.clone(),
                state: Some(state_path.clone()),
                feeds: feeds.clone(),
                limit: None,
                dry_run: false,
                force: false,
            },
            settings,
        )
        .await;
        if let Err(error) = result {
            ui::warn(format!(
                "RSS destination {destination} failed after local sync: {error:#}"
            ));
            failures.push(format!("{destination}: {error:#}"));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        bail!(
            "RSS entries were synced locally, but {} destination push(es) failed: {}",
            failures.len(),
            failures.join("; ")
        )
    }
}

#[cfg(test)]
mod tests;
