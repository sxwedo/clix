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
    #[serde(default)]
    extra: EntryExtra,
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
            extra: EntryExtra::default(),
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

    /// Return the opaque key used to address this entry in the store.
    #[must_use]
    pub fn storage_key(&self) -> String {
        entry_key(&self.source_url, &self.entry.id)
    }

    /// Return the latest checkpoint for a named delivery destination.
    #[must_use]
    pub fn delivery_state(&self, destination: &str) -> Option<&DeliveryState> {
        self.extra.deliveries.get(destination)
    }
}

/// Current status of the latest delivery attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    /// The payload was confirmed at the remote destination.
    Succeeded,
    /// The latest attempt failed and should be retried.
    Failed,
}

/// Durable checkpoint for one entry at one named destination.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DeliveryState {
    pub kind: String,
    pub target_fingerprint: String,
    pub payload_hash: String,
    pub status: DeliveryStatus,
    pub remote_id: Option<String>,
    pub attempts: u32,
    pub last_attempt_at: String,
    pub delivered_at: Option<String>,
    pub last_error: Option<String>,
}

impl DeliveryState {
    /// Return whether this checkpoint confirms the exact target and payload.
    #[must_use]
    pub fn confirms(&self, target_fingerprint: &str, payload_hash: &str) -> bool {
        self.status == DeliveryStatus::Succeeded
            && self.target_fingerprint == target_fingerprint
            && self.payload_hash == payload_hash
    }
}

/// Remote outcome to persist for one delivery attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// The destination confirmed the payload and returned its remote record ID.
    Succeeded { remote_id: String },
    /// The attempt failed with an actionable bounded error summary.
    Failed { error: String },
}

/// One delivery result to merge into an existing RSS record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryCheckpoint {
    pub entry_key: String,
    pub destination: String,
    pub kind: String,
    pub target_fingerprint: String,
    pub payload_hash: String,
    pub attempted_at: String,
    pub outcome: DeliveryOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
struct EntryExtra {
    #[serde(default = "current_extra_version")]
    version: u16,
    #[serde(default)]
    deliveries: BTreeMap<String, DeliveryState>,
    #[serde(flatten)]
    extensions: BTreeMap<String, serde_json::Value>,
}

impl Default for EntryExtra {
    fn default() -> Self {
        Self {
            version: current_extra_version(),
            deliveries: BTreeMap::new(),
            extensions: BTreeMap::new(),
        }
    }
}

const fn current_extra_version() -> u16 {
    1
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
                        candidate.extra.clone_from(&current.extra);
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

    /// Atomically merge delivery outcomes into their latest stored RSS records.
    ///
    /// The method re-reads each record inside the write transaction, so a
    /// concurrent RSS refresh cannot be overwritten by a stale delivery copy.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid or duplicate checkpoints, missing entries,
    /// corrupt stored JSON, attempt counter overflow, or transaction failure.
    pub fn record_delivery_outcomes(&self, checkpoints: &[DeliveryCheckpoint]) -> Result<()> {
        validate_delivery_checkpoints(checkpoints)?;
        if checkpoints.is_empty() {
            return Ok(());
        }

        let transaction = self.db.begin_write()?;
        {
            let mut table = transaction.open_table(ENTRIES_TABLE)?;
            for checkpoint in checkpoints {
                let bytes = table
                    .get(checkpoint.entry_key.as_str())?
                    .with_context(|| {
                        format!(
                            "RSS entry no longer exists for key {}",
                            checkpoint.entry_key
                        )
                    })?
                    .value()
                    .to_vec();
                let mut entry: StoredEntry = serde_json::from_slice(&bytes).with_context(|| {
                    format!("invalid RSS state record for key {}", checkpoint.entry_key)
                })?;
                let previous = entry.extra.deliveries.get(&checkpoint.destination);
                let attempts = previous
                    .map_or(0, |state| state.attempts)
                    .checked_add(1)
                    .context("RSS delivery attempt counter overflow")?;
                let (status, remote_id, delivered_at, last_error) =
                    delivery_result_fields(previous, checkpoint);
                entry.extra.deliveries.insert(
                    checkpoint.destination.clone(),
                    DeliveryState {
                        kind: checkpoint.kind.clone(),
                        target_fingerprint: checkpoint.target_fingerprint.clone(),
                        payload_hash: checkpoint.payload_hash.clone(),
                        status,
                        remote_id,
                        attempts,
                        last_attempt_at: checkpoint.attempted_at.clone(),
                        delivered_at,
                        last_error,
                    },
                );
                let serialized = serde_json::to_vec(&entry).with_context(|| {
                    format!("failed to encode RSS state record {}", checkpoint.entry_key)
                })?;
                table.insert(checkpoint.entry_key.as_str(), serialized.as_slice())?;
            }
        }
        transaction.commit()?;
        Ok(())
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

fn validate_delivery_checkpoints(checkpoints: &[DeliveryCheckpoint]) -> Result<()> {
    let mut unique = HashSet::new();
    for checkpoint in checkpoints {
        if checkpoint.entry_key.trim().is_empty()
            || checkpoint.destination.trim().is_empty()
            || checkpoint.kind.trim().is_empty()
            || checkpoint.target_fingerprint.trim().is_empty()
            || checkpoint.payload_hash.trim().is_empty()
        {
            bail!("RSS delivery checkpoint fields must not be blank");
        }
        DateTime::parse_from_rfc3339(&checkpoint.attempted_at)
            .with_context(|| "RSS delivery attempted_at must be RFC 3339")?;
        match &checkpoint.outcome {
            DeliveryOutcome::Succeeded { remote_id } if remote_id.trim().is_empty() => {
                bail!("RSS delivery remote ID must not be blank");
            }
            DeliveryOutcome::Failed { error } if error.trim().is_empty() => {
                bail!("RSS delivery error must not be blank");
            }
            DeliveryOutcome::Succeeded { .. } | DeliveryOutcome::Failed { .. } => {}
        }
        if !unique.insert((&checkpoint.entry_key, &checkpoint.destination)) {
            bail!(
                "duplicate RSS delivery checkpoint for entry {} and destination {}",
                checkpoint.entry_key,
                checkpoint.destination
            );
        }
    }
    Ok(())
}

fn delivery_result_fields(
    previous: Option<&DeliveryState>,
    checkpoint: &DeliveryCheckpoint,
) -> (
    DeliveryStatus,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    match &checkpoint.outcome {
        DeliveryOutcome::Succeeded { remote_id } => (
            DeliveryStatus::Succeeded,
            Some(remote_id.clone()),
            Some(checkpoint.attempted_at.clone()),
            None,
        ),
        DeliveryOutcome::Failed { error } => (
            DeliveryStatus::Failed,
            previous.and_then(|state| state.remote_id.clone()),
            previous.and_then(|state| state.delivered_at.clone()),
            Some(error.clone()),
        ),
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
    fn delivery_checkpoint_survives_unchanged_and_changed_rss_refreshes() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("rss.redb");
        let store = RssStore::open_or_create(&path).expect("open store");
        let original = feed(
            "Example",
            "https://example.com/feed.xml",
            &[("one", "Original", "2026-07-31T00:00:00Z")],
        );
        store
            .upsert_feeds(std::slice::from_ref(&original), "2026-07-31T01:00:00Z")
            .expect("initial sync");
        let entry = store
            .query(&EntryQuery::default())
            .expect("query")
            .entries
            .pop()
            .expect("entry");

        store
            .record_delivery_outcomes(&[DeliveryCheckpoint {
                entry_key: entry.storage_key(),
                destination: "news".to_string(),
                kind: "lark_base".to_string(),
                target_fingerprint: "target-v1".to_string(),
                payload_hash: "payload-v1".to_string(),
                attempted_at: "2026-07-31T02:00:00Z".to_string(),
                outcome: DeliveryOutcome::Succeeded {
                    remote_id: "rec-one".to_string(),
                },
            }])
            .expect("record delivery");

        let unchanged = store
            .upsert_feeds(std::slice::from_ref(&original), "2026-07-31T03:00:00Z")
            .expect("unchanged sync");
        assert_eq!(unchanged.unchanged, 1);

        let changed = feed(
            "Example",
            "https://example.com/feed.xml",
            &[("one", "Changed", "2026-07-31T00:00:00Z")],
        );
        let updated = store
            .upsert_feeds(&[changed], "2026-07-31T04:00:00Z")
            .expect("changed sync");
        assert_eq!(updated.updated, 1);

        let entry = &store.query(&EntryQuery::default()).expect("query").entries[0];
        assert_eq!(entry.entry.title, "Changed");
        let delivery = entry.delivery_state("news").expect("delivery state");
        assert_eq!(delivery.status, DeliveryStatus::Succeeded);
        assert_eq!(delivery.remote_id.as_deref(), Some("rec-one"));
        assert_eq!(delivery.attempts, 1);
        assert_eq!(delivery.payload_hash, "payload-v1");
    }

    #[test]
    fn legacy_json_without_extra_defaults_to_an_empty_versioned_envelope() {
        let legacy = serde_json::json!({
            "subscription": "Example",
            "source_url": "https://example.com/feed.xml",
            "feed_title": "Example Feed",
            "feed_type": "RSS2",
            "site_url": "https://example.com/",
            "feed_updated_at": "2026-07-31T00:00:00Z",
            "entry": {
                "id": "one",
                "title": "Original",
                "url": "https://example.com/one",
                "published_at": "2026-07-31T00:00:00Z",
                "authors": ["Ada"],
                "categories": ["Rust"],
                "summary": "Summary"
            },
            "first_seen_at": "2026-07-31T01:00:00Z"
        });

        let entry: StoredEntry =
            serde_json::from_value(legacy).expect("legacy record should remain readable");
        assert!(entry.delivery_state("news").is_none());
        let migrated = serde_json::to_value(entry).expect("serialize with extra");
        assert_eq!(migrated["extra"]["version"], 1);
        assert_eq!(migrated["extra"]["deliveries"], serde_json::json!({}));
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
