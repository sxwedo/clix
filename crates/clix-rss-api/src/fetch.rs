use anyhow::{Context, Result, bail};

use crate::{
    model::{FetchedFeed, normalize_feed},
    subscription::{Subscription, validate_feed_url},
};

const MAX_FEED_BYTES: usize = 10 * 1024 * 1024;
const MAX_CONCURRENT_FEEDS: usize = 8;

/// A subscription that could not be fetched or parsed.
#[derive(Debug)]
pub struct FetchFailure {
    pub subscription: String,
    pub error: String,
}

/// Fetch and normalize subscriptions with bounded concurrency.
pub async fn fetch_subscriptions(
    client: &reqwest::Client,
    subscriptions: Vec<Subscription>,
    limit: usize,
) -> (Vec<FetchedFeed>, Vec<FetchFailure>) {
    let mut pending = subscriptions.into_iter().enumerate();
    let mut tasks = tokio::task::JoinSet::new();
    for (index, subscription) in pending.by_ref().take(MAX_CONCURRENT_FEEDS) {
        spawn_feed_fetch(&mut tasks, client.clone(), index, subscription, limit);
    }

    let mut successes = Vec::new();
    let mut failures = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((index, _, Ok(feed))) => successes.push((index, feed)),
            Ok((_, subscription, Err(error))) => failures.push(FetchFailure {
                subscription,
                error: format!("{error:#}"),
            }),
            Err(error) => failures.push(FetchFailure {
                subscription: "<task>".to_string(),
                error: format!("feed fetch task failed: {error}"),
            }),
        }
        if let Some((index, subscription)) = pending.next() {
            spawn_feed_fetch(&mut tasks, client.clone(), index, subscription, limit);
        }
    }

    successes.sort_unstable_by_key(|(index, _)| *index);
    (
        successes.into_iter().map(|(_, feed)| feed).collect(),
        failures,
    )
}

fn spawn_feed_fetch(
    tasks: &mut tokio::task::JoinSet<(usize, String, Result<FetchedFeed>)>,
    client: reqwest::Client,
    index: usize,
    subscription: Subscription,
    limit: usize,
) {
    tasks.spawn(async move {
        let name = subscription.name.clone();
        let result = fetch_subscription(&client, &subscription, limit).await;
        (index, name, result)
    });
}

async fn fetch_subscription(
    client: &reqwest::Client,
    subscription: &Subscription,
    limit: usize,
) -> Result<FetchedFeed> {
    let url = validate_feed_url(&subscription.name, &subscription.url)?;
    let response = client
        .get(url)
        .header(
            reqwest::header::ACCEPT,
            "application/rss+xml, application/atom+xml, application/feed+json, application/json, application/xml, text/xml;q=0.9, */*;q=0.5",
        )
        .send()
        .await
        .with_context(|| format!("failed to request {}", subscription.url))?;
    let status = response.status();
    if !status.is_success() {
        bail!("source returned HTTP {status}");
    }
    let bytes = read_limited_body(response).await?;
    parse_feed(subscription, &bytes, limit)
}

async fn read_limited_body(mut response: reqwest::Response) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_FEED_BYTES as u64)
    {
        bail!("feed exceeds the {MAX_FEED_BYTES}-byte response limit");
    }

    let mut body = Vec::with_capacity(
        response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or(16 * 1024)
            .min(MAX_FEED_BYTES),
    );
    while let Some(chunk) = response
        .chunk()
        .await
        .context("failed while reading feed response")?
    {
        if body.len().saturating_add(chunk.len()) > MAX_FEED_BYTES {
            bail!("feed exceeds the {MAX_FEED_BYTES}-byte response limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Parse one RSS, Atom, or JSON Feed document into the shared model.
///
/// # Errors
///
/// Returns an error when the document is malformed or unsupported.
pub fn parse_feed(subscription: &Subscription, bytes: &[u8], limit: usize) -> Result<FetchedFeed> {
    let parser = feed_rs::parser::Builder::new()
        .sanitize_content(true)
        .build();
    let parsed = parser
        .parse(bytes)
        .with_context(|| format!("failed to parse feed document at {}", subscription.url))?;
    Ok(normalize_feed(subscription, parsed, limit))
}
