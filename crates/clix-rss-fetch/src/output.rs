use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clix_core::fs::{atomic_write, parent_or_current};

use crate::{OutputFormat, model::RssExport};

pub fn resolve_format(explicit: Option<OutputFormat>, output: Option<&Path>) -> OutputFormat {
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

pub fn default_output_path(format: OutputFormat) -> PathBuf {
    PathBuf::from(match format {
        OutputFormat::Markdown => "rss.md",
        OutputFormat::Json => "rss.json",
    })
}

pub fn write_output(export: &RssExport, path: &Path, format: OutputFormat) -> Result<()> {
    let content = match format {
        OutputFormat::Markdown => render_markdown(export)?,
        OutputFormat::Json => {
            serde_json::to_string_pretty(export).context("failed to serialize RSS JSON output")?
        }
    };
    let parent = parent_or_current(path);
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create RSS output directory {}", parent.display()))?;
    atomic_write(path, content.as_bytes())
        .with_context(|| format!("failed to write RSS output {}", path.display()))
}

pub fn render_markdown(export: &RssExport) -> Result<String> {
    let mut output = String::new();
    output.push_str("# RSS Subscriptions\n\n");
    writeln!(output, "- Fetched: {}", export.fetched_at)
        .context("failed to render RSS fetch time")?;
    writeln!(output, "- Feeds: {}", export.feed_count)
        .context("failed to render RSS feed count")?;
    writeln!(output, "- Entries: {}\n", export.entry_count)
        .context("failed to render RSS entry count")?;

    for feed in &export.feeds {
        writeln!(output, "## {}\n", escape_markdown_text(&feed.title))
            .context("failed to render RSS feed title")?;
        writeln!(
            output,
            "- Subscription: {}",
            escape_markdown_text(&feed.subscription)
        )
        .context("failed to render RSS subscription name")?;
        writeln!(output, "- Feed: <{}>", feed.source_url)
            .context("failed to render RSS source URL")?;
        if let Some(site_url) = &feed.site_url {
            writeln!(output, "- Website: <{site_url}>")
                .context("failed to render RSS website URL")?;
        }
        if let Some(updated_at) = &feed.updated_at {
            writeln!(output, "- Updated: {updated_at}")
                .context("failed to render RSS update time")?;
        }
        output.push('\n');

        if feed.entries.is_empty() {
            output.push_str("_No entries found._\n\n");
            continue;
        }
        for entry in &feed.entries {
            if let Some(url) = &entry.url {
                writeln!(
                    output,
                    "### [{}](<{}>)\n",
                    escape_markdown_link_label(&entry.title),
                    url
                )
                .context("failed to render RSS entry link")?;
            } else {
                writeln!(output, "### {}\n", escape_markdown_text(&entry.title))
                    .context("failed to render RSS entry title")?;
            }
            if let Some(published_at) = &entry.published_at {
                writeln!(output, "- Published: {published_at}")
                    .context("failed to render RSS publication time")?;
            }
            if !entry.authors.is_empty() {
                writeln!(
                    output,
                    "- Authors: {}",
                    escape_markdown_text(&entry.authors.join(", "))
                )
                .context("failed to render RSS authors")?;
            }
            if !entry.categories.is_empty() {
                writeln!(
                    output,
                    "- Categories: {}",
                    escape_markdown_text(&entry.categories.join(", "))
                )
                .context("failed to render RSS categories")?;
            }
            if let Some(summary) = &entry.summary {
                output.push('\n');
                for line in summary.lines() {
                    writeln!(output, "> {line}").context("failed to render RSS summary")?;
                }
            }
            output.push('\n');
        }
    }
    Ok(output)
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
