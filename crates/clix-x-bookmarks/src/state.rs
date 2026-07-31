//! Persistent bookmark state backed by a redb key-value store.
//!
//! Stores the set of already-seen tweet IDs and their cached article titles,
//! replacing the previous `<output>.state.json` sidecar. The database lives at
//! `~/.config/clix/bookmarks.redb` by default but can be redirected with
//! `--state <path>.redb`.
//!
//! A single table maps `tweet_id (&str)` → `title (&str)`. A key's presence
//! marks the tweet as seen; an empty title value means "seen with no article
//! title". This preserves the exact semantics of the former JSON state while
//! enabling incremental O(1) upserts instead of full-file rewrites.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use redb::{Database, ReadableTable, TableDefinition};

const BOOKMARKS_TABLE: TableDefinition<&str, &str> = TableDefinition::new("bookmarks");

/// Persistent bookmark dedup state.
pub struct BookmarksDb {
    db: Database,
}

/// One consistent read of every persisted bookmark and cached article title.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BookmarkSnapshot {
    pub known_ids: HashSet<String>,
    pub article_titles: BTreeMap<String, String>,
}

impl BookmarkSnapshot {
    pub fn is_empty(&self) -> bool {
        self.known_ids.is_empty()
    }
}

impl BookmarksDb {
    /// Open or create the database at `path`, ensuring the parent directory exists.
    ///
    /// # Errors
    ///
    /// Returns an error when the directory cannot be created or the database
    /// cannot be opened (for example, it is already locked by another process).
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let db = Database::create(path)
            .with_context(|| format!("failed to open bookmark state {}", path.display()))?;
        Ok(Self { db })
    }

    /// Load dedup IDs and cached titles in one read transaction and table scan.
    ///
    /// # Errors
    ///
    /// Returns an error when the database cannot be read.
    pub(crate) fn snapshot(&self) -> Result<BookmarkSnapshot> {
        let txn = self.db.begin_read()?;
        let table = match txn.open_table(BOOKMARKS_TABLE) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => {
                return Ok(BookmarkSnapshot::default());
            }
            Err(error) => return Err(error.into()),
        };
        let mut snapshot = BookmarkSnapshot::default();
        for entry in table.iter()? {
            let (key, value) = entry?;
            let id = key.value().to_string();
            let title = value.value();
            if !title.is_empty() {
                snapshot
                    .article_titles
                    .insert(id.clone(), title.to_string());
            }
            snapshot.known_ids.insert(id);
        }
        Ok(snapshot)
    }

    /// Upsert a batch of `(tweet id, article title)` pairs in one transaction.
    ///
    /// Existing keys are updated; absent keys are inserted. An empty title marks
    /// the tweet as seen without caching a title.
    ///
    /// # Errors
    ///
    /// Returns an error when the write transaction cannot be committed.
    pub fn upsert<'a, I>(&self, entries: I) -> Result<()>
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(BOOKMARKS_TABLE)?;
            for (id, title) in entries {
                table.insert(id, title)?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Remove every tracked tweet, leaving an empty state.
    ///
    /// Used by non-incremental runs to reset before recording a fresh export.
    ///
    /// # Errors
    ///
    /// Returns an error when the table cannot be cleared.
    pub fn clear(&self) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(BOOKMARKS_TABLE)?;
            // `retain` drops entries that do not match; rejecting all empties the table.
            table.retain(|_, _| false)?;
        }
        txn.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> BookmarksDb {
        let dir = tempfile::tempdir().expect("temp dir");
        BookmarksDb::open(&dir.path().join("test.redb")).expect("open db")
    }

    #[test]
    fn empty_db_reports_no_known_ids_or_titles() {
        let db = temp_db();
        let snapshot = db.snapshot().expect("snapshot");
        assert!(snapshot.is_empty());
        assert!(snapshot.article_titles.is_empty());
    }

    #[test]
    fn upsert_then_read_round_trips_ids_and_titles() {
        let db = temp_db();
        db.upsert([("111", ""), ("222", "Title Two"), ("333", "")])
            .expect("upsert");

        let snapshot = db.snapshot().expect("snapshot");
        assert_eq!(
            snapshot.known_ids,
            HashSet::from(["111".into(), "222".into(), "333".into()])
        );

        let titles = snapshot.article_titles;
        assert_eq!(titles.len(), 1, "only non-empty titles are returned");
        assert_eq!(titles.get("222").map(String::as_str), Some("Title Two"));
    }

    #[test]
    fn upsert_updates_an_existing_title() {
        let db = temp_db();
        db.upsert([("1", "old")]).expect("upsert old");
        db.upsert([("1", "new")]).expect("upsert new");

        let titles = db.snapshot().expect("snapshot").article_titles;
        assert_eq!(titles.get("1").map(String::as_str), Some("new"));
    }

    #[test]
    fn clear_removes_everything() {
        let db = temp_db();
        db.upsert([("1", "a"), ("2", "b")]).expect("upsert");
        assert_eq!(db.snapshot().expect("snapshot").known_ids.len(), 2);

        db.clear().expect("clear");
        assert!(db.snapshot().expect("snapshot after clear").is_empty());
    }
}
