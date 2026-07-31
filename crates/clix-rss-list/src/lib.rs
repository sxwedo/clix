use std::{fmt::Write as _, path::PathBuf};

use anyhow::{Context, Result};
use clap::Args;
use clix_core::ui;
use clix_rss_store::{EntryQuery, QueryResult, RssStore, default_state_path};

const DEFAULT_LIST_LIMIT: usize = 20;

/// Arguments accepted by `clix rss list` and `clix-rss-list`.
#[derive(Debug, Args)]
pub struct ListArgs {
    /// List only named stored subscriptions; repeat or use comma-separated names
    #[arg(long = "feed", value_name = "NAME", value_delimiter = ',')]
    pub feeds: Vec<String>,

    /// redb database path (default: configured `[rss].state` or `~/.config/clix/rss.redb`)
    #[arg(long, value_name = "PATH")]
    pub state: Option<PathBuf>,

    /// Maximum newest entries displayed across all selected feeds (default: 20)
    #[arg(short = 'n', long, value_name = "COUNT")]
    pub limit: Option<usize>,
}

/// Read the local RSS archive and print a compact newest-first terminal view.
///
/// # Errors
///
/// Returns an error when the state is absent, a filter is invalid, or the
/// database cannot be read.
pub fn run(args: ListArgs, settings: &clix_core::settings::Settings) -> Result<()> {
    let state_path = args
        .state
        .or_else(|| settings.rss.state.clone())
        .unwrap_or_else(default_state_path);
    let store = RssStore::open(&state_path)?;
    let result = store.query(&EntryQuery {
        feeds: args.feeds,
        since: None,
        limit: Some(args.limit.unwrap_or(DEFAULT_LIST_LIMIT)),
    })?;
    print!("{}", render_list(&result, &state_path)?);
    Ok(())
}

fn render_list(result: &QueryResult, state_path: &std::path::Path) -> Result<String> {
    let mut output = String::new();
    writeln!(output, "{}", ui::style_cyan_bold("RSS Archive"))
        .context("failed to render RSS archive heading")?;
    writeln!(output, "Database: {}", state_path.display())
        .context("failed to render RSS database path")?;
    writeln!(
        output,
        "Entries: showing {} of {} matched ({} stored)\n",
        result.entries.len(),
        result.matched_entries,
        result.database_entries
    )
    .context("failed to render RSS entry counts")?;

    if result.entries.is_empty() {
        output.push_str("No stored entries matched the query.\n");
        return Ok(output);
    }

    for (index, stored) in result.entries.iter().enumerate() {
        writeln!(
            output,
            "{}. [{}] {}",
            index + 1,
            stored.archive_timestamp(),
            ui::style_bold(&one_line(&stored.entry.title))
        )
        .context("failed to render RSS entry title")?;
        writeln!(
            output,
            "   Feed: {}",
            ui::style_dim(&one_line(&stored.subscription))
        )
        .context("failed to render RSS subscription")?;
        if let Some(url) = &stored.entry.url {
            writeln!(output, "   URL: {url}").context("failed to render RSS entry URL")?;
        }
        output.push('\n');
    }
    Ok(output)
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use clix_rss_api::{FetchedEntry, FetchedFeed};

    use super::*;

    #[test]
    fn lists_newest_entries_from_the_store_with_counts() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("rss.redb");
        let store = RssStore::open_or_create(&path).expect("store");
        store
            .upsert_feeds(
                &[FetchedFeed {
                    subscription: "Example\nFeed".to_string(),
                    source_url: "https://example.com/feed".to_string(),
                    title: "Example".to_string(),
                    feed_type: "RSS2".to_string(),
                    site_url: None,
                    updated_at: None,
                    entries: vec![
                        entry("old", "Old entry", "2026-07-30T00:00:00Z"),
                        entry("new", "New\nentry", "2026-07-31T00:00:00Z"),
                    ],
                }],
                "2026-07-31T01:00:00Z",
            )
            .expect("sync");
        let result = store
            .query(&EntryQuery {
                limit: Some(1),
                ..EntryQuery::default()
            })
            .expect("query");

        let rendered = render_list(&result, &path).expect("render");
        assert!(rendered.contains("showing 1 of 2 matched (2 stored)"));
        assert!(rendered.contains("New entry"));
        assert!(rendered.contains("Feed: Example Feed"));
        assert!(rendered.contains("https://example.com/new"));
        assert!(!rendered.contains("Old entry"));
    }

    fn entry(id: &str, title: &str, published_at: &str) -> FetchedEntry {
        FetchedEntry {
            id: id.to_string(),
            title: title.to_string(),
            url: Some(format!("https://example.com/{id}")),
            published_at: Some(published_at.to_string()),
            authors: Vec::new(),
            categories: Vec::new(),
            summary: None,
        }
    }
}
