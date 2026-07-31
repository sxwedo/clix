//! Shared persistent RSS entry store backed by redb.

use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clix_rss_api::{FetchedEntry, FetchedFeed};
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

const ENTRIES_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("rss_entries_v1");

/// One durable normalized RSS entry and its feed context.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct StoredEntry {
    pub subscription: String,
    pub source_url: String,
    pub feed_title: String,
    pub feed_type: String,
    pub site_url: Option<String>,
    pub feed_updated_at: Option<String>,
    pub entry: FetchedEntry,
    pub first_seen_at: String,
}

impl StoredEntry {
    fn from_feed(feed: &FetchedFeed, entry: &FetchedEntry, first_seen_at: &str) -> Self {
        Self {
            subscription: feed.subscription.clone(),
            source_url: feed.source_url.clone(),
            feed_title: feed.title.clone(),
            feed_type: feed.feed_type.clone(),
            site_url: feed.site_url.clone(),
            feed_updated_at: feed.updated_at.clone(),
            entry: entry.clone(),
            first_seen_at: first_seen_at.to_string(),
        }
    }

    /// Timestamp used for archive ordering and `since` filtering.
    #[must_use]
    pub fn archive_timestamp(&self) -> &str {
        self.entry
            .published_at
            .as_deref()
            .unwrap_or(&self.first_seen_at)
    }
}

/// Counts returned by one atomic RSS database sync.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub total: usize,
}

/// Filters applied while reading the RSS archive.
#[derive(Debug, Clone, Default)]
pub struct EntryQuery {
    /// Case-insensitive subscription names. Empty selects every feed.
    pub feeds: Vec<String>,
    /// Include entries published or first observed at or after this time.
    pub since: Option<DateTime<Utc>>,
    /// Maximum entries returned after sorting, across all selected feeds.
    pub limit: Option<usize>,
}

/// One ordered RSS archive query and its pre-limit counts.
#[derive(Debug)]
pub struct QueryResult {
    pub database_entries: usize,
    pub matched_entries: usize,
    pub entries: Vec<StoredEntry>,
}

/// Persistent RSS entry store.
pub struct RssStore {
    db: Database,
}

impl RssStore {
    /// Open or create an RSS database, creating its parent directory if needed.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory or database cannot be opened.
    pub fn open_or_create(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let db = Database::create(path)
            .with_context(|| format!("failed to open RSS state {}", path.display()))?;
        Ok(Self { db })
    }

    /// Open an existing RSS database without silently creating an empty store.
    ///
    /// # Errors
    ///
    /// Returns an actionable error when the database is absent or cannot be opened.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.is_file() {
            bail!(
                "RSS state database does not exist: {}. Run `clix rss sync` first",
                path.display()
            );
        }
        let db = Database::open(path)
            .with_context(|| format!("failed to open RSS state {}", path.display()))?;
        Ok(Self { db })
    }

    /// Insert new entries and update changed entries in one transaction.
    ///
    /// Entries are keyed by the exact feed source URL and feed entry ID.
    /// Existing `first_seen_at` timestamps are retained. Unchanged records are
    /// deliberately not rewritten.
    ///
    /// # Errors
    ///
    /// Returns an error when stored JSON is invalid or the transaction fails.
    pub fn upsert_feeds(&self, feeds: &[FetchedFeed], synced_at: &str) -> Result<SyncStats> {
        let transaction = self.db.begin_write()?;
        let mut stats = SyncStats::default();
        {
            let mut table = transaction.open_table(ENTRIES_TABLE)?;
            for feed in feeds {
                for entry in &feed.entries {
                    let key = entry_key(&feed.source_url, &entry.id);
                    let existing = table.get(key.as_str())?.map(|value| value.value().to_vec());
                    let mut candidate = StoredEntry::from_feed(feed, entry, synced_at);

                    if let Some(bytes) = existing {
                        let current: StoredEntry = serde_json::from_slice(&bytes)
                            .with_context(|| format!("invalid RSS state record for key {key}"))?;
                        candidate.first_seen_at.clone_from(&current.first_seen_at);
                        if current == candidate {
                            stats.unchanged += 1;
                            continue;
                        }
                        stats.updated += 1;
                    } else {
                        stats.inserted += 1;
                    }

                    let serialized = serde_json::to_vec(&candidate)
                        .with_context(|| format!("failed to encode RSS state record {key}"))?;
                    table.insert(key.as_str(), serialized.as_slice())?;
                }
            }
        }
        transaction.commit()?;
        stats.total = self.entry_count()?;
        Ok(stats)
    }

    /// Read, validate, filter, and newest-first sort the RSS archive.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero limit, unknown feed, invalid stored timestamp,
    /// corrupt stored JSON, or failed read transaction.
    pub fn query(&self, query: &EntryQuery) -> Result<QueryResult> {
        if query.limit == Some(0) {
            bail!("RSS query limit must be greater than zero");
        }

        let transaction = self.db.begin_read()?;
        let table = match transaction.open_table(ENTRIES_TABLE) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => {
                validate_requested_feeds(&query.feeds, &[])?;
                return Ok(QueryResult {
                    database_entries: 0,
                    matched_entries: 0,
                    entries: Vec::new(),
                });
            }
            Err(error) => return Err(error.into()),
        };

        let mut entries = Vec::new();
        for record in table.iter()? {
            let (key, value) = record?;
            let entry = serde_json::from_slice(value.value())
                .with_context(|| format!("invalid RSS state record for key {}", key.value()))?;
            entries.push(entry);
        }
        let database_entries = entries.len();

        let requested = validate_requested_feeds(&query.feeds, &entries)?;
        if !requested.is_empty() {
            entries.retain(|entry| requested.contains(&entry.subscription.to_lowercase()));
        }
        if let Some(since) = query.since.as_ref() {
            let mut recent = Vec::with_capacity(entries.len());
            for entry in entries {
                if parse_archive_timestamp(&entry)? >= *since {
                    recent.push(entry);
                }
            }
            entries = recent;
        }

        entries.sort_by(|left, right| {
            right
                .archive_timestamp()
                .cmp(left.archive_timestamp())
                .then_with(|| left.subscription.cmp(&right.subscription))
                .then_with(|| left.entry.id.cmp(&right.entry.id))
        });
        let matched_entries = entries.len();
        if let Some(limit) = query.limit {
            entries.truncate(limit);
        }

        Ok(QueryResult {
            database_entries,
            matched_entries,
            entries,
        })
    }

    fn entry_count(&self) -> Result<usize> {
        let transaction = self.db.begin_read()?;
        match transaction.open_table(ENTRIES_TABLE) {
            Ok(table) => Ok(table.iter()?.count()),
            Err(redb::TableError::TableDoesNotExist(_)) => Ok(0),
            Err(error) => Err(error.into()),
        }
    }
}

/// Default RSS database path: `~/.config/clix/rss.redb`.
#[must_use]
pub fn default_state_path() -> PathBuf {
    clix_core::settings::config_dir().join("rss.redb")
}

fn validate_requested_feeds(
    requested: &[String],
    entries: &[StoredEntry],
) -> Result<HashSet<String>> {
    let mut normalized = HashSet::new();
    let mut original_names = BTreeMap::new();
    for entry in entries {
        original_names
            .entry(entry.subscription.to_lowercase())
            .or_insert_with(|| entry.subscription.clone());
    }

    let mut missing = Vec::new();
    for name in requested {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            bail!("--feed requires a non-blank subscription name");
        }
        let key = trimmed.to_lowercase();
        if !original_names.contains_key(&key) {
            missing.push(trimmed);
        }
        normalized.insert(key);
    }
    if !missing.is_empty() {
        let available = original_names.into_values().collect::<Vec<_>>().join(", ");
        bail!(
            "unknown RSS subscription(s) in state: {}. Stored subscriptions: {}",
            missing.join(", "),
            if available.is_empty() {
                "<none>"
            } else {
                &available
            }
        );
    }
    Ok(normalized)
}

fn parse_archive_timestamp(entry: &StoredEntry) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(entry.archive_timestamp())
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .with_context(|| format!("invalid archive timestamp for RSS entry {}", entry.entry.id))
}

fn entry_key(source_url: &str, entry_id: &str) -> String {
    format!(
        "v1:{}:{source_url}{}:{entry_id}",
        source_url.len(),
        entry_id.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(subscription: &str, source_url: &str, entries: &[(&str, &str, &str)]) -> FetchedFeed {
        FetchedFeed {
            subscription: subscription.to_string(),
            source_url: source_url.to_string(),
            title: format!("{subscription} Feed"),
            feed_type: "RSS2".to_string(),
            site_url: Some("https://example.com/".to_string()),
            updated_at: Some("2026-07-31T00:00:00Z".to_string()),
            entries: entries
                .iter()
                .map(|(id, title, published_at)| FetchedEntry {
                    id: (*id).to_string(),
                    title: (*title).to_string(),
                    url: Some(format!("https://example.com/{id}")),
                    published_at: Some((*published_at).to_string()),
                    authors: vec!["Ada".to_string()],
                    categories: vec!["Rust".to_string()],
                    summary: Some("Summary".to_string()),
                })
                .collect(),
        }
    }

    #[test]
    fn inserts_skips_and_updates_without_losing_first_seen_time() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("rss.redb");
        let store = RssStore::open_or_create(&path).expect("open store");
        let original = feed(
            "Example",
            "https://example.com/feed.xml",
            &[("one", "Original", "2026-07-31T00:00:00Z")],
        );

        let first = store
            .upsert_feeds(std::slice::from_ref(&original), "2026-07-31T01:00:00Z")
            .expect("first sync");
        assert_eq!(
            first,
            SyncStats {
                inserted: 1,
                updated: 0,
                unchanged: 0,
                total: 1
            }
        );

        let second = store
            .upsert_feeds(&[original], "2026-07-31T02:00:00Z")
            .expect("unchanged sync");
        assert_eq!(
            second,
            SyncStats {
                inserted: 0,
                updated: 0,
                unchanged: 1,
                total: 1
            }
        );

        let changed = feed(
            "Example",
            "https://example.com/feed.xml",
            &[("one", "Changed", "2026-07-31T00:00:00Z")],
        );
        let third = store
            .upsert_feeds(&[changed], "2026-07-31T03:00:00Z")
            .expect("changed sync");
        assert_eq!(
            third,
            SyncStats {
                inserted: 0,
                updated: 1,
                unchanged: 0,
                total: 1
            }
        );
        let result = store.query(&EntryQuery::default()).expect("query");
        assert_eq!(result.entries[0].entry.title, "Changed");
        assert_eq!(result.entries[0].first_seen_at, "2026-07-31T01:00:00Z");
    }

    #[test]
    fn query_filters_orders_limits_and_survives_reopen() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("nested/rss.redb");
        {
            let store = RssStore::open_or_create(&path).expect("open store");
            store
                .upsert_feeds(
                    &[
                        feed(
                            "One",
                            "https://one.example/feed",
                            &[
                                ("older", "Older", "2026-07-29T00:00:00Z"),
                                ("newer", "Newer", "2026-07-31T00:00:00Z"),
                            ],
                        ),
                        feed(
                            "Two",
                            "https://two.example/feed",
                            &[("middle", "Middle", "2026-07-30T00:00:00Z")],
                        ),
                    ],
                    "2026-07-31T01:00:00Z",
                )
                .expect("sync");
        }

        let reopened = RssStore::open(&path).expect("reopen store");
        let result = reopened
            .query(&EntryQuery {
                feeds: vec!["one".to_string()],
                since: Some(
                    DateTime::parse_from_rfc3339("2026-07-30T00:00:00Z")
                        .expect("timestamp")
                        .with_timezone(&Utc),
                ),
                limit: Some(1),
            })
            .expect("filtered query");
        assert_eq!(result.database_entries, 3);
        assert_eq!(result.matched_entries, 1);
        assert_eq!(result.entries[0].entry.title, "Newer");

        let all = reopened.query(&EntryQuery::default()).expect("all entries");
        assert_eq!(
            all.entries
                .iter()
                .map(|entry| entry.entry.title.as_str())
                .collect::<Vec<_>>(),
            ["Newer", "Middle", "Older"]
        );
        assert!(
            reopened
                .query(&EntryQuery {
                    feeds: vec!["Missing".to_string()],
                    ..EntryQuery::default()
                })
                .is_err()
        );
    }

    #[test]
    fn opening_an_absent_read_store_does_not_create_it() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("missing.redb");
        assert!(RssStore::open(&path).is_err());
        assert!(!path.exists());
    }
}
