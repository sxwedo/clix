//! Reliable delivery of stored RSS entries to configured destinations.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{File, OpenOptions},
    future::Future,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use clap::Args;
use clix_core::{settings::RssDestinationSettings, ui};
use clix_lark_base::{
    BaseFieldType, BaseTarget, BaseValue, LarkBaseClient, LarkCredentials, UpsertMode,
    UpsertRecord, UpsertReport, UpsertRequest,
};
use clix_rss_store::{
    DeliveryCheckpoint, DeliveryOutcome, EntryQuery, RssStore, StoredEntry, default_state_path,
};
use fs2::FileExt as _;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Arguments accepted by `clix rss push` and `clix-rss-push`.
#[derive(Debug, Clone, Args)]
pub struct PushArgs {
    /// Named entry in `[rss.destinations]`
    pub destination: String,

    /// redb database path (default: configured `[rss].state` or `~/.config/clix/rss.redb`)
    #[arg(long, value_name = "PATH")]
    pub state: Option<PathBuf>,

    /// Push only named stored subscriptions; repeat or use comma-separated names
    #[arg(long = "feed", value_name = "NAME", value_delimiter = ',')]
    pub feeds: Vec<String>,

    /// Maximum newest matching entries considered
    #[arg(short = 'n', long, value_name = "COUNT")]
    pub limit: Option<usize>,

    /// Validate and show planned changes without writing or checkpointing
    #[arg(long)]
    pub dry_run: bool,

    /// Reconcile every matching entry even when its local checkpoint is current
    #[arg(long)]
    pub force: bool,
}

trait BaseUpserter: Sync {
    fn upsert(&self, request: UpsertRequest) -> impl Future<Output = Result<UpsertReport>> + Send;
}

impl BaseUpserter for LarkBaseClient {
    async fn upsert(&self, request: UpsertRequest) -> Result<UpsertReport> {
        self.upsert_records(request).await
    }
}

/// Push stored RSS entries to one configured destination.
///
/// # Errors
///
/// Returns an error for invalid configuration, an absent state database,
/// invalid stored entries, a concurrent push, remote failures, or checkpoint
/// failures.
pub async fn run(args: PushArgs, settings: &clix_core::settings::Settings) -> Result<()> {
    let destination = ResolvedDestination::resolve(settings, &args.destination)?;
    let client = LarkBaseClient::new(destination.credentials.clone())?;
    execute(args, settings, &destination, &client).await
}

#[cfg(test)]
async fn run_with<U: BaseUpserter>(
    args: PushArgs,
    settings: &clix_core::settings::Settings,
    upserter: &U,
) -> Result<()> {
    let destination = ResolvedDestination::resolve(settings, &args.destination)?;
    execute(args, settings, &destination, upserter).await
}

struct ResolvedDestination {
    name: String,
    credentials: LarkCredentials,
    target: BaseTarget,
    key_field: String,
    hash_field: String,
    fields: BTreeMap<String, String>,
    target_fingerprint: String,
}

impl ResolvedDestination {
    fn resolve(settings: &clix_core::settings::Settings, name: &str) -> Result<Self> {
        let name = name.trim();
        if name.is_empty() {
            bail!("RSS destination name must not be blank");
        }
        let configured = settings
            .rss
            .destinations
            .get(name)
            .with_context(|| unknown_destination_message(settings, name))?;
        let RssDestinationSettings::LarkBase {
            base,
            key_field,
            hash_field,
            fields,
        } = configured;
        let base_settings = settings.lark.bases.get(base).with_context(|| {
            format!("RSS destination `{name}` references unknown Lark Base `{base}`")
        })?;
        let account = settings
            .lark
            .accounts
            .get(&base_settings.account)
            .with_context(|| {
                format!(
                    "Lark Base `{base}` references unknown account `{}`",
                    base_settings.account
                )
            })?;
        validate_destination_fields(name, key_field, hash_field, fields)?;

        let target_fingerprint = hash_value(&serde_json::json!({
            "kind": "lark_base",
            "app_token": base_settings.app_token,
            "table_id": base_settings.table_id,
            "key_field": key_field,
            "hash_field": hash_field,
            "fields": fields,
        }))?;
        Ok(Self {
            name: name.to_string(),
            credentials: LarkCredentials {
                app_id: account.app_id.clone(),
                app_secret: account.app_secret.clone(),
            },
            target: BaseTarget {
                app_token: base_settings.app_token.clone(),
                table_id: base_settings.table_id.clone(),
            },
            key_field: key_field.clone(),
            hash_field: hash_field.clone(),
            fields: fields.clone(),
            target_fingerprint,
        })
    }
}

async fn execute<U: BaseUpserter>(
    args: PushArgs,
    settings: &clix_core::settings::Settings,
    destination: &ResolvedDestination,
    upserter: &U,
) -> Result<()> {
    let state_path = args
        .state
        .or_else(|| settings.rss.state.clone())
        .unwrap_or_else(default_state_path);
    let _lock = (!args.dry_run)
        .then(|| acquire_delivery_lock(&state_path))
        .transpose()?;
    let store = RssStore::open(&state_path)?;
    let result = store.query(&EntryQuery {
        feeds: args.feeds,
        since: None,
        limit: args.limit,
    })?;
    let prepared = result
        .entries
        .iter()
        .map(|entry| prepare_entry(entry, destination))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|prepared| {
            args.force
                || prepared
                    .stored
                    .delivery_state(&destination.name)
                    .is_none_or(|state| {
                        !state.confirms(
                            &destination.target_fingerprint,
                            &prepared.record.payload_hash,
                        )
                    })
        })
        .collect::<Vec<_>>();

    if prepared.is_empty() {
        ui::success(format!(
            "RSS destination {} is already current (0 entries pending)",
            ui::style_bold(&destination.name)
        ));
        return Ok(());
    }

    let attempted_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let request = UpsertRequest {
        target: destination.target.clone(),
        key_field: destination.key_field.clone(),
        hash_field: destination.hash_field.clone(),
        mode: if args.dry_run {
            UpsertMode::DryRun
        } else {
            UpsertMode::Apply
        },
        records: prepared
            .iter()
            .map(|prepared| prepared.record.clone())
            .collect(),
    };
    let report = match upserter.upsert(request).await {
        Ok(report) => report,
        Err(error) => {
            if !args.dry_run {
                let summary = bounded_error(&error.to_string());
                let checkpoints = prepared
                    .iter()
                    .map(|prepared| {
                        checkpoint(
                            prepared,
                            destination,
                            &attempted_at,
                            DeliveryOutcome::Failed {
                                error: summary.clone(),
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                store
                    .record_delivery_outcomes(&checkpoints)
                    .context("remote push failed and its failure checkpoint could not be saved")?;
            }
            return Err(error)
                .with_context(|| format!("failed to push RSS destination {}", destination.name));
        }
    };

    if !args.dry_run {
        persist_successes(&store, &prepared, destination, &attempted_at, &report)?;
    }
    print_report(destination, &state_path, &report, args.dry_run);
    Ok(())
}

struct PreparedEntry<'a> {
    stored: &'a StoredEntry,
    record: UpsertRecord,
}

fn prepare_entry<'a>(
    entry: &'a StoredEntry,
    destination: &ResolvedDestination,
) -> Result<PreparedEntry<'a>> {
    let mut fields = BTreeMap::new();
    for (source, target) in &destination.fields {
        if let Some(value) = map_field(entry, source)? {
            fields.insert(target.clone(), value);
        }
    }
    let payload_hash = hash_value(&fields)?;
    Ok(PreparedEntry {
        stored: entry,
        record: UpsertRecord {
            key: entry.storage_key(),
            payload_hash,
            fields,
        },
    })
}

fn map_field(entry: &StoredEntry, source: &str) -> Result<Option<BaseValue>> {
    let value = match source {
        "title" => Some(BaseValue::Text(entry.entry.title.clone())),
        "url" => Some(entry.entry.url.as_ref().map_or(
            BaseValue::Empty(BaseFieldType::Url),
            |url| BaseValue::Url {
                text: entry.entry.title.clone(),
                link: url.clone(),
            },
        )),
        "subscription" => Some(BaseValue::Text(entry.subscription.clone())),
        "source_url" => Some(BaseValue::Url {
            text: entry.source_url.clone(),
            link: entry.source_url.clone(),
        }),
        "entry_id" => Some(BaseValue::Text(entry.entry.id.clone())),
        "published_at" => Some(
            entry
                .entry
                .published_at
                .as_deref()
                .map(date_value)
                .transpose()?
                .unwrap_or(BaseValue::Empty(BaseFieldType::DateTime)),
        ),
        "authors" => Some(BaseValue::MultiSelect(entry.entry.authors.clone())),
        "categories" => Some(BaseValue::MultiSelect(entry.entry.categories.clone())),
        "summary" => Some(
            entry
                .entry
                .summary
                .clone()
                .map_or(BaseValue::Empty(BaseFieldType::Text), BaseValue::Text),
        ),
        "first_seen_at" => Some(date_value(&entry.first_seen_at)?),
        "feed_title" => Some(BaseValue::Text(entry.feed_title.clone())),
        "feed_type" => Some(BaseValue::Text(entry.feed_type.clone())),
        "site_url" => Some(entry.site_url.as_ref().map_or(
            BaseValue::Empty(BaseFieldType::Url),
            |url| BaseValue::Url {
                text: url.clone(),
                link: url.clone(),
            },
        )),
        "feed_updated_at" => Some(
            entry
                .feed_updated_at
                .as_deref()
                .map(date_value)
                .transpose()?
                .unwrap_or(BaseValue::Empty(BaseFieldType::DateTime)),
        ),
        _ => bail!("unsupported RSS destination field `{source}`"),
    };
    Ok(value)
}

fn date_value(value: &str) -> Result<BaseValue> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| BaseValue::DateTime(value.timestamp_millis()))
        .with_context(|| format!("invalid RSS timestamp `{value}`"))
}

fn persist_successes(
    store: &RssStore,
    prepared: &[PreparedEntry<'_>],
    destination: &ResolvedDestination,
    attempted_at: &str,
    report: &UpsertReport,
) -> Result<()> {
    let by_key = prepared
        .iter()
        .map(|entry| (entry.record.key.as_str(), entry))
        .collect::<HashMap<_, _>>();
    let mut seen = HashSet::new();
    let checkpoints = report
        .receipts
        .iter()
        .map(|receipt| {
            if !seen.insert(receipt.key.as_str()) {
                bail!(
                    "Lark Base returned duplicate receipt for key {}",
                    receipt.key
                );
            }
            let prepared = by_key.get(receipt.key.as_str()).with_context(|| {
                format!("Lark Base returned unknown receipt key {}", receipt.key)
            })?;
            let remote_id = receipt
                .remote_id
                .clone()
                .with_context(|| format!("Lark Base omitted remote ID for key {}", receipt.key))?;
            Ok(checkpoint(
                prepared,
                destination,
                attempted_at,
                DeliveryOutcome::Succeeded { remote_id },
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    if checkpoints.len() != prepared.len() {
        bail!(
            "Lark Base returned {} receipts for {} RSS entries",
            checkpoints.len(),
            prepared.len()
        );
    }
    store.record_delivery_outcomes(&checkpoints)
}

fn checkpoint(
    prepared: &PreparedEntry<'_>,
    destination: &ResolvedDestination,
    attempted_at: &str,
    outcome: DeliveryOutcome,
) -> DeliveryCheckpoint {
    DeliveryCheckpoint {
        entry_key: prepared.record.key.clone(),
        destination: destination.name.clone(),
        kind: "lark_base".to_string(),
        target_fingerprint: destination.target_fingerprint.clone(),
        payload_hash: prepared.record.payload_hash.clone(),
        attempted_at: attempted_at.to_string(),
        outcome,
    }
}

fn validate_destination_fields(
    destination: &str,
    key_field: &str,
    hash_field: &str,
    fields: &BTreeMap<String, String>,
) -> Result<()> {
    if key_field.trim().is_empty() || hash_field.trim().is_empty() {
        bail!("RSS destination `{destination}` key_field and hash_field must not be blank");
    }
    if key_field == hash_field {
        bail!("RSS destination `{destination}` key_field and hash_field must differ");
    }
    if fields.is_empty() {
        bail!("RSS destination `{destination}` must map at least one RSS field");
    }
    let mut targets = HashSet::new();
    for (source, target) in fields {
        if source.trim().is_empty() || target.trim().is_empty() {
            bail!("RSS destination `{destination}` field mappings must not be blank");
        }
        if target == key_field || target == hash_field {
            bail!(
                "RSS destination `{destination}` mapped field `{target}` conflicts with its key or hash field"
            );
        }
        if !targets.insert(target) {
            bail!("RSS destination `{destination}` maps multiple values to field `{target}`");
        }
    }
    Ok(())
}

fn unknown_destination_message(settings: &clix_core::settings::Settings, name: &str) -> String {
    let available = settings
        .rss
        .destinations
        .keys()
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "unknown RSS destination `{name}`. Configured destinations: {}",
        if available.is_empty() {
            "<none>"
        } else {
            &available
        }
    )
}

fn hash_value(value: &impl Serialize) -> Result<String> {
    let encoded = serde_json::to_vec(value).context("failed to encode RSS delivery hash input")?;
    Ok(format!("sha256:{:x}", Sha256::digest(encoded)))
}

fn acquire_delivery_lock(state_path: &Path) -> Result<File> {
    let mut lock_name = state_path.as_os_str().to_os_string();
    lock_name.push(".push.lock");
    let lock_path = PathBuf::from(lock_name);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed to open RSS push lock {}", lock_path.display()))?;
    file.try_lock_exclusive().with_context(|| {
        format!(
            "another RSS push is already running for {}",
            state_path.display()
        )
    })?;
    Ok(file)
}

fn bounded_error(value: &str) -> String {
    const LIMIT: usize = 1_000;
    let mut characters = value.chars();
    let mut result = characters.by_ref().take(LIMIT).collect::<String>();
    if characters.next().is_some() {
        result.pop();
        result.push('…');
    }
    result
}

fn print_report(
    destination: &ResolvedDestination,
    state_path: &Path,
    report: &UpsertReport,
    dry_run: bool,
) {
    let prefix = if dry_run { "would push" } else { "pushed" };
    ui::success(format!(
        "{prefix} {} created, {} updated, {} unchanged RSS entries from {} to {}{}",
        ui::style_bold(&report.created.to_string()),
        ui::style_bold(&report.updated.to_string()),
        ui::style_bold(&report.unchanged.to_string()),
        ui::style_bold(&state_path.display().to_string()),
        ui::style_bold(&destination.name),
        if dry_run { " (dry run)" } else { "" }
    ));
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use clix_lark_base::{UpsertAction, UpsertReceipt};
    use clix_rss_api::{FetchedEntry, FetchedFeed};
    use clix_rss_store::{EntryQuery, RssStore};

    use super::*;

    struct RecordingUpserter {
        requests: Mutex<Vec<UpsertRequest>>,
    }

    impl BaseUpserter for RecordingUpserter {
        async fn upsert(&self, request: UpsertRequest) -> Result<UpsertReport> {
            let receipts = request
                .records
                .iter()
                .enumerate()
                .map(|(index, record)| UpsertReceipt {
                    key: record.key.clone(),
                    remote_id: Some(format!("rec-{index}")),
                    action: UpsertAction::Created,
                })
                .collect::<Vec<_>>();
            self.requests.lock().expect("requests").push(request);
            Ok(UpsertReport {
                created: receipts.len(),
                receipts,
                ..UpsertReport::default()
            })
        }
    }

    struct RejectingUpserter;

    impl BaseUpserter for RejectingUpserter {
        async fn upsert(&self, _request: UpsertRequest) -> Result<UpsertReport> {
            bail!("upserter must not be called when every checkpoint is current")
        }
    }

    struct FailingUpserter;

    impl BaseUpserter for FailingUpserter {
        async fn upsert(&self, _request: UpsertRequest) -> Result<UpsertReport> {
            bail!("temporary destination failure")
        }
    }

    #[tokio::test]
    async fn successful_push_checkpoints_the_entry_and_next_push_is_local_noop() {
        let directory = tempfile::tempdir().expect("temp dir");
        let state = directory.path().join("rss.redb");
        let store = RssStore::open_or_create(&state).expect("store");
        store
            .upsert_feeds(&[feed()], "2026-07-31T01:00:00Z")
            .expect("seed");
        drop(store);
        let settings: clix_core::settings::Settings = toml::from_str(&format!(
            r#"
[rss]
state = "{}"

[lark.accounts.default]
app_id = "cli_test"
app_secret = "secret"

[lark.bases.rss_news]
account = "default"
app_token = "app-token"
table_id = "table-id"

[rss.destinations.news]
type = "lark_base"
base = "rss_news"
key_field = "RSS Key"
hash_field = "Payload Hash"

[rss.destinations.news.fields]
title = "标题"
"#,
            state.display()
        ))
        .expect("settings");
        let args = PushArgs {
            destination: "news".to_string(),
            state: None,
            feeds: Vec::new(),
            limit: None,
            dry_run: false,
            force: false,
        };
        let upserter = RecordingUpserter {
            requests: Mutex::new(Vec::new()),
        };

        run_with(args.clone(), &settings, &upserter)
            .await
            .expect("first push");
        run_with(args, &settings, &RejectingUpserter)
            .await
            .expect("second push");

        let entry = &RssStore::open(&state)
            .expect("reopen")
            .query(&EntryQuery::default())
            .expect("query")
            .entries[0];
        let delivery = entry.delivery_state("news").expect("checkpoint");
        assert_eq!(delivery.remote_id.as_deref(), Some("rec-0"));
        assert_eq!(delivery.attempts, 1);

        let requests = upserter.requests.lock().expect("requests");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].records.len(), 1);
        assert_eq!(
            requests[0].records[0].fields["标题"],
            clix_lark_base::BaseValue::Text("Original".to_string())
        );
        drop(requests);
    }

    #[tokio::test]
    async fn failed_push_is_checkpointed_and_remains_eligible_for_retry() {
        let directory = tempfile::tempdir().expect("temp dir");
        let state = directory.path().join("rss.redb");
        let store = RssStore::open_or_create(&state).expect("store");
        store
            .upsert_feeds(&[feed()], "2026-07-31T01:00:00Z")
            .expect("seed");
        drop(store);
        let settings = settings(&state);
        let args = push_args();

        let error = run_with(args.clone(), &settings, &FailingUpserter)
            .await
            .expect_err("first push should fail");
        assert!(error.to_string().contains("failed to push RSS destination"));
        let failed = delivery(&state);
        assert_eq!(failed.status, clix_rss_store::DeliveryStatus::Failed);
        assert_eq!(failed.attempts, 1);
        assert_eq!(
            failed.last_error.as_deref(),
            Some("temporary destination failure")
        );

        let retry = RecordingUpserter {
            requests: Mutex::new(Vec::new()),
        };
        run_with(args, &settings, &retry)
            .await
            .expect("retry should succeed");
        let succeeded = delivery(&state);
        assert_eq!(succeeded.status, clix_rss_store::DeliveryStatus::Succeeded);
        assert_eq!(succeeded.attempts, 2);
    }

    #[tokio::test]
    async fn absent_optional_values_explicitly_clear_managed_remote_fields() {
        let directory = tempfile::tempdir().expect("temp dir");
        let state = directory.path().join("rss.redb");
        let mut source = feed();
        source.entries[0].url = None;
        source.entries[0].published_at = None;
        source.entries[0].summary = None;
        let store = RssStore::open_or_create(&state).expect("store");
        store
            .upsert_feeds(&[source], "2026-07-31T01:00:00Z")
            .expect("seed");
        drop(store);
        let mut settings = settings(&state);
        let RssDestinationSettings::LarkBase { fields, .. } =
            settings.rss.destinations.get_mut("news").expect("news");
        fields.insert("url".to_string(), "URL".to_string());
        fields.insert("published_at".to_string(), "Published".to_string());
        fields.insert("summary".to_string(), "Summary".to_string());
        let upserter = RecordingUpserter {
            requests: Mutex::new(Vec::new()),
        };

        run_with(push_args(), &settings, &upserter)
            .await
            .expect("push");

        let requests = upserter.requests.lock().expect("requests");
        let fields = requests[0].records[0].fields.clone();
        drop(requests);
        assert_eq!(
            fields["URL"],
            BaseValue::Empty(clix_lark_base::BaseFieldType::Url)
        );
        assert_eq!(
            fields["Published"],
            BaseValue::Empty(clix_lark_base::BaseFieldType::DateTime)
        );
        assert_eq!(
            fields["Summary"],
            BaseValue::Empty(clix_lark_base::BaseFieldType::Text)
        );
    }

    fn settings(state: &Path) -> clix_core::settings::Settings {
        toml::from_str(&format!(
            r#"
[rss]
state = "{}"

[lark.accounts.default]
app_id = "cli_test"
app_secret = "secret"

[lark.bases.rss_news]
account = "default"
app_token = "app-token"
table_id = "table-id"

[rss.destinations.news]
type = "lark_base"
base = "rss_news"
key_field = "RSS Key"
hash_field = "Payload Hash"

[rss.destinations.news.fields]
title = "标题"
"#,
            state.display()
        ))
        .expect("settings")
    }

    fn push_args() -> PushArgs {
        PushArgs {
            destination: "news".to_string(),
            state: None,
            feeds: Vec::new(),
            limit: None,
            dry_run: false,
            force: false,
        }
    }

    fn delivery(state: &Path) -> clix_rss_store::DeliveryState {
        RssStore::open(state)
            .expect("reopen")
            .query(&EntryQuery::default())
            .expect("query")
            .entries[0]
            .delivery_state("news")
            .expect("checkpoint")
            .clone()
    }

    fn feed() -> FetchedFeed {
        FetchedFeed {
            subscription: "Example".to_string(),
            source_url: "https://example.com/feed.xml".to_string(),
            title: "Example Feed".to_string(),
            feed_type: "RSS2".to_string(),
            site_url: Some("https://example.com/".to_string()),
            updated_at: Some("2026-07-31T00:00:00Z".to_string()),
            entries: vec![FetchedEntry {
                id: "one".to_string(),
                title: "Original".to_string(),
                url: Some("https://example.com/one".to_string()),
                published_at: Some("2026-07-31T00:00:00Z".to_string()),
                authors: vec!["Ada".to_string()],
                categories: vec!["Rust".to_string()],
                summary: Some("Summary".to_string()),
            }],
        }
    }
}
