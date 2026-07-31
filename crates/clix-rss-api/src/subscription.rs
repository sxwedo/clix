use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use clix_core::settings::{RssFeedSettings, RssSettings};

/// One validated RSS subscription selected from the clix configuration.
#[derive(Debug, Clone)]
pub struct Subscription {
    pub name: String,
    pub url: String,
}

/// Select enabled subscriptions, optionally filtering by configured names.
///
/// # Errors
///
/// Returns an error for absent, invalid, duplicated, disabled, or unknown subscriptions.
pub fn select_subscriptions(
    settings: &RssSettings,
    requested: &[String],
) -> Result<Vec<Subscription>> {
    let enabled = settings
        .feeds
        .iter()
        .filter(|feed| feed.enabled)
        .collect::<Vec<_>>();
    if enabled.is_empty() {
        bail!(
            "no enabled RSS subscriptions found in {}. Add one or more [[rss.feeds]] blocks",
            clix_core::settings::config_path().display()
        );
    }

    validate_subscriptions(&enabled)?;
    if requested.is_empty() {
        return Ok(enabled.into_iter().map(normalize_subscription).collect());
    }

    let requested = requested
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    if requested.is_empty() {
        bail!("--feed requires a non-blank subscription name");
    }

    let missing = requested
        .iter()
        .filter(|name| {
            !enabled
                .iter()
                .any(|feed| feed.name.trim().eq_ignore_ascii_case(name))
        })
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let available = enabled
            .iter()
            .map(|feed| feed.name.trim())
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "unknown or disabled RSS subscription(s): {}. Enabled subscriptions: {available}",
            missing.join(", ")
        );
    }

    Ok(enabled
        .into_iter()
        .filter(|feed| {
            requested
                .iter()
                .any(|name| feed.name.trim().eq_ignore_ascii_case(name))
        })
        .map(normalize_subscription)
        .collect())
}

fn validate_subscriptions(feeds: &[&RssFeedSettings]) -> Result<()> {
    let mut names = HashSet::new();
    for feed in feeds {
        let name = feed.name.trim();
        if name.is_empty() {
            bail!("RSS subscription names cannot be blank");
        }
        if !names.insert(name.to_lowercase()) {
            bail!("duplicate enabled RSS subscription name: {name}");
        }
        validate_feed_url(name, feed.url.trim())?;
    }
    Ok(())
}

pub fn validate_feed_url(name: &str, value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value)
        .with_context(|| format!("RSS subscription {name} has an invalid URL: {value}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("RSS subscription {name} must use an HTTP(S) URL: {value}");
    }
    Ok(url)
}

fn normalize_subscription(feed: &RssFeedSettings) -> Subscription {
    Subscription {
        name: feed.name.trim().to_string(),
        url: feed.url.trim().to_string(),
    }
}
