//! Persistent normalized RSS entries backed by redb.

use std::path::Path;

use anyhow::{Context, Result};
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
}

/// Counts returned by one atomic RSS database sync.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
}

/// Persistent RSS entry store.
pub struct RssDb {
    db: Database,
}

impl RssDb {
    /// Open or create an RSS database, creating its parent directory if needed.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory or database cannot be opened.
    pub fn open(path: &Path) -> Result<Self> {
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
        Ok(stats)
    }

    /// Return the number of stored RSS entries.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be read.
    pub fn len(&self) -> Result<usize> {
        let transaction = self.db.begin_read()?;
        match transaction.open_table(ENTRIES_TABLE) {
            Ok(table) => Ok(table.iter()?.count()),
            Err(redb::TableError::TableDoesNotExist(_)) => Ok(0),
            Err(error) => Err(error.into()),
        }
    }

    /// Read a stored entry by feed source URL and entry ID.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be read or the stored JSON is invalid.
    #[cfg(test)]
    pub fn get(&self, source_url: &str, entry_id: &str) -> Result<Option<StoredEntry>> {
        let transaction = self.db.begin_read()?;
        let table = match transaction.open_table(ENTRIES_TABLE) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        table
            .get(entry_key(source_url, entry_id).as_str())?
            .map(|value| {
                serde_json::from_slice(value.value()).context("invalid stored RSS entry JSON")
            })
            .transpose()
    }
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

    fn feed(source_url: &str, entry_id: &str, title: &str) -> FetchedFeed {
        FetchedFeed {
            subscription: "Example".to_string(),
            source_url: source_url.to_string(),
            title: "Example Feed".to_string(),
            feed_type: "RSS2".to_string(),
            site_url: Some("https://example.com/".to_string()),
            updated_at: Some("2026-07-31T00:00:00Z".to_string()),
            entries: vec![FetchedEntry {
                id: entry_id.to_string(),
                title: title.to_string(),
                url: Some(format!("https://example.com/{entry_id}")),
                published_at: Some("2026-07-31T00:00:00Z".to_string()),
                authors: vec!["Ada".to_string()],
                categories: vec!["Rust".to_string()],
                summary: Some("Summary".to_string()),
            }],
        }
    }

    #[test]
    fn inserts_skips_and_updates_entries_without_losing_first_seen_time() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("rss.redb");
        let db = RssDb::open(&path).expect("open db");
        let original = feed("https://example.com/feed.xml", "one", "Original");

        let first = db
            .upsert_feeds(std::slice::from_ref(&original), "2026-07-31T01:00:00Z")
            .expect("first sync");
        assert_eq!(
            first,
            SyncStats {
                inserted: 1,
                updated: 0,
                unchanged: 0
            }
        );

        let second = db
            .upsert_feeds(&[original], "2026-07-31T02:00:00Z")
            .expect("unchanged sync");
        assert_eq!(
            second,
            SyncStats {
                inserted: 0,
                updated: 0,
                unchanged: 1
            }
        );

        let changed = feed("https://example.com/feed.xml", "one", "Changed");
        let third = db
            .upsert_feeds(&[changed], "2026-07-31T03:00:00Z")
            .expect("changed sync");
        assert_eq!(
            third,
            SyncStats {
                inserted: 0,
                updated: 1,
                unchanged: 0
            }
        );
        let stored = db
            .get("https://example.com/feed.xml", "one")
            .expect("read")
            .expect("entry");
        assert_eq!(stored.entry.title, "Changed");
        assert_eq!(stored.first_seen_at, "2026-07-31T01:00:00Z");
        assert_eq!(db.len().expect("len"), 1);
    }

    #[test]
    fn source_url_is_part_of_the_identity_and_state_survives_reopen() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("nested/rss.redb");
        {
            let db = RssDb::open(&path).expect("open db");
            db.upsert_feeds(
                &[
                    feed("https://one.example/feed", "same", "One"),
                    feed("https://two.example/feed", "same", "Two"),
                ],
                "2026-07-31T01:00:00Z",
            )
            .expect("sync");
            assert_eq!(db.len().expect("len"), 2);
        }

        let reopened = RssDb::open(&path).expect("reopen db");
        assert_eq!(reopened.len().expect("len after reopen"), 2);
        assert_eq!(
            reopened
                .get("https://two.example/feed", "same")
                .expect("read")
                .expect("entry")
                .entry
                .title,
            "Two"
        );
    }
}
