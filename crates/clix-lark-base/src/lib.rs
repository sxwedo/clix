//! Reusable Lark Base schema inspection and idempotent record upserts.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

const MAX_WRITE_ATTEMPTS: u32 = 3;

/// Credentials for one Lark custom app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LarkCredentials {
    pub app_id: String,
    pub app_secret: String,
}

/// One Lark Base table address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseTarget {
    pub app_token: String,
    pub table_id: String,
}

/// One field exposed by a Lark Base table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableField {
    pub id: String,
    pub name: String,
    pub field_type: u16,
    pub ui_type: Option<String>,
    pub is_primary: bool,
}

/// The fields currently exposed by one Lark Base table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema {
    pub fields: Vec<TableField>,
}

/// One typed value accepted by the Base upsert interface.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum BaseValue {
    /// Multi-line text field.
    Text(String),
    /// Numeric field.
    Number(f64),
    /// Date field represented as Unix milliseconds.
    DateTime(i64),
    /// Checkbox field.
    Checkbox(bool),
    /// Multi-select field.
    MultiSelect(Vec<String>),
    /// Hyperlink field.
    Url { text: String, link: String },
    /// Explicitly clear a managed field while retaining its expected type.
    Empty(BaseFieldType),
}

/// Supported Lark Base field types for explicit empty values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseFieldType {
    Text,
    Number,
    DateTime,
    Checkbox,
    MultiSelect,
    Url,
}

impl BaseFieldType {
    const fn code(self) -> u16 {
        match self {
            Self::Text => 1,
            Self::Number => 2,
            Self::MultiSelect => 4,
            Self::DateTime => 5,
            Self::Checkbox => 7,
            Self::Url => 15,
        }
    }
}

impl BaseValue {
    const fn field_type(&self) -> u16 {
        match self {
            Self::Text(_) => BaseFieldType::Text.code(),
            Self::Number(_) => BaseFieldType::Number.code(),
            Self::MultiSelect(_) => BaseFieldType::MultiSelect.code(),
            Self::DateTime(_) => BaseFieldType::DateTime.code(),
            Self::Checkbox(_) => BaseFieldType::Checkbox.code(),
            Self::Url { .. } => BaseFieldType::Url.code(),
            Self::Empty(field_type) => field_type.code(),
        }
    }

    fn into_json(self) -> serde_json::Value {
        match self {
            Self::Text(value) => value.into(),
            Self::Number(value) => serde_json::json!(value),
            Self::DateTime(value) => value.into(),
            Self::Checkbox(value) => value.into(),
            Self::MultiSelect(value) => value.into(),
            Self::Url { text, link } => serde_json::json!({
                "text": text,
                "link": link,
            }),
            Self::Empty(_) => serde_json::Value::Null,
        }
    }
}

/// One logical record to create, update, or skip by key and payload hash.
#[derive(Debug, Clone, PartialEq)]
pub struct UpsertRecord {
    pub key: String,
    pub payload_hash: String,
    pub fields: BTreeMap<String, BaseValue>,
}

/// One complete idempotent Base upsert.
#[derive(Debug, Clone, PartialEq)]
pub struct UpsertRequest {
    pub target: BaseTarget,
    pub key_field: String,
    pub hash_field: String,
    pub mode: UpsertMode,
    pub records: Vec<UpsertRecord>,
}

/// Whether an upsert should write or only return the planned actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertMode {
    Apply,
    DryRun,
}

/// The observable action applied to one logical record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertAction {
    Created,
    Updated,
    Unchanged,
}

/// The remote identity and action returned for one logical record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsertReceipt {
    pub key: String,
    pub remote_id: Option<String>,
    pub action: UpsertAction,
}

/// Aggregate and per-record results from one successful upsert.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpsertReport {
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub receipts: Vec<UpsertReceipt>,
}

/// Deep module for authenticated Lark Base operations.
pub struct LarkBaseClient {
    client: reqwest::Client,
    credentials: LarkCredentials,
    api_base: String,
    token: Mutex<Option<CachedToken>>,
}

struct CachedToken {
    value: String,
    refresh_at: Instant,
}

impl LarkBaseClient {
    /// Create a client for the public Lark `OpenAPI` endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when either credential is blank.
    pub fn new(credentials: LarkCredentials) -> Result<Self> {
        Self::with_api_base(credentials, "https://open.feishu.cn")
    }

    fn with_api_base(credentials: LarkCredentials, api_base: &str) -> Result<Self> {
        if credentials.app_id.trim().is_empty() || credentials.app_secret.trim().is_empty() {
            bail!("Lark app ID and app secret must not be blank");
        }
        Ok(Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .context("failed to build Lark HTTP client")?,
            credentials,
            api_base: api_base.trim_end_matches('/').to_string(),
            token: Mutex::new(None),
        })
    }

    /// Read every field in a Base table, following Lark pagination.
    ///
    /// # Errors
    ///
    /// Returns an error when authentication or schema retrieval fails.
    pub async fn inspect_table(&self, target: &BaseTarget) -> Result<TableSchema> {
        validate_target(target)?;
        let token = self.tenant_access_token().await?;
        let url = format!(
            "{}/open-apis/bitable/v1/apps/{}/tables/{}/fields",
            self.api_base, target.app_token, target.table_id
        );
        let mut fields = Vec::new();
        let mut page_token = None;

        loop {
            let mut request = self
                .client
                .get(&url)
                .bearer_auth(&token)
                .query(&[("page_size", "100")]);
            if let Some(value) = page_token.as_deref() {
                request = request.query(&[("page_token", value)]);
            }
            let response = request
                .send()
                .await
                .context("failed to request Lark Base fields")?;
            let status = response.status();
            let body = response
                .text()
                .await
                .context("failed to read Lark Base fields response")?;
            if !status.is_success() {
                bail!("Lark Base fields request failed with HTTP {status}: {body}");
            }
            let envelope: ApiEnvelope<FieldPage> = serde_json::from_str(&body)
                .context("failed to decode Lark Base fields response")?;
            let page = envelope.into_data("list Lark Base fields")?;
            fields.extend(page.items.into_iter().map(TableField::from));
            if !page.has_more {
                break;
            }
            page_token = page.page_token;
            if page_token.is_none() {
                bail!("Lark Base fields response says more pages exist without a page token");
            }
        }

        Ok(TableSchema { fields })
    }

    /// Validate a table schema and idempotently create, update, or skip records.
    ///
    /// Existing records are matched by `key_field`. `hash_field` determines
    /// whether the managed fields need an update. Writes are sent serially in
    /// batches of at most 500 records.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid input, schema mismatches, duplicate local or
    /// remote keys, authentication failures, or failed remote reads and writes.
    pub async fn upsert_records(&self, request: UpsertRequest) -> Result<UpsertReport> {
        validate_upsert_request(&request)?;
        if request.records.is_empty() {
            return Ok(UpsertReport::default());
        }

        let schema = self.inspect_table(&request.target).await?;
        validate_upsert_schema(&schema, &request)?;
        let existing = self
            .existing_records(&request.target, &request.key_field, &request.hash_field)
            .await?;
        let mut plan = plan_upsert(&request, &existing);
        if request.mode == UpsertMode::DryRun {
            complete_dry_run(&mut plan);
        } else {
            self.apply_plan(&request.target, &mut plan).await?;
        }
        finish_report(&request.records, plan.completed)
    }

    async fn apply_plan(&self, target: &BaseTarget, plan: &mut UpsertPlan) -> Result<()> {
        for batch in plan.creates.chunks(500) {
            let receipts = self
                .write_batch(target, "batch_create", UpsertAction::Created, batch)
                .await?;
            extend_completed(&mut plan.completed, receipts);
        }
        for batch in plan.updates.chunks(500) {
            let receipts = self
                .write_batch(target, "batch_update", UpsertAction::Updated, batch)
                .await?;
            extend_completed(&mut plan.completed, receipts);
        }
        Ok(())
    }

    async fn existing_records(
        &self,
        target: &BaseTarget,
        key_field: &str,
        hash_field: &str,
    ) -> Result<HashMap<String, ExistingRecord>> {
        let token = self.tenant_access_token().await?;
        let url = format!(
            "{}/open-apis/bitable/v1/apps/{}/tables/{}/records/search",
            self.api_base, target.app_token, target.table_id
        );
        let mut page_token = None;
        let mut records = HashMap::new();
        loop {
            let mut request = self
                .client
                .post(&url)
                .bearer_auth(&token)
                .query(&[("page_size", "500")])
                .json(&serde_json::json!({
                    "field_names": [key_field, hash_field],
                }));
            if let Some(value) = page_token.as_deref() {
                request = request.query(&[("page_token", value)]);
            }
            let response = request
                .send()
                .await
                .context("failed to search Lark Base records")?;
            let status = response.status();
            let body = response
                .text()
                .await
                .context("failed to read Lark Base record search response")?;
            if !status.is_success() {
                bail!("Lark Base record search failed with HTTP {status}: {body}");
            }
            let envelope: ApiEnvelope<RecordPage> = serde_json::from_str(&body)
                .context("failed to decode Lark Base record search response")?;
            let page = envelope.into_data("search Lark Base records")?;
            for item in page.items {
                let Some(key) = item.fields.get(key_field).and_then(text_value) else {
                    continue;
                };
                let existing = ExistingRecord {
                    record_id: item.record_id,
                    payload_hash: item.fields.get(hash_field).and_then(text_value),
                };
                if records.insert(key.clone(), existing).is_some() {
                    bail!("Lark Base contains duplicate value `{key}` in key field `{key_field}`");
                }
            }
            if !page.has_more {
                break;
            }
            page_token = page.page_token;
            if page_token.is_none() {
                bail!("Lark Base record search says more pages exist without a page token");
            }
        }
        Ok(records)
    }

    async fn write_batch(
        &self,
        target: &BaseTarget,
        operation: &str,
        action: UpsertAction,
        batch: &[PendingWrite],
    ) -> Result<Vec<UpsertReceipt>> {
        let token = self.tenant_access_token().await?;
        let url = format!(
            "{}/open-apis/bitable/v1/apps/{}/tables/{}/records/{operation}",
            self.api_base, target.app_token, target.table_id
        );
        let records = batch
            .iter()
            .map(|record| {
                let mut value = serde_json::Map::new();
                if let Some(remote_id) = record.remote_id.as_ref() {
                    value.insert("record_id".to_string(), remote_id.clone().into());
                }
                value.insert(
                    "fields".to_string(),
                    serde_json::Value::Object(record.fields.clone()),
                );
                serde_json::Value::Object(value)
            })
            .collect::<Vec<_>>();
        let payload = serde_json::json!({ "records": records });
        let (status, body) = self
            .send_write_with_retry(&url, &token, &payload, operation)
            .await?;
        if !status.is_success() {
            bail!("Lark Base {operation} failed with HTTP {status}: {body}");
        }
        let envelope: ApiEnvelope<WriteData> = serde_json::from_str(&body)
            .with_context(|| format!("failed to decode Lark Base {operation} response"))?;
        let written = envelope.into_data(&format!("Lark Base {operation}"))?;
        if written.records.len() != batch.len() {
            bail!(
                "Lark Base {operation} returned {} records for a {} record batch",
                written.records.len(),
                batch.len()
            );
        }
        batch
            .iter()
            .zip(written.records)
            .map(|(pending, remote)| {
                if remote.record_id.trim().is_empty() {
                    bail!("Lark Base {operation} returned a blank record ID");
                }
                Ok(UpsertReceipt {
                    key: pending.key.clone(),
                    remote_id: Some(remote.record_id),
                    action,
                })
            })
            .collect()
    }

    async fn send_write_with_retry(
        &self,
        url: &str,
        token: &str,
        payload: &serde_json::Value,
        operation: &str,
    ) -> Result<(reqwest::StatusCode, String)> {
        for attempt in 0..MAX_WRITE_ATTEMPTS {
            let response = self
                .client
                .post(url)
                .bearer_auth(token)
                .json(payload)
                .send()
                .await;
            match response {
                Ok(response) => {
                    let status = response.status();
                    let body = response.text().await.with_context(|| {
                        format!("failed to read Lark Base {operation} response")
                    })?;
                    let code = serde_json::from_str::<ApiStatus>(&body)
                        .ok()
                        .map(|value| value.code);
                    if attempt + 1 < MAX_WRITE_ATTEMPTS && is_transient(status, code) {
                        tokio::time::sleep(retry_delay(attempt)).await;
                        continue;
                    }
                    return Ok((status, body));
                }
                Err(error)
                    if attempt + 1 < MAX_WRITE_ATTEMPTS
                        && (error.is_connect() || error.is_timeout()) =>
                {
                    tokio::time::sleep(retry_delay(attempt)).await;
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to request Lark Base {operation}"));
                }
            }
        }
        unreachable!("the bounded retry loop always returns on its final attempt")
    }

    async fn tenant_access_token(&self) -> Result<String> {
        {
            let cached = self.token.lock().await;
            if let Some(token) = cached.as_ref()
                && Instant::now() < token.refresh_at
            {
                return Ok(token.value.clone());
            }
        }

        let url = format!(
            "{}/open-apis/auth/v3/tenant_access_token/internal",
            self.api_base
        );
        let response = self
            .client
            .post(url)
            .json(&serde_json::json!({
                "app_id": self.credentials.app_id,
                "app_secret": self.credentials.app_secret,
            }))
            .send()
            .await
            .context("failed to request Lark tenant access token")?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read Lark authentication response")?;
        if !status.is_success() {
            bail!("Lark authentication failed with HTTP {status}: {body}");
        }
        let token: TokenResponse =
            serde_json::from_str(&body).context("failed to decode Lark authentication response")?;
        if token.code != 0 {
            bail!(
                "Lark authentication failed with code {}: {}",
                token.code,
                token.msg
            );
        }
        let value = token
            .tenant_access_token
            .filter(|value| !value.trim().is_empty())
            .context("Lark authentication response omitted tenant_access_token")?;
        let lifetime = Duration::from_secs(token.expire.unwrap_or(7_200).saturating_sub(60));
        *self.token.lock().await = Some(CachedToken {
            value: value.clone(),
            refresh_at: Instant::now() + lifetime,
        });
        Ok(value)
    }
}

fn validate_upsert_request(request: &UpsertRequest) -> Result<()> {
    validate_target(&request.target)?;
    if request.key_field.trim().is_empty() || request.hash_field.trim().is_empty() {
        bail!("Lark Base key field and hash field must not be blank");
    }
    if request.key_field == request.hash_field {
        bail!("Lark Base key field and hash field must be different");
    }
    let mut keys = HashSet::new();
    for record in &request.records {
        if record.key.trim().is_empty() || record.payload_hash.trim().is_empty() {
            bail!("Lark Base upsert keys and payload hashes must not be blank");
        }
        if !keys.insert(&record.key) {
            bail!("Lark Base upsert contains duplicate key `{}`", record.key);
        }
        if record.fields.contains_key(&request.key_field)
            || record.fields.contains_key(&request.hash_field)
        {
            bail!("managed fields must not redefine the Lark key or hash field");
        }
    }
    Ok(())
}

fn validate_upsert_schema(schema: &TableSchema, request: &UpsertRequest) -> Result<()> {
    let fields = schema
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field.field_type))
        .collect::<HashMap<_, _>>();
    for technical in [&request.key_field, &request.hash_field] {
        match fields.get(technical.as_str()) {
            Some(1) => {}
            Some(actual) => bail!(
                "Lark Base field `{technical}` has type {actual}, expected multi-line text type 1"
            ),
            None => bail!("Lark Base is missing required field `{technical}`"),
        }
    }
    for record in &request.records {
        for (name, value) in &record.fields {
            match fields.get(name.as_str()) {
                Some(actual) if *actual == value.field_type() => {}
                Some(actual) => bail!(
                    "Lark Base field `{name}` has type {actual}, expected type {}",
                    value.field_type()
                ),
                None => bail!("Lark Base is missing mapped field `{name}`"),
            }
        }
    }
    Ok(())
}

fn record_fields(
    key_field: &str,
    hash_field: &str,
    record: &UpsertRecord,
) -> serde_json::Map<String, serde_json::Value> {
    let mut fields = record
        .fields
        .clone()
        .into_iter()
        .map(|(name, value)| (name, value.into_json()))
        .collect::<serde_json::Map<_, _>>();
    fields.insert(key_field.to_string(), record.key.clone().into());
    fields.insert(hash_field.to_string(), record.payload_hash.clone().into());
    fields
}

fn validate_target(target: &BaseTarget) -> Result<()> {
    if target.app_token.trim().is_empty() || target.table_id.trim().is_empty() {
        bail!("Lark Base app token and table ID must not be blank");
    }
    Ok(())
}

#[derive(Deserialize)]
struct TokenResponse {
    code: i64,
    msg: String,
    tenant_access_token: Option<String>,
    expire: Option<u64>,
}

#[derive(Deserialize)]
struct ApiEnvelope<T> {
    code: i64,
    msg: String,
    data: Option<T>,
}

#[derive(Deserialize)]
struct ApiStatus {
    code: i64,
}

impl<T> ApiEnvelope<T> {
    fn into_data(self, operation: &str) -> Result<T> {
        if self.code != 0 {
            bail!(
                "{operation} failed with Lark code {}: {}",
                self.code,
                self.msg
            );
        }
        self.data
            .with_context(|| format!("{operation} response omitted data"))
    }
}

#[derive(Deserialize)]
struct FieldPage {
    #[serde(default)]
    has_more: bool,
    page_token: Option<String>,
    #[serde(default)]
    items: Vec<FieldItem>,
}

#[derive(Deserialize)]
struct FieldItem {
    field_id: String,
    field_name: String,
    #[serde(rename = "type")]
    field_type: u16,
    ui_type: Option<String>,
    #[serde(default)]
    is_primary: bool,
}

struct ExistingRecord {
    record_id: String,
    payload_hash: Option<String>,
}

struct UpsertPlan {
    completed: HashMap<String, UpsertReceipt>,
    creates: Vec<PendingWrite>,
    updates: Vec<PendingWrite>,
}

struct PendingWrite {
    key: String,
    remote_id: Option<String>,
    fields: serde_json::Map<String, serde_json::Value>,
}

fn plan_upsert(request: &UpsertRequest, existing: &HashMap<String, ExistingRecord>) -> UpsertPlan {
    let mut plan = UpsertPlan {
        completed: HashMap::new(),
        creates: Vec::new(),
        updates: Vec::new(),
    };
    for record in &request.records {
        let fields = record_fields(&request.key_field, &request.hash_field, record);
        match existing.get(&record.key) {
            Some(current) if current.payload_hash.as_deref() == Some(&record.payload_hash) => {
                plan.completed.insert(
                    record.key.clone(),
                    UpsertReceipt {
                        key: record.key.clone(),
                        remote_id: Some(current.record_id.clone()),
                        action: UpsertAction::Unchanged,
                    },
                );
            }
            Some(current) => plan.updates.push(PendingWrite {
                key: record.key.clone(),
                remote_id: Some(current.record_id.clone()),
                fields,
            }),
            None => plan.creates.push(PendingWrite {
                key: record.key.clone(),
                remote_id: None,
                fields,
            }),
        }
    }
    plan
}

fn complete_dry_run(plan: &mut UpsertPlan) {
    plan.completed.extend(plan.creates.iter().map(|pending| {
        (
            pending.key.clone(),
            UpsertReceipt {
                key: pending.key.clone(),
                remote_id: None,
                action: UpsertAction::Created,
            },
        )
    }));
    plan.completed.extend(plan.updates.iter().map(|pending| {
        (
            pending.key.clone(),
            UpsertReceipt {
                key: pending.key.clone(),
                remote_id: pending.remote_id.clone(),
                action: UpsertAction::Updated,
            },
        )
    }));
}

fn extend_completed(completed: &mut HashMap<String, UpsertReceipt>, receipts: Vec<UpsertReceipt>) {
    completed.extend(
        receipts
            .into_iter()
            .map(|receipt| (receipt.key.clone(), receipt)),
    );
}

fn finish_report(
    records: &[UpsertRecord],
    mut completed: HashMap<String, UpsertReceipt>,
) -> Result<UpsertReport> {
    let receipts = records
        .iter()
        .map(|record| {
            completed
                .remove(&record.key)
                .with_context(|| format!("Lark upsert omitted result for key {}", record.key))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(UpsertReport {
        created: count_action(&receipts, UpsertAction::Created),
        updated: count_action(&receipts, UpsertAction::Updated),
        unchanged: count_action(&receipts, UpsertAction::Unchanged),
        receipts,
    })
}

fn count_action(receipts: &[UpsertReceipt], action: UpsertAction) -> usize {
    receipts
        .iter()
        .filter(|receipt| receipt.action == action)
        .count()
}

#[derive(Deserialize)]
struct RecordPage {
    #[serde(default)]
    has_more: bool,
    page_token: Option<String>,
    #[serde(default)]
    items: Vec<RecordItem>,
}

#[derive(Deserialize)]
struct RecordItem {
    record_id: String,
    #[serde(default)]
    fields: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct WriteData {
    #[serde(default)]
    records: Vec<WrittenRecord>,
}

#[derive(Deserialize)]
struct WrittenRecord {
    record_id: String,
}

fn text_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Object(value) => value
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned),
        serde_json::Value::Array(values) => {
            let text = values
                .iter()
                .filter_map(|value| match value {
                    serde_json::Value::String(value) => Some(value.as_str()),
                    serde_json::Value::Object(value) => {
                        value.get("text").and_then(serde_json::Value::as_str)
                    }
                    _ => None,
                })
                .collect::<String>();
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn is_transient(status: reqwest::StatusCode, code: Option<i64>) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
        || matches!(code, Some(1_254_290 | 1_254_291 | 1_255_040))
}

const fn retry_delay(attempt: u32) -> Duration {
    Duration::from_millis(100 * 2_u64.pow(attempt))
}

impl From<FieldItem> for TableField {
    fn from(value: FieldItem) -> Self {
        Self {
            id: value.field_id,
            name: value.field_name,
            field_type: value.field_type,
            ui_type: value.ui_type,
            is_primary: value.is_primary,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{body_json, header, method, path, query_param},
    };

    use super::{
        BaseTarget, BaseValue, LarkBaseClient, LarkCredentials, TableField, TableSchema,
        UpsertAction, UpsertMode, UpsertRecord, UpsertRequest,
    };

    async fn mount_auth(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/open-apis/auth/v3/tenant_access_token/internal"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "msg": "ok",
                "tenant_access_token": "tenant-token",
                "expire": 7200
            })))
            .mount(server)
            .await;
    }

    async fn mount_fields(server: &MockServer, items: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path(
                "/open-apis/bitable/v1/apps/app-token/tables/table-id/fields",
            ))
            .and(query_param("page_size", "100"))
            .and(header("authorization", "Bearer tenant-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "msg": "ok",
                "data": {"has_more": false, "items": items}
            })))
            .mount(server)
            .await;
    }

    async fn mount_search(
        server: &MockServer,
        key_field: &str,
        hash_field: &str,
        items: serde_json::Value,
    ) {
        Mock::given(method("POST"))
            .and(path(
                "/open-apis/bitable/v1/apps/app-token/tables/table-id/records/search",
            ))
            .and(query_param("page_size", "500"))
            .and(body_json(json!({
                "field_names": [key_field, hash_field]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "msg": "ok",
                "data": {"has_more": false, "items": items}
            })))
            .mount(server)
            .await;
    }

    async fn mount_write(
        server: &MockServer,
        operation: &str,
        records: serde_json::Value,
        remote_id: &str,
    ) {
        Mock::given(method("POST"))
            .and(path(format!(
                "/open-apis/bitable/v1/apps/app-token/tables/table-id/records/{operation}"
            )))
            .and(body_json(json!({"records": records})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": 0,
                "msg": "ok",
                "data": {
                    "records": [{"record_id": remote_id, "fields": {}}]
                }
            })))
            .mount(server)
            .await;
    }

    fn client(server: &MockServer) -> LarkBaseClient {
        LarkBaseClient::with_api_base(
            LarkCredentials {
                app_id: "cli-test".to_string(),
                app_secret: "secret".to_string(),
            },
            &server.uri(),
        )
        .expect("client")
    }

    fn target() -> BaseTarget {
        BaseTarget {
            app_token: "app-token".to_string(),
            table_id: "table-id".to_string(),
        }
    }

    #[tokio::test]
    async fn caller_can_inspect_a_table_through_one_authenticated_interface() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        mount_fields(
            &server,
            json!([{
                "field_id": "fld-title",
                "field_name": "标题",
                "type": 1,
                "ui_type": "Text",
                "is_primary": true
            }]),
        )
        .await;

        let schema = client(&server)
            .inspect_table(&target())
            .await
            .expect("inspect table");

        assert_eq!(
            schema,
            TableSchema {
                fields: vec![TableField {
                    id: "fld-title".to_string(),
                    name: "标题".to_string(),
                    field_type: 1,
                    ui_type: Some("Text".to_string()),
                    is_primary: true,
                }]
            }
        );
    }

    #[tokio::test]
    async fn caller_can_idempotently_upsert_records_without_knowing_lark_endpoints() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        mount_fields(
            &server,
            json!([
                {"field_id": "fld-key", "field_name": "RSS Key", "type": 1},
                {"field_id": "fld-hash", "field_name": "Payload Hash", "type": 1},
                {"field_id": "fld-title", "field_name": "标题", "type": 1}
            ]),
        )
        .await;
        mount_search(
            &server,
            "RSS Key",
            "Payload Hash",
            json!([
                {
                    "record_id": "rec-same",
                    "fields": {"RSS Key": "same", "Payload Hash": "hash-same"}
                },
                {
                    "record_id": "rec-changed",
                    "fields": {"RSS Key": "changed", "Payload Hash": "old-hash"}
                }
            ]),
        )
        .await;
        mount_write(
            &server,
            "batch_create",
            json!([{
                "fields": {
                    "RSS Key": "new",
                    "Payload Hash": "hash-new",
                    "标题": "New"
                }
            }]),
            "rec-new",
        )
        .await;
        mount_write(
            &server,
            "batch_update",
            json!([{
                "record_id": "rec-changed",
                "fields": {
                    "RSS Key": "changed",
                    "Payload Hash": "hash-changed",
                    "标题": "Changed"
                }
            }]),
            "rec-changed",
        )
        .await;

        let record = |key: &str, hash: &str, title: &str| UpsertRecord {
            key: key.to_string(),
            payload_hash: hash.to_string(),
            fields: BTreeMap::from([("标题".to_string(), BaseValue::Text(title.to_string()))]),
        };
        let report = client(&server)
            .upsert_records(UpsertRequest {
                target: target(),
                key_field: "RSS Key".to_string(),
                hash_field: "Payload Hash".to_string(),
                mode: UpsertMode::Apply,
                records: vec![
                    record("same", "hash-same", "Same"),
                    record("new", "hash-new", "New"),
                    record("changed", "hash-changed", "Changed"),
                ],
            })
            .await
            .expect("upsert");

        assert_eq!(report.created, 1);
        assert_eq!(report.updated, 1);
        assert_eq!(report.unchanged, 1);
        assert_eq!(
            report
                .receipts
                .iter()
                .map(|receipt| (receipt.key.as_str(), receipt.action))
                .collect::<Vec<_>>(),
            vec![
                ("same", UpsertAction::Unchanged),
                ("new", UpsertAction::Created),
                ("changed", UpsertAction::Updated),
            ]
        );
    }

    #[tokio::test]
    async fn transient_lark_write_conflicts_are_retried_behind_the_interface() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        mount_fields(
            &server,
            json!([
                {"field_id": "fld-key", "field_name": "Key", "type": 1},
                {"field_id": "fld-hash", "field_name": "Hash", "type": 1},
                {"field_id": "fld-title", "field_name": "Title", "type": 1}
            ]),
        )
        .await;
        mount_search(&server, "Key", "Hash", json!([])).await;

        let attempts = Arc::new(AtomicUsize::new(0));
        let response_attempts = Arc::clone(&attempts);
        Mock::given(method("POST"))
            .and(path(
                "/open-apis/bitable/v1/apps/app-token/tables/table-id/records/batch_create",
            ))
            .respond_with(move |_: &wiremock::Request| {
                if response_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "code": 1_254_291,
                        "msg": "write conflict"
                    }))
                } else {
                    ResponseTemplate::new(200).set_body_json(json!({
                        "code": 0,
                        "msg": "ok",
                        "data": {
                            "records": [{"record_id": "rec-created", "fields": {}}]
                        }
                    }))
                }
            })
            .mount(&server)
            .await;

        let report = client(&server)
            .upsert_records(UpsertRequest {
                target: target(),
                key_field: "Key".to_string(),
                hash_field: "Hash".to_string(),
                mode: UpsertMode::Apply,
                records: vec![UpsertRecord {
                    key: "one".to_string(),
                    payload_hash: "hash-one".to_string(),
                    fields: BTreeMap::from([(
                        "Title".to_string(),
                        BaseValue::Text("One".to_string()),
                    )]),
                }],
            })
            .await
            .expect("transient conflict should be retried");

        assert_eq!(report.created, 1);
    }

    #[tokio::test]
    async fn dry_run_validates_and_plans_without_calling_write_endpoints() {
        let server = MockServer::start().await;
        mount_auth(&server).await;
        mount_fields(
            &server,
            json!([
                {"field_id": "fld-key", "field_name": "Key", "type": 1},
                {"field_id": "fld-hash", "field_name": "Hash", "type": 1},
                {"field_id": "fld-title", "field_name": "Title", "type": 1}
            ]),
        )
        .await;
        mount_search(&server, "Key", "Hash", json!([])).await;

        let report = client(&server)
            .upsert_records(UpsertRequest {
                target: target(),
                key_field: "Key".to_string(),
                hash_field: "Hash".to_string(),
                mode: UpsertMode::DryRun,
                records: vec![UpsertRecord {
                    key: "one".to_string(),
                    payload_hash: "hash-one".to_string(),
                    fields: BTreeMap::from([(
                        "Title".to_string(),
                        BaseValue::Text("One".to_string()),
                    )]),
                }],
            })
            .await
            .expect("dry run");

        assert_eq!(report.created, 1);
        assert_eq!(report.receipts[0].remote_id, None);
    }
}
