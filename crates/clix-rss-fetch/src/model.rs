use chrono::{DateTime, SecondsFormat, Utc};
use feed_rs::model::{Entry, Feed, Text};
use serde::Serialize;

use crate::subscription::Subscription;

const MAX_SUMMARY_CHARACTERS: usize = 1_200;

#[derive(Debug, Serialize)]
pub struct RssExport {
    pub fetched_at: String,
    pub feed_count: usize,
    pub entry_count: usize,
    pub feeds: Vec<FetchedFeed>,
}

impl RssExport {
    pub fn new(feeds: Vec<FetchedFeed>) -> Self {
        let entry_count = feeds.iter().map(|feed| feed.entries.len()).sum();
        Self {
            fetched_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            feed_count: feeds.len(),
            entry_count,
            feeds,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct FetchedFeed {
    pub subscription: String,
    pub source_url: String,
    pub title: String,
    pub feed_type: String,
    pub site_url: Option<String>,
    pub updated_at: Option<String>,
    pub entries: Vec<FetchedEntry>,
}

#[derive(Debug, Serialize)]
pub struct FetchedEntry {
    pub id: String,
    pub title: String,
    pub url: Option<String>,
    pub published_at: Option<String>,
    pub authors: Vec<String>,
    pub categories: Vec<String>,
    pub summary: Option<String>,
}

pub fn normalize_feed(subscription: &Subscription, feed: Feed, limit: usize) -> FetchedFeed {
    let feed_authors = feed
        .authors
        .iter()
        .map(|author| author.name.trim())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let mut entries = feed
        .entries
        .into_iter()
        .enumerate()
        .map(|(index, entry)| {
            let sort_date = entry.published.or(entry.updated);
            (
                sort_date,
                index,
                normalize_entry(entry, &feed_authors, sort_date),
            )
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    let entries = entries
        .into_iter()
        .take(limit)
        .map(|(_, _, entry)| entry)
        .collect();

    let title = feed
        .title
        .as_ref()
        .map(text_content)
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| subscription.name.clone());
    FetchedFeed {
        subscription: subscription.name.clone(),
        source_url: subscription.url.clone(),
        title,
        feed_type: format!("{:?}", feed.feed_type),
        site_url: preferred_link(&feed.links),
        updated_at: feed.updated.map(format_date),
        entries,
    }
}

fn normalize_entry(
    entry: Entry,
    feed_authors: &[String],
    published_at: Option<DateTime<Utc>>,
) -> FetchedEntry {
    let url = preferred_link(&entry.links).or_else(|| safe_web_url(&entry.id));
    let title = entry
        .title
        .as_ref()
        .map(text_content)
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| "(untitled)".to_string());
    let authors = if entry.authors.is_empty() {
        feed_authors.to_vec()
    } else {
        entry
            .authors
            .iter()
            .map(|author| author.name.trim())
            .filter(|name| !name.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    };
    let categories = entry
        .categories
        .iter()
        .map(|category| category.term.trim())
        .filter(|term| !term.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let summary = entry_summary(&entry);

    FetchedEntry {
        id: entry.id,
        title,
        url,
        published_at: published_at.map(format_date),
        authors,
        categories,
        summary,
    }
}

fn entry_summary(entry: &Entry) -> Option<String> {
    if let Some(summary) = &entry.summary {
        return normalize_summary(
            &summary.content,
            media_type_is_html(summary.content_type.as_ref()),
        );
    }
    entry.content.as_ref().and_then(|content| {
        content.body.as_deref().and_then(|body| {
            normalize_summary(body, media_type_is_html(content.content_type.as_ref()))
        })
    })
}

fn media_type_is_html(value: &str) -> bool {
    contains_ascii_case_insensitive(value, b"html")
        || value
            .as_bytes()
            .get(value.len().saturating_sub(4)..)
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case(b"+xml"))
}

fn normalize_summary(value: &str, is_html: bool) -> Option<String> {
    let rendered = if is_html || looks_like_html(value) {
        html2md::parse_html(value)
    } else {
        value.to_string()
    };
    let mut normalized = String::with_capacity(rendered.len());
    let mut previous_blank = false;
    for line in rendered.lines() {
        let line = line.trim();
        if line.is_empty() {
            if !previous_blank && !normalized.is_empty() {
                normalized.push('\n');
            }
            previous_blank = true;
        } else {
            if !normalized.is_empty() && !normalized.ends_with('\n') {
                normalized.push('\n');
            }
            normalized.push_str(line);
            previous_blank = false;
        }
    }
    let normalized = normalized.trim();
    if normalized.is_empty() {
        return None;
    }
    Some(truncate_characters(normalized, MAX_SUMMARY_CHARACTERS))
}

fn looks_like_html(value: &str) -> bool {
    [
        b"<p".as_slice(),
        b"<div",
        b"<br",
        b"<a ",
        b"<img",
        b"<ul",
        b"<ol",
        b"<li",
    ]
    .iter()
    .any(|marker| contains_ascii_case_insensitive(value, marker))
}

fn contains_ascii_case_insensitive(value: &str, needle: &[u8]) -> bool {
    value
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

fn truncate_characters(value: &str, limit: usize) -> String {
    let mut chars = value.chars();
    let mut summary = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        if limit == 0 {
            return String::new();
        }
        summary.pop();
        summary.push('…');
    }
    summary
}

fn text_content(text: &Text) -> String {
    normalize_summary(
        &text.content,
        media_type_is_html(text.content_type.as_ref()),
    )
    .unwrap_or_default()
}

fn preferred_link(links: &[feed_rs::model::Link]) -> Option<String> {
    links
        .iter()
        .filter(|link| {
            link.rel
                .as_deref()
                .is_none_or(|rel| rel.eq_ignore_ascii_case("alternate"))
        })
        .find_map(|link| safe_web_url(&link.href))
        .or_else(|| links.iter().find_map(|link| safe_web_url(&link.href)))
}

fn safe_web_url(value: &str) -> Option<String> {
    reqwest::Url::parse(value).ok().and_then(|url| {
        (matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
            .then(|| url.to_string())
    })
}

fn format_date(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::truncate_characters;

    #[test]
    fn truncation_including_ellipsis_never_exceeds_the_limit() {
        let truncated = truncate_characters(&"内".repeat(1_205), 1_200);
        assert_eq!(truncated.chars().count(), 1_200);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncate_characters("unchanged", 20), "unchanged");
        assert!(truncate_characters("value", 0).is_empty());
    }
}
