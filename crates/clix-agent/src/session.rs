use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::AgentKind;

const MAX_SESSION_FILES: usize = 50_000;
const MAX_JSON_BYTES: u64 = 64 * 1_024 * 1_024;
const MAX_RECORD_BYTES: usize = 8 * 1_024 * 1_024;

#[derive(Debug, Clone, PartialEq)]
pub struct AgentSession {
    pub kind: AgentKind,
    pub id: String,
    pub project: Option<PathBuf>,
    pub path: PathBuf,
    pub started_at: Option<String>,
    pub updated_at: u64,
    pub tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Default)]
pub struct SessionCatalog {
    home: PathBuf,
    sessions: Vec<AgentSession>,
}

#[derive(Debug, Clone, Copy)]
struct CachedUsage {
    updated_at: u64,
    tokens: Option<u64>,
    cost_usd: Option<f64>,
}

#[derive(Debug, Default)]
pub struct UsageCache {
    entries: BTreeMap<PathBuf, CachedUsage>,
}

impl UsageCache {
    pub fn enrich(
        &mut self,
        catalog: &SessionCatalog,
        session: &AgentSession,
    ) -> Result<AgentSession> {
        if let Some(cached) = self
            .entries
            .get(&session.path)
            .filter(|cached| cached.updated_at == session.updated_at)
        {
            let mut enriched = session.clone();
            enriched.tokens = cached.tokens;
            enriched.cost_usd = cached.cost_usd;
            return Ok(enriched);
        }

        let enriched = catalog.with_usage(session)?;
        self.entries.insert(
            session.path.clone(),
            CachedUsage {
                updated_at: session.updated_at,
                tokens: enriched.tokens,
                cost_usd: enriched.cost_usd,
            },
        );
        Ok(enriched)
    }
}

impl SessionCatalog {
    #[cfg(test)]
    pub fn scan(home: &Path) -> Result<Self> {
        Self::scan_provider(home, None)
    }

    pub fn scan_provider(home: &Path, provider: Option<&AgentKind>) -> Result<Self> {
        let mut sessions = Vec::new();
        if provider.is_none_or(|kind| kind == &AgentKind::Codex) {
            scan_codex(home, &mut sessions)?;
        }
        if provider.is_none_or(|kind| kind == &AgentKind::ClaudeCode) {
            scan_claude(home, &mut sessions)?;
        }
        if provider.is_none_or(|kind| kind == &AgentKind::GeminiCli) {
            scan_gemini(home, &mut sessions)?;
        }
        if provider.is_none_or(|kind| kind == &AgentKind::OpenCode) {
            scan_opencode(home, &mut sessions)?;
        }
        if provider.is_none_or(|kind| kind == &AgentKind::Pi) {
            scan_pi(home, &mut sessions)?;
        }
        if provider.is_none_or(|kind| kind == &AgentKind::OhMyPi) {
            scan_oh_my_pi(home, &mut sessions)?;
        }
        sessions.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(Self {
            home: home.to_path_buf(),
            sessions,
        })
    }

    pub fn sessions(&self) -> &[AgentSession] {
        &self.sessions
    }

    pub fn resolve(&self, provider: Option<&str>, session_id: &str) -> Result<&AgentSession> {
        let mut matches = self.sessions.iter().filter(|session| {
            provider.is_none_or(|name| session.kind.slug() == name)
                && (session.id == session_id || session.id.starts_with(session_id))
        });
        let first = matches.next().with_context(|| {
            let prefix = provider.map_or_else(String::new, |name| format!("{name}:"));
            format!("agent session not found: {prefix}{session_id}")
        })?;
        if matches.next().is_some() {
            bail!(
                "agent session selector is ambiguous: {session_id}; use provider:full-session-id"
            );
        }
        Ok(first)
    }

    pub fn latest_for_process(
        &self,
        kind: &AgentKind,
        project: Option<&Path>,
        process_started_at: u64,
    ) -> Option<&AgentSession> {
        if let Some(project) = project.filter(|project| !is_root(project)) {
            return self
                .sessions
                .iter()
                .filter(|session| {
                    &session.kind == kind && session.project.as_deref() == Some(project)
                })
                .max_by_key(|session| session.updated_at);
        }
        self.sessions
            .iter()
            .filter(|session| {
                &session.kind == kind && session.updated_at >= process_started_at.saturating_sub(5)
            })
            .max_by_key(|session| session.updated_at)
    }

    pub fn with_usage(&self, session: &AgentSession) -> Result<AgentSession> {
        let mut enriched = session.clone();
        let (tokens, cost_usd) = match session.kind {
            AgentKind::Codex => codex_usage(&session.path)?,
            AgentKind::ClaudeCode => claude_usage(&session.path)?,
            AgentKind::GeminiCli => gemini_usage(&session.path)?,
            AgentKind::OpenCode => opencode_usage(&self.home, &session.id)?,
            AgentKind::Pi | AgentKind::OhMyPi => pi_usage(&session.path)?,
            AgentKind::Cursor | AgentKind::Custom(_) => (None, None),
        };
        enriched.tokens = tokens;
        enriched.cost_usd = cost_usd;
        Ok(enriched)
    }
}

fn scan_codex(home: &Path, sessions: &mut Vec<AgentSession>) -> Result<()> {
    for path in files_with_extension(&home.join(".codex/sessions"), "jsonl")? {
        let mut id = None;
        let mut project = None;
        let mut started_at = None;
        visit_bounded_lines_limit(&path, Some(1), |line| {
            let Ok(record) = serde_json::from_slice::<Value>(line) else {
                return;
            };
            if record.get("type").and_then(Value::as_str) == Some("session_meta") {
                id = string_at(&record, "/payload/id");
                project = string_at(&record, "/payload/cwd").map(PathBuf::from);
                started_at = string_at(&record, "/payload/timestamp")
                    .or_else(|| string_at(&record, "/timestamp"));
            }
        })?;
        if let Some(id) = id {
            sessions.push(session(
                AgentKind::Codex,
                id,
                project,
                path,
                started_at,
                None,
                None,
            )?);
        }
    }
    Ok(())
}

fn scan_claude(home: &Path, sessions: &mut Vec<AgentSession>) -> Result<()> {
    for path in files_with_extension(&home.join(".claude/projects"), "jsonl")? {
        let mut id = None;
        let mut project = None;
        let mut started_at = None;
        visit_bounded_lines_limit(&path, Some(64), |line| {
            let Ok(record) = serde_json::from_slice::<Value>(line) else {
                return;
            };
            id = id.take().or_else(|| string_at(&record, "/sessionId"));
            project = project
                .take()
                .or_else(|| string_at(&record, "/cwd").map(PathBuf::from));
            started_at = started_at
                .take()
                .or_else(|| string_at(&record, "/timestamp"));
        })?;
        if let Some(id) = id.or_else(|| file_stem(&path)) {
            sessions.push(session(
                AgentKind::ClaudeCode,
                id,
                project,
                path,
                started_at,
                None,
                None,
            )?);
        }
    }
    Ok(())
}

fn scan_gemini(home: &Path, sessions: &mut Vec<AgentSession>) -> Result<()> {
    let root = home.join(".gemini/tmp");
    for path in files_with_extension(&root, "json")? {
        if path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            != Some("chats")
        {
            continue;
        }
        let Some(value) = read_json_file(&path)? else {
            continue;
        };
        let Some(id) = string_at(&value, "/sessionId") else {
            continue;
        };
        let project = path
            .parent()
            .and_then(Path::parent)
            .map(|directory| directory.join(".project_root"))
            .and_then(|marker| fs::read_to_string(marker).ok())
            .map(|value| PathBuf::from(value.trim()));
        sessions.push(session(
            AgentKind::GeminiCli,
            id,
            project,
            path,
            string_at(&value, "/startTime"),
            None,
            None,
        )?);
    }
    Ok(())
}

fn scan_opencode(home: &Path, sessions: &mut Vec<AgentSession>) -> Result<()> {
    let root = home.join(".local/share/opencode/storage");
    for path in files_with_extension(&root.join("session"), "json")? {
        let Some(value) = read_json_file(&path)? else {
            continue;
        };
        let Some(id) = string_at(&value, "/id") else {
            continue;
        };
        sessions.push(session(
            AgentKind::OpenCode,
            id,
            string_at(&value, "/directory").map(PathBuf::from),
            path,
            value
                .pointer("/time/created")
                .and_then(Value::as_u64)
                .map(|time| time.to_string()),
            None,
            None,
        )?);
    }
    Ok(())
}

fn scan_pi(home: &Path, sessions: &mut Vec<AgentSession>) -> Result<()> {
    scan_pi_sessions(&home.join(".pi/agent/sessions"), &AgentKind::Pi, sessions)
}

fn scan_oh_my_pi(home: &Path, sessions: &mut Vec<AgentSession>) -> Result<()> {
    scan_pi_sessions(
        &home.join(".omp/agent/sessions"),
        &AgentKind::OhMyPi,
        sessions,
    )
}

fn scan_pi_sessions(root: &Path, kind: &AgentKind, sessions: &mut Vec<AgentSession>) -> Result<()> {
    for path in files_with_extension(root, "jsonl")? {
        let mut id = None;
        let mut project = None;
        let mut started_at = None;
        visit_bounded_lines_limit(&path, Some(64), |line| {
            let Ok(record) = serde_json::from_slice::<Value>(line) else {
                return;
            };
            if record.get("type").and_then(Value::as_str) == Some("session") {
                id = string_at(&record, "/id");
                project = string_at(&record, "/cwd").map(PathBuf::from);
                started_at = string_at(&record, "/timestamp");
            }
        })?;
        if let Some(id) = id {
            sessions.push(session(
                kind.clone(),
                id,
                project,
                path,
                started_at,
                None,
                None,
            )?);
        }
    }
    Ok(())
}

fn is_root(path: &Path) -> bool {
    path.parent().is_none()
}

#[allow(clippy::too_many_arguments)]
fn session(
    kind: AgentKind,
    id: String,
    project: Option<PathBuf>,
    path: PathBuf,
    started_at: Option<String>,
    tokens: Option<u64>,
    cost_usd: Option<f64>,
) -> Result<AgentSession> {
    Ok(AgentSession {
        kind,
        id,
        project,
        updated_at: modified_seconds(&path)?,
        path,
        started_at,
        tokens,
        cost_usd,
    })
}

fn files_with_extension(root: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)
            .with_context(|| format!("failed to read agent sessions in {}", directory.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| {
                format!("failed to read an agent session in {}", directory.display())
            })?;
            let file_type = entry.file_type().with_context(|| {
                format!(
                    "failed to inspect agent session path {}",
                    entry.path().display()
                )
            })?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file()
                && entry.path().extension().and_then(|value| value.to_str()) == Some(extension)
            {
                files.push(entry.path());
                if files.len() > MAX_SESSION_FILES {
                    bail!(
                        "agent session scan exceeded {MAX_SESSION_FILES} files under {}",
                        root.display()
                    );
                }
            }
        }
    }
    files.sort();
    Ok(files)
}

fn read_json_file(path: &Path) -> Result<Option<Value>> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to inspect agent session {}", path.display()))?;
    if metadata.len() > MAX_JSON_BYTES {
        return Ok(None);
    }
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read agent session {}", path.display()))?;
    Ok(serde_json::from_slice(&bytes).ok())
}

fn visit_bounded_lines(path: &Path, visitor: impl FnMut(&[u8])) -> Result<bool> {
    visit_bounded_lines_limit(path, None, visitor)
}

fn visit_bounded_lines_limit(
    path: &Path,
    limit: Option<usize>,
    mut visitor: impl FnMut(&[u8]),
) -> Result<bool> {
    let file = File::open(path)
        .with_context(|| format!("failed to read agent session {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut overflow = false;
    let mut skipped = false;
    let mut visited = 0_usize;

    loop {
        let available = reader
            .fill_buf()
            .with_context(|| format!("failed to read agent session {}", path.display()))?;
        if available.is_empty() {
            if !line.is_empty() && !overflow {
                visitor(trim_carriage_return(&line));
            }
            return Ok(skipped);
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let length = newline.map_or(available.len(), |index| index + 1);
        let content_length = newline.unwrap_or(length);
        if !overflow {
            if line.len().saturating_add(content_length) <= MAX_RECORD_BYTES {
                line.extend_from_slice(&available[..content_length]);
            } else {
                overflow = true;
                skipped = true;
                line.clear();
            }
        }
        reader.consume(length);
        if newline.is_some() {
            if !overflow {
                visitor(trim_carriage_return(&line));
                visited += 1;
            }
            line.clear();
            overflow = false;
            if limit.is_some_and(|limit| visited >= limit) {
                return Ok(skipped);
            }
        }
    }
}

fn codex_usage(path: &Path) -> Result<(Option<u64>, Option<f64>)> {
    let mut tokens = None;
    let skipped = visit_bounded_lines(path, |line| {
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        if record.pointer("/payload/type").and_then(Value::as_str) == Some("token_count")
            && let Some(total) = record
                .pointer("/payload/info/total_token_usage/total_tokens")
                .and_then(Value::as_u64)
        {
            tokens = Some(total);
        }
    })?;
    Ok(((!skipped).then_some(tokens).flatten(), None))
}

fn claude_usage(path: &Path) -> Result<(Option<u64>, Option<f64>)> {
    let mut tokens = 0_u64;
    let mut has_usage = false;
    let skipped = visit_bounded_lines(path, |line| {
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        if let Some(usage) = record.pointer("/message/usage") {
            has_usage = true;
            for field in [
                "input_tokens",
                "output_tokens",
                "cache_read_input_tokens",
                "cache_creation_input_tokens",
            ] {
                tokens = tokens
                    .saturating_add(usage.get(field).and_then(Value::as_u64).unwrap_or_default());
            }
        }
    })?;
    Ok(((has_usage && !skipped).then_some(tokens), None))
}

fn gemini_usage(path: &Path) -> Result<(Option<u64>, Option<f64>)> {
    let tokens = read_json_file(path)?.and_then(|value| {
        value
            .get("messages")
            .and_then(Value::as_array)
            .and_then(|messages| {
                let totals: Vec<u64> = messages
                    .iter()
                    .filter_map(|message| message.pointer("/tokens/total").and_then(Value::as_u64))
                    .collect();
                (!totals.is_empty()).then(|| totals.into_iter().sum())
            })
    });
    Ok((tokens, None))
}

fn opencode_usage(home: &Path, id: &str) -> Result<(Option<u64>, Option<f64>)> {
    let root = home.join(".local/share/opencode/storage/message").join(id);
    let mut tokens = 0_u64;
    let mut has_tokens = false;
    let mut cost = 0.0_f64;
    let mut has_cost = false;
    for message_path in files_with_extension(&root, "json")? {
        let Some(message) = read_json_file(&message_path)? else {
            continue;
        };
        if let Some(usage) = message.get("tokens") {
            has_tokens = true;
            for pointer in [
                "/input",
                "/output",
                "/reasoning",
                "/cache/read",
                "/cache/write",
            ] {
                tokens = tokens.saturating_add(
                    usage
                        .pointer(pointer)
                        .and_then(Value::as_u64)
                        .unwrap_or_default(),
                );
            }
        }
        if let Some(value) = message.get("cost").and_then(Value::as_f64) {
            has_cost = true;
            cost += value;
        }
    }
    Ok((has_tokens.then_some(tokens), has_cost.then_some(cost)))
}

fn pi_usage(path: &Path) -> Result<(Option<u64>, Option<f64>)> {
    let mut tokens = 0_u64;
    let mut has_tokens = false;
    let mut cost = 0.0_f64;
    let mut has_cost = false;
    let skipped = visit_bounded_lines(path, |line| {
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        if let Some(usage) = record.pointer("/message/usage") {
            if let Some(value) = usage.get("totalTokens").and_then(Value::as_u64) {
                has_tokens = true;
                tokens = tokens.saturating_add(value);
            }
            if let Some(value) = usage.pointer("/cost/total").and_then(Value::as_f64) {
                has_cost = true;
                cost += value;
            }
        }
    })?;
    Ok((
        (has_tokens && !skipped).then_some(tokens),
        (has_cost && !skipped).then_some(cost),
    ))
}

pub fn tail_records(path: &Path, limit: usize) -> Result<Vec<String>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut records = VecDeque::with_capacity(limit.min(1_024));
    visit_bounded_lines(path, |line| {
        if records.len() == limit {
            records.pop_front();
        }
        records.push_back(String::from_utf8_lossy(line).into_owned());
    })?;
    Ok(records.into_iter().collect())
}

fn modified_seconds(path: &Path) -> Result<u64> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .map_or(Ok(0), |duration| Ok(duration.as_secs()))
}

fn string_at(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn file_stem(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|value| value.to_str())
        .map(str::to_owned)
}

fn trim_carriage_return(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::SessionCatalog;
    use crate::AgentKind;

    #[test]
    fn indexes_supported_session_formats_and_real_usage_fields() {
        let temp = tempdir().expect("temp home");
        let home = temp.path();

        let codex = home.join(".codex/sessions/2026/01/02/codex.jsonl");
        write(
            &codex,
            concat!(
                "{\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-id\",\"cwd\":\"/work/codex\",\"timestamp\":\"2026-01-02T03:04:05Z\"}}\n",
                "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"total_tokens\":120}}}}\n"
            ),
        );

        let claude = home.join(".claude/projects/project/claude-id.jsonl");
        write(
            &claude,
            concat!(
                "{\"type\":\"user\",\"sessionId\":\"claude-id\",\"cwd\":\"/work/claude\",\"timestamp\":\"2026-01-02T03:04:05Z\"}\n",
                "{\"type\":\"assistant\",\"sessionId\":\"claude-id\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":20,\"cache_read_input_tokens\":30,\"cache_creation_input_tokens\":40}}}\n"
            ),
        );

        let gemini_root = home.join(".gemini/tmp/gemini-project");
        write(&gemini_root.join(".project_root"), "/work/gemini\n");
        write(
            &gemini_root.join("chats/session.json"),
            r#"{"sessionId":"gemini-id","startTime":"2026-01-02T03:04:05Z","lastUpdated":"2026-01-02T03:05:05Z","messages":[{"type":"gemini","tokens":{"total":55}}]}"#,
        );

        write(
            &home.join(".local/share/opencode/storage/session/project/opencode.json"),
            r#"{"id":"opencode-id","directory":"/work/opencode","time":{"created":1760000000000,"updated":1760000001000}}"#,
        );
        write(
            &home.join(".local/share/opencode/storage/message/opencode-id/message.json"),
            r#"{"role":"assistant","tokens":{"input":11,"output":12,"reasoning":13,"cache":{"read":14,"write":15}},"cost":0.25}"#,
        );

        write(
            &home.join(".pi/agent/sessions/project/pi.jsonl"),
            concat!(
                "{\"type\":\"session\",\"id\":\"pi-id\",\"cwd\":\"/work/pi\",\"timestamp\":\"2026-01-02T03:04:05Z\"}\n",
                "{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"usage\":{\"totalTokens\":77,\"cost\":{\"total\":0.5}}}}\n"
            ),
        );

        write(
            &home.join(".omp/agent/sessions/project/omp.jsonl"),
            concat!(
                "{\"type\":\"title\",\"title\":\"Fixture\"}\n",
                "{\"type\":\"session\",\"id\":\"omp-id\",\"cwd\":\"/work/omp\",\"timestamp\":\"2026-01-02T03:04:05Z\"}\n",
                "{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"usage\":{\"totalTokens\":88,\"cost\":{\"total\":0.75}}}}\n"
            ),
        );

        let catalog = SessionCatalog::scan(home).expect("scan sessions");
        assert_eq!(catalog.sessions().len(), 6);
        assert_session(&catalog, &AgentKind::Codex, "codex-id", 120, None);
        assert_session(&catalog, &AgentKind::ClaudeCode, "claude-id", 100, None);
        assert_session(&catalog, &AgentKind::GeminiCli, "gemini-id", 55, None);
        assert_session(
            &catalog,
            &AgentKind::OpenCode,
            "opencode-id",
            65,
            Some(0.25),
        );
        assert_session(&catalog, &AgentKind::Pi, "pi-id", 77, Some(0.5));
        assert_session(&catalog, &AgentKind::OhMyPi, "omp-id", 88, Some(0.75));
    }

    #[test]
    fn matches_a_recent_session_when_a_gui_agent_reports_the_root_directory() {
        let temp = tempdir().expect("temp home");
        let session_path = temp.path().join(".codex/sessions/2026/01/02/codex.jsonl");
        write(
            &session_path,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-id\",\"cwd\":\"/work/project\"}}\n",
        );
        let catalog = SessionCatalog::scan(temp.path()).expect("scan sessions");
        let updated_at = catalog.sessions()[0].updated_at;

        let matched = catalog
            .latest_for_process(
                &AgentKind::Codex,
                Some(std::path::Path::new("/")),
                updated_at,
            )
            .expect("recent Codex session");

        assert_eq!(matched.id, "codex-id");
        assert_eq!(
            matched.project.as_deref(),
            Some(std::path::Path::new("/work/project"))
        );
        assert!(
            catalog
                .latest_for_process(
                    &AgentKind::Codex,
                    Some(std::path::Path::new("/work/other-project")),
                    updated_at,
                )
                .is_none(),
            "a meaningful cwd must never fall back to another project's session"
        );
    }

    fn assert_session(
        catalog: &SessionCatalog,
        kind: &AgentKind,
        id: &str,
        tokens: u64,
        cost_usd: Option<f64>,
    ) {
        let session = catalog
            .sessions()
            .iter()
            .find(|session| &session.kind == kind)
            .expect("session kind");
        let session = catalog.with_usage(session).expect("load session usage");
        assert_eq!(session.id, id);
        assert_eq!(session.tokens, Some(tokens));
        assert_eq!(session.cost_usd, cost_usd);
    }

    fn write(path: &std::path::Path, contents: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("create fixture directory");
        fs::write(path, contents).expect("write fixture");
    }
}
