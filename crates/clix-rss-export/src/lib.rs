use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use clap::{Args, ValueEnum};
use clix_core::{
    fs::{atomic_write, parent_or_current},
    ui,
};
use clix_rss_store::{EntryQuery, QueryResult, RssStore, StoredEntry, default_state_path};
use serde::Serialize;

/// Supported RSS archive export formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Markdown,
    Json,
}

/// Arguments accepted by `clix rss export` and `clix-rss-export`.
#[derive(Debug, Args)]
pub struct ExportArgs {
    /// Export only named stored subscriptions; repeat or use comma-separated names
    #[arg(long = "feed", value_name = "NAME", value_delimiter = ',')]
    pub feeds: Vec<String>,

    /// redb database path (default: configured `[rss].state` or `~/.config/clix/rss.redb`)
    #[arg(long, value_name = "PATH")]
    pub state: Option<PathBuf>,

    /// Output path (default: `rss-export.md` or `rss-export.json`)
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Output format; inferred as JSON when the output path ends in .json
    #[arg(short, long, value_enum)]
    pub format: Option<OutputFormat>,

    /// Maximum newest entries exported across all selected feeds
    #[arg(short = 'n', long, value_name = "COUNT")]
    pub limit: Option<usize>,

    /// Include entries since a duration (`7d`, `24h`, `2w`, `30m`) or RFC 3339 time
    #[arg(long, value_name = "TIME")]
    pub since: Option<String>,
}

#[derive(Serialize)]
struct ArchiveExport<'a> {
    exported_at: String,
    database: String,
    database_entry_count: usize,
    matched_entry_count: usize,
    entry_count: usize,
    entries: &'a [StoredEntry],
}

/// Read the local RSS archive and atomically export Markdown or JSON.
///
/// # Errors
///
/// Returns an error when the state is absent, a filter is invalid, output and
/// state collide, rendering fails, or the output cannot be written.
pub fn run(args: ExportArgs, settings: &clix_core::settings::Settings) -> Result<()> {
    let state_path = args
        .state
        .or_else(|| settings.rss.state.clone())
        .unwrap_or_else(default_state_path);
    let format = resolve_format(args.format, args.output.as_deref());
    let output_path = args.output.unwrap_or_else(|| default_output_path(format));
    ensure_distinct_paths(&output_path, &state_path)?;

    let since = args
        .since
        .as_deref()
        .map(|value| parse_since(value, Utc::now()))
        .transpose()?;
    let store = RssStore::open(&state_path)?;
    let result = store.query(&EntryQuery {
        feeds: args.feeds,
        since,
        limit: args.limit,
    })?;
    write_output(&result, &state_path, &output_path, format)?;

    ui::success(format!(
        "exported {} RSS entries from {} to {}",
        ui::style_bold(&result.entries.len().to_string()),
        ui::style_bold(&state_path.display().to_string()),
        ui::style_bold(&output_path.display().to_string())
    ));
    Ok(())
}

fn resolve_format(explicit: Option<OutputFormat>, output: Option<&Path>) -> OutputFormat {
    explicit.unwrap_or_else(|| {
        if output
            .and_then(Path::extension)
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        {
            OutputFormat::Json
        } else {
            OutputFormat::Markdown
        }
    })
}

fn default_output_path(format: OutputFormat) -> PathBuf {
    PathBuf::from(match format {
        OutputFormat::Markdown => "rss-export.md",
        OutputFormat::Json => "rss-export.json",
    })
}

fn write_output(
    result: &QueryResult,
    state_path: &Path,
    output_path: &Path,
    format: OutputFormat,
) -> Result<()> {
    let exported_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let content = match format {
        OutputFormat::Markdown => render_markdown(result, state_path, &exported_at)?,
        OutputFormat::Json => render_json(result, state_path, &exported_at)?,
    };
    let parent = parent_or_current(output_path);
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create RSS export directory {}", parent.display()))?;
    atomic_write(output_path, content.as_bytes())
        .with_context(|| format!("failed to write RSS export {}", output_path.display()))
}

fn render_json(result: &QueryResult, state_path: &Path, exported_at: &str) -> Result<String> {
    let export = ArchiveExport {
        exported_at: exported_at.to_string(),
        database: state_path.display().to_string(),
        database_entry_count: result.database_entries,
        matched_entry_count: result.matched_entries,
        entry_count: result.entries.len(),
        entries: &result.entries,
    };
    let mut content =
        serde_json::to_string_pretty(&export).context("failed to serialize RSS archive JSON")?;
    content.push('\n');
    Ok(content)
}

fn render_markdown(result: &QueryResult, state_path: &Path, exported_at: &str) -> Result<String> {
    let mut output = String::new();
    output.push_str("# RSS Archive\n\n");
    writeln!(output, "- Exported: {exported_at}").context("failed to render RSS export time")?;
    writeln!(output, "- Database: `{}`", state_path.display())
        .context("failed to render RSS database path")?;
    writeln!(output, "- Stored entries: {}", result.database_entries)
        .context("failed to render RSS database count")?;
    writeln!(output, "- Matched entries: {}", result.matched_entries)
        .context("failed to render RSS matched count")?;
    writeln!(output, "- Exported entries: {}\n", result.entries.len())
        .context("failed to render RSS export count")?;

    if result.entries.is_empty() {
        output.push_str("_No stored entries matched the query._\n");
        return Ok(output);
    }

    for stored in &result.entries {
        if let Some(url) = &stored.entry.url {
            writeln!(
                output,
                "## [{}](<{}>)\n",
                escape_markdown_link_label(&stored.entry.title),
                url
            )
            .context("failed to render RSS entry link")?;
        } else {
            writeln!(output, "## {}\n", escape_markdown_text(&stored.entry.title))
                .context("failed to render RSS entry title")?;
        }
        writeln!(
            output,
            "- Feed: {}",
            escape_markdown_text(&stored.subscription)
        )
        .context("failed to render RSS subscription")?;
        writeln!(output, "- Feed source: <{}>", stored.source_url)
            .context("failed to render RSS feed source")?;
        if let Some(published_at) = &stored.entry.published_at {
            writeln!(output, "- Published: {published_at}")
                .context("failed to render RSS publication time")?;
        }
        writeln!(output, "- First seen: {}", stored.first_seen_at)
            .context("failed to render RSS first-seen time")?;
        if !stored.entry.authors.is_empty() {
            writeln!(
                output,
                "- Authors: {}",
                escape_markdown_text(&stored.entry.authors.join(", "))
            )
            .context("failed to render RSS authors")?;
        }
        if !stored.entry.categories.is_empty() {
            writeln!(
                output,
                "- Categories: {}",
                escape_markdown_text(&stored.entry.categories.join(", "))
            )
            .context("failed to render RSS categories")?;
        }
        if let Some(summary) = &stored.entry.summary {
            output.push('\n');
            for line in summary.lines() {
                writeln!(output, "> {line}").context("failed to render RSS summary")?;
            }
        }
        output.push('\n');
    }
    Ok(output)
}

fn parse_since(value: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let value = value.trim();
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(value) {
        return Ok(timestamp.with_timezone(&Utc));
    }

    let (amount, unit) = value.split_at(value.len().saturating_sub(1));
    let amount = amount
        .parse::<i64>()
        .with_context(|| format!("invalid --since value: {value}"))?;
    if amount <= 0 {
        bail!("--since duration must be greater than zero: {value}");
    }
    let duration = match unit {
        "m" => Duration::minutes(amount),
        "h" => Duration::hours(amount),
        "d" => Duration::days(amount),
        "w" => Duration::weeks(amount),
        _ => bail!("invalid --since value: {value}; use 30m, 24h, 7d, 2w, or RFC 3339"),
    };
    now.checked_sub_signed(duration)
        .with_context(|| format!("--since duration is out of range: {value}"))
}

fn ensure_distinct_paths(output_path: &Path, state_path: &Path) -> Result<()> {
    let output = comparable_path(output_path)?;
    let state = comparable_path(state_path)?;
    if output == state {
        bail!(
            "RSS export and state must use different paths (both resolve to {})",
            output.display()
        );
    }
    Ok(())
}

fn comparable_path(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return path
            .canonicalize()
            .with_context(|| format!("failed to resolve path {}", path.display()));
    }
    let file_name = path
        .file_name()
        .with_context(|| format!("path {} has no file name", path.display()))?;
    let parent = parent_or_current(path);
    let resolved_parent = parent
        .canonicalize()
        .or_else(|_| std::path::absolute(parent))
        .with_context(|| format!("failed to resolve parent directory {}", parent.display()))?;
    Ok(resolved_parent.join(file_name))
}

fn escape_markdown_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '`' | '*' | '_' | '[' | ']' | '#' => {
                escaped.push('\\');
                escaped.push(character);
            }
            '\r' | '\n' => escaped.push(' '),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn escape_markdown_link_label(value: &str) -> String {
    escape_markdown_text(value).replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use clix_rss_api::{FetchedEntry, FetchedFeed};

    use super::*;

    #[test]
    fn parses_relative_and_absolute_since_values() {
        let now = DateTime::parse_from_rfc3339("2026-07-31T12:00:00Z")
            .expect("now")
            .with_timezone(&Utc);
        assert_eq!(
            parse_since("7d", now).expect("seven days"),
            DateTime::parse_from_rfc3339("2026-07-24T12:00:00Z")
                .expect("expected")
                .with_timezone(&Utc)
        );
        assert_eq!(
            parse_since("2026-07-01T00:00:00+08:00", now).expect("absolute"),
            DateTime::parse_from_rfc3339("2026-06-30T16:00:00Z")
                .expect("expected")
                .with_timezone(&Utc)
        );
        assert!(parse_since("0d", now).is_err());
        assert!(parse_since("month", now).is_err());
    }

    #[test]
    fn exports_filtered_store_to_markdown_and_json() {
        let directory = tempfile::tempdir().expect("temp dir");
        let state_path = directory.path().join("rss.redb");
        let store = RssStore::open_or_create(&state_path).expect("store");
        store
            .upsert_feeds(&[feed()], "2026-07-31T01:00:00Z")
            .expect("sync");
        let result = store.query(&EntryQuery::default()).expect("query");

        let markdown =
            render_markdown(&result, &state_path, "2026-07-31T02:00:00Z").expect("markdown");
        assert!(markdown.contains("## [New \\[entry\\]](<https://example.com/new>)"));
        assert!(markdown.contains("- Feed: Example"));
        assert!(markdown.contains("> Summary."));

        let json = render_json(&result, &state_path, "2026-07-31T02:00:00Z").expect("json");
        let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(value["database_entry_count"], 1);
        assert_eq!(value["entry_count"], 1);
        assert_eq!(value["entries"][0]["entry"]["id"], "new");
    }

    #[test]
    fn refuses_to_overwrite_the_database() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("rss.redb");
        fs::write(&path, b"state").expect("fixture");
        assert!(ensure_distinct_paths(&path, &path).is_err());
    }

    fn feed() -> FetchedFeed {
        FetchedFeed {
            subscription: "Example".to_string(),
            source_url: "https://example.com/feed".to_string(),
            title: "Example Feed".to_string(),
            feed_type: "RSS2".to_string(),
            site_url: Some("https://example.com/".to_string()),
            updated_at: None,
            entries: vec![FetchedEntry {
                id: "new".to_string(),
                title: "New [entry]".to_string(),
                url: Some("https://example.com/new".to_string()),
                published_at: Some("2026-07-31T00:00:00Z".to_string()),
                authors: vec!["Ada".to_string()],
                categories: vec!["Rust".to_string()],
                summary: Some("Summary.".to_string()),
            }],
        }
    }
}
