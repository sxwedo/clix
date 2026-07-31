use chrono::{SecondsFormat, Utc};
use clix_rss_api::FetchedFeed;
use serde::Serialize;

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
