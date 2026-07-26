use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsString,
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use chrono::DateTime;
use clix_core::fs::{atomic_write, parent_or_current};
use clix_x_api::{
    ContentSubtype, ContentType, is_x_article_url, markdown_destination, media_extension,
};
use fs2::FileExt;

use crate::{
    api::article_link_index,
    model::{BookmarkState, OutputFormat, STATE_VERSION, TweetBookmark},
};

const MARKDOWN_HEADER: &str = "| Content Type | Subtypes | Author | Published At | Tweet | Media / Links | Bookmarks | Likes | Replies | Views | Reposts | Quotes |";
const MARKDOWN_SEPARATOR: &str =
    "| --- | --- | --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |";
const MARKDOWN_STATUS_MARKER: &str = "[View Status](";

pub struct ExportLocks {
    _files: [File; 2],
}

pub fn acquire_export_locks(output_path: &Path, state_path: &Path) -> Result<ExportLocks> {
    let mut lock_paths = [
        sibling_lock_path(&comparable_path(output_path)?)?,
        sibling_lock_path(&comparable_path(state_path)?)?,
    ];
    lock_paths.sort_unstable();

    let first = acquire_lock(&lock_paths[0])?;
    let second = acquire_lock(&lock_paths[1])?;
    Ok(ExportLocks {
        _files: [first, second],
    })
}

fn sibling_lock_path(target: &Path) -> Result<PathBuf> {
    let file_name = target
        .file_name()
        .with_context(|| format!("path {} has no file name", target.display()))?;
    let mut lock_name = OsString::from(".");
    lock_name.push(file_name);
    lock_name.push(".clix.lock");
    Ok(target.with_file_name(lock_name))
}

fn acquire_lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("failed to open export lock {}", path.display()))?;
    file.try_lock_exclusive().with_context(|| {
        format!(
            "another clix export is already updating files protected by {}",
            path.display()
        )
    })?;
    Ok(file)
}

#[derive(Debug)]
pub enum ExistingOutput {
    Markdown {
        content: String,
        ids: HashSet<String>,
        row_count: usize,
        line_ending: &'static str,
        needs_rewrite: bool,
    },
    Urls {
        content: String,
        ids: HashSet<String>,
    },
    Json {
        bookmarks: Vec<TweetBookmark>,
        ids: HashSet<String>,
    },
}

impl ExistingOutput {
    pub(super) fn load(path: &Path, format: OutputFormat) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read existing output {}", path.display()))?;
        match format {
            OutputFormat::Markdown => {
                let migrated = migrate_markdown_tweet_cells(&content);
                let needs_rewrite = migrated.is_some();
                let content = migrated.unwrap_or(content);
                let (ids, row_count) = markdown_status_ids_and_count(&content);
                Ok(Self::Markdown {
                    line_ending: if content.contains("\r\n") {
                        "\r\n"
                    } else {
                        "\n"
                    },
                    content,
                    ids,
                    row_count,
                    needs_rewrite,
                })
            }
            OutputFormat::Urls => Ok(Self::Urls {
                ids: url_output_status_ids(&content),
                content,
            }),
            OutputFormat::Json => {
                let bookmarks: Vec<TweetBookmark> =
                    serde_json::from_str(&content).with_context(|| {
                        format!(
                            "JSON output {} uses an invalid or outdated schema; run once without --incremental to migrate it",
                            path.display()
                        )
                    })?;
                let ids = bookmarks
                    .iter()
                    .map(|bookmark| bookmark.id.clone())
                    .collect();
                Ok(Self::Json { bookmarks, ids })
            }
        }
    }

    pub(super) const fn ids(&self) -> &HashSet<String> {
        match self {
            Self::Markdown { ids, .. } | Self::Urls { ids, .. } | Self::Json { ids, .. } => ids,
        }
    }

    pub(super) fn article_titles(&self) -> BTreeMap<String, String> {
        match self {
            Self::Markdown { content, .. } => markdown_article_titles(content),
            Self::Urls { .. } => BTreeMap::new(),
            Self::Json { bookmarks, .. } => bookmarks
                .iter()
                .filter(|bookmark| bookmark.content_type == ContentType::Article)
                .filter_map(|bookmark| {
                    article_link_index(bookmark)
                        .and_then(|index| bookmark.links[index].title.as_ref())
                        .map(|title| (bookmark.id.clone(), title.clone()))
                })
                .collect(),
        }
    }
}

pub fn default_state_path(output_path: &Path) -> PathBuf {
    let candidate = output_path.with_extension("state.json");
    if candidate == output_path {
        let file_name = output_path
            .file_name()
            .map_or_else(|| "x_bookmarks".into(), |name| name.to_string_lossy());
        output_path.with_file_name(format!("{file_name}.sync-state.json"))
    } else {
        candidate
    }
}

pub fn ensure_distinct_paths(output_path: &Path, state_path: &Path) -> Result<()> {
    let output = comparable_path(output_path)?;
    let state = comparable_path(state_path)?;
    if output == state {
        bail!(
            "Output and state must use different paths (both resolve to {})",
            output.display()
        );
    }
    Ok(())
}

fn comparable_path(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return path
            .canonicalize()
            .with_context(|| format!("failed to resolve path {}", path.display()));
    }

    let file_name = path
        .file_name()
        .with_context(|| format!("path {} has no file name", path.display()))?;
    let parent = parent_or_current(path);
    let resolved_parent = parent
        .canonicalize()
        .or_else(|_| std::path::absolute(parent))
        .with_context(|| format!("failed to resolve parent directory {}", parent.display()))?;
    Ok(resolved_parent.join(file_name))
}

pub fn load_state_cache(path: &Path) -> Result<(HashSet<String>, BTreeMap<String, String>)> {
    if !path.exists() {
        return Ok((HashSet::new(), BTreeMap::new()));
    }

    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read state file {}", path.display()))?;
    let state: BookmarkState = serde_json::from_str(&content)
        .with_context(|| format!("invalid state file {}", path.display()))?;
    if state.version != STATE_VERSION {
        bail!(
            "Unsupported bookmark state version {} in {} (expected {})",
            state.version,
            path.display(),
            STATE_VERSION
        );
    }
    Ok((
        state.seen_tweet_ids.into_iter().collect(),
        state.article_titles,
    ))
}

pub fn markdown_status_ids_and_count(content: &str) -> (HashSet<String>, usize) {
    let mut ids = HashSet::new();
    let mut row_count = 0;

    for line in content.lines() {
        let Some(start) = line
            .find(MARKDOWN_STATUS_MARKER)
            .map(|offset| offset + MARKDOWN_STATUS_MARKER.len())
        else {
            continue;
        };
        let Some(end) = line[start..].find(')').map(|offset| start + offset) else {
            continue;
        };
        if let Some(id) = status_id_from_url(&line[start..end]) {
            ids.insert(id);
            row_count += 1;
        }
    }
    (ids, row_count)
}

fn migrate_markdown_tweet_cells(content: &str) -> Option<String> {
    let mut migrated = String::with_capacity(content.len());
    let mut changed = false;

    for chunk in content.split_inclusive('\n') {
        let (line, line_ending) = chunk.strip_suffix("\r\n").map_or_else(
            || {
                chunk
                    .strip_suffix('\n')
                    .map_or((chunk, ""), |line| (line, "\n"))
            },
            |line| (line, "\r\n"),
        );

        if let Some(normalized) = migrate_markdown_row(line) {
            changed |= normalized != line;
            migrated.push_str(&normalized);
        } else {
            migrated.push_str(line);
        }
        migrated.push_str(line_ending);
    }

    changed.then_some(migrated)
}

fn markdown_article_titles(content: &str) -> BTreeMap<String, String> {
    content
        .lines()
        .filter_map(|line| {
            let cells = markdown_row_cells(line)?;
            if cells.len() != 12 {
                return None;
            }
            let id = status_id_from_markdown_row(line)?;
            article_title_from_markdown_links(cells[5]).map(|title| (id, title))
        })
        .collect()
}

fn article_title_from_markdown_links(value: &str) -> Option<String> {
    let mut cursor = 0;
    while let Some((open, label_end, destination_end, is_image)) = next_markdown_link(value, cursor)
    {
        let destination = &value[label_end + 2..destination_end];
        let label = &value[open + 1..label_end];
        if !is_image
            && is_x_article_url(destination)
            && label != "View Article"
            && !label.trim().is_empty()
        {
            return Some(unescape_tweet_fragment(label));
        }
        cursor = destination_end + 1;
    }
    None
}

fn status_id_from_markdown_row(line: &str) -> Option<String> {
    let start = line.find(MARKDOWN_STATUS_MARKER)? + MARKDOWN_STATUS_MARKER.len();
    let end = line[start..].find(')')? + start;
    status_id_from_url(&line[start..end])
}

fn unescape_tweet_fragment(value: &str) -> String {
    let value = value.replace("&lt;", "<").replace("&gt;", ">");
    let mut unescaped = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\\'
            && characters
                .peek()
                .is_some_and(|next| matches!(next, '\\' | '[' | ']' | '|'))
        {
            if let Some(next) = characters.next() {
                unescaped.push(next);
            }
        } else {
            unescaped.push(character);
        }
    }
    unescaped
}

fn migrate_markdown_row(line: &str) -> Option<String> {
    let cells = markdown_row_cells(line)?;
    if cells.len() != 12 || !cells[5].contains(MARKDOWN_STATUS_MARKER) {
        return None;
    }

    let tweet = cells[4];
    if !tweet_cell_needs_migration(tweet) {
        return None;
    }

    let raw_tweet = unescape_markdown_table_pipes(tweet);
    let normalized = render_plain_tweet_text(&raw_tweet);
    let normalized = if normalized.is_empty() {
        "-"
    } else {
        normalized.as_str()
    };
    let migrated_cells = cells
        .iter()
        .enumerate()
        .map(|(index, cell)| if index == 4 { normalized } else { *cell })
        .collect::<Vec<_>>();

    Some(format!("| {} |", migrated_cells.join(" | ")))
}

fn markdown_row_cells(line: &str) -> Option<Vec<&str>> {
    if !line.starts_with('|') || !line.ends_with('|') {
        return None;
    }

    let bytes = line.as_bytes();
    let separators = bytes
        .iter()
        .enumerate()
        .filter_map(|(index, byte)| (*byte == b'|' && !is_escaped(bytes, index)).then_some(index))
        .collect::<Vec<_>>();
    if separators.len() < 2 {
        return None;
    }

    Some(
        separators
            .windows(2)
            .map(|pair| line[pair[0] + 1..pair[1]].trim())
            .collect(),
    )
}

fn tweet_cell_needs_migration(value: &str) -> bool {
    let mut offset = 0;
    while offset < value.len() {
        if web_url_length(&value[offset..]).is_some() {
            return true;
        }
        let Some(character) = value[offset..].chars().next() else {
            break;
        };
        offset += character.len_utf8();
    }
    next_markdown_link(value, 0).is_some()
}

fn unescape_markdown_table_pipes(value: &str) -> String {
    let mut unescaped = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\\' && characters.peek() == Some(&'|') {
            characters.next();
            unescaped.push('|');
        } else {
            unescaped.push(character);
        }
    }
    unescaped
}

fn url_output_status_ids(content: &str) -> HashSet<String> {
    content
        .lines()
        .filter(|line| !line.starts_with(char::is_whitespace))
        .filter_map(|line| line.rsplit_once(' ').map(|(_, url)| url))
        .filter_map(status_id_from_url)
        .collect()
}

fn status_id_from_url(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    if !matches!(
        url.host_str(),
        Some("x.com" | "www.x.com" | "twitter.com" | "www.twitter.com")
    ) {
        return None;
    }

    let mut segments = url.path_segments()?;
    while let Some(segment) = segments.next() {
        if segment == "status" {
            let id = segments.next()?;
            return (!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| id.to_string());
        }
    }
    None
}

pub fn write_state(path: &Path, state: &BookmarkState) -> Result<()> {
    let content =
        serde_json::to_string_pretty(state).context("failed to serialize bookmark state")?;
    atomic_write(path, content.as_bytes())
        .with_context(|| format!("failed to write state file {}", path.display()))
}

pub fn write_output(
    bookmarks: &[TweetBookmark],
    path: &Path,
    format: OutputFormat,
    link_only: bool,
) -> Result<()> {
    let content = render_output(bookmarks, format, link_only)?;
    atomic_write(path, content.as_bytes())
        .with_context(|| format!("failed to write output {}", path.display()))
}

pub fn render_output(
    bookmarks: &[TweetBookmark],
    format: OutputFormat,
    link_only: bool,
) -> Result<String> {
    match format {
        OutputFormat::Markdown => {
            let mut content = String::new();
            content.push_str("# X (Twitter) Bookmarks\n\n");
            writeln!(content, "Total: {} bookmarks\n", bookmarks.len())
                .context("failed to render bookmark total")?;
            content.push_str(MARKDOWN_HEADER);
            content.push('\n');
            content.push_str(MARKDOWN_SEPARATOR);
            content.push('\n');
            for bookmark in bookmarks {
                content.push_str(&render_markdown_row(bookmark, link_only));
            }
            Ok(content)
        }
        OutputFormat::Urls => render_url_entries(bookmarks),
        OutputFormat::Json => {
            serde_json::to_string_pretty(bookmarks).context("failed to serialize JSON output")
        }
    }
}

pub fn append_output(
    bookmarks: &[TweetBookmark],
    path: &Path,
    format: OutputFormat,
    link_only: bool,
    known_ids: &HashSet<String>,
    existing: Option<&ExistingOutput>,
) -> Result<()> {
    let Some(existing) = existing else {
        return write_output(bookmarks, path, format, link_only);
    };
    if bookmarks.is_empty() {
        return rewrite_migrated_markdown(existing, path);
    }

    let unique_new = bookmarks
        .iter()
        .filter(|bookmark| {
            !known_ids.contains(&bookmark.id) && !existing.ids().contains(&bookmark.id)
        })
        .collect::<Vec<_>>();
    if unique_new.is_empty() {
        return rewrite_migrated_markdown(existing, path);
    }

    let merged = match existing {
        ExistingOutput::Markdown {
            content,
            row_count,
            line_ending,
            ..
        } => merge_markdown_output(
            content,
            *row_count,
            line_ending,
            &unique_new,
            link_only,
            path,
        )?,
        ExistingOutput::Urls { content: old, .. } => {
            let mut content = render_url_entries(unique_new.iter().copied())?;
            content.push_str(old);
            content
        }
        ExistingOutput::Json { bookmarks: old, .. } => merge_json_output(old, &unique_new)?,
    };
    atomic_write(path, merged.as_bytes())
        .with_context(|| format!("failed to update output {}", path.display()))
}

fn rewrite_migrated_markdown(existing: &ExistingOutput, path: &Path) -> Result<()> {
    if let ExistingOutput::Markdown {
        content,
        needs_rewrite: true,
        ..
    } = existing
    {
        atomic_write(path, content.as_bytes())
            .with_context(|| format!("failed to migrate existing output {}", path.display()))?;
    }
    Ok(())
}

fn merge_markdown_output(
    existing: &str,
    existing_row_count: usize,
    line_ending: &str,
    bookmarks: &[&TweetBookmark],
    link_only: bool,
    path: &Path,
) -> Result<String> {
    let body_start = markdown_body_start(existing).with_context(|| {
        format!(
            "Cannot incrementally update {} because its Markdown table schema is not current; run once without --incremental to migrate it",
            path.display()
        )
    })?;

    let mut rows = String::new();
    for bookmark in bookmarks {
        rows.push_str(&render_markdown_row(bookmark, link_only));
    }
    if line_ending == "\r\n" {
        rows = rows.replace('\n', "\r\n");
    }

    let mut merged = String::with_capacity(existing.len() + rows.len());
    merged.push_str(&existing[..body_start]);
    merged.push_str(&rows);
    merged.push_str(&existing[body_start..]);

    let total = existing_row_count + bookmarks.len();
    replace_markdown_total(&mut merged, total, path)?;
    Ok(merged)
}

fn markdown_body_start(content: &str) -> Option<usize> {
    let header_start = content.find(MARKDOWN_HEADER)?;
    let after_header = header_start + MARKDOWN_HEADER.len();
    let after_line_ending = if content[after_header..].starts_with("\r\n") {
        after_header + 2
    } else if content[after_header..].starts_with('\n') {
        after_header + 1
    } else {
        return None;
    };
    if !content[after_line_ending..].starts_with(MARKDOWN_SEPARATOR) {
        return None;
    }

    let after_separator = after_line_ending + MARKDOWN_SEPARATOR.len();
    if content[after_separator..].starts_with("\r\n") {
        Some(after_separator + 2)
    } else if content[after_separator..].starts_with('\n') {
        Some(after_separator + 1)
    } else if after_separator == content.len() {
        Some(after_separator)
    } else {
        None
    }
}

fn replace_markdown_total(content: &mut String, total: usize, path: &Path) -> Result<()> {
    let prefix = "Total: ";
    let start = content.find(prefix).with_context(|| {
        format!(
            "Cannot incrementally update {} because its total line is missing",
            path.display()
        )
    })?;
    let line_end = content[start..]
        .find('\n')
        .map_or(content.len(), |offset| start + offset);
    let end = line_end
        - usize::from(
            line_end > start && content.as_bytes().get(line_end - 1).copied() == Some(b'\r'),
        );
    content.replace_range(start..end, &format!("Total: {total} bookmarks"));
    Ok(())
}

fn merge_json_output(existing: &[TweetBookmark], bookmarks: &[&TweetBookmark]) -> Result<String> {
    let mut items = Vec::with_capacity(bookmarks.len() + existing.len());
    items.extend(bookmarks.iter().copied());
    items.extend(existing);
    serde_json::to_string_pretty(&items).context("failed to serialize merged JSON output")
}

fn render_url_entries<'a>(
    bookmarks: impl IntoIterator<Item = &'a TweetBookmark>,
) -> Result<String> {
    let mut content = String::new();
    for bookmark in bookmarks {
        writeln!(
            content,
            "[{}] [{}] {}",
            bookmark.content_type,
            format_subtypes_plain(&bookmark.subtypes),
            bookmark.url
        )
        .context("failed to render bookmark URL entry")?;
        for media in &bookmark.media {
            writeln!(content, "  └─ {media}").context("failed to render bookmark media URL")?;
        }
    }
    Ok(content)
}

fn render_markdown_row(bookmark: &TweetBookmark, link_only: bool) -> String {
    let author_name = escape_markdown_table_cell(&bookmark.author_name);
    let author_handle = escape_markdown_table_cell(&bookmark.author_handle);
    let published_at = format_published_at(&bookmark.created_at);
    let tweet_cell = format_tweet_cell(bookmark, link_only);
    let mut media_links = Vec::new();

    for media_url in resolved_media(bookmark) {
        let destination = markdown_destination(media_url);
        media_links.push(format!("[![Img]({destination})]({destination})"));
    }

    let mut link_items = vec![format!(
        "[View Status]({})",
        markdown_destination(&bookmark.url)
    )];
    let mut rendered_urls = HashSet::new();
    for preview in &bookmark.links {
        let target_url = preview.expanded_url.as_deref().unwrap_or(&preview.url);
        if target_url == bookmark.url || !rendered_urls.insert(target_url) {
            continue;
        }
        let label = if is_x_article_url(target_url) && !link_only {
            preview
                .title
                .as_deref()
                .map(render_article_link_label)
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| "View Article".to_string())
        } else if is_x_article_url(target_url) {
            "View Article".to_string()
        } else {
            "Open Link".to_string()
        };
        link_items.push(format!("[{label}]({})", markdown_destination(target_url)));
    }
    if !media_links.is_empty() {
        link_items.push(format!("🖼️ {}", media_links.join(" ")));
    }
    let media_col = link_items.join("<br/>");
    let subtypes = format_subtypes_markdown(&bookmark.subtypes);

    format!(
        "| `{}` | {} | {} (@{}) | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
        bookmark.content_type,
        subtypes,
        author_name,
        author_handle,
        published_at,
        tweet_cell,
        media_col,
        format_metric(bookmark.metrics.bookmarks),
        format_metric(bookmark.metrics.likes),
        format_metric(bookmark.metrics.replies),
        format_metric(bookmark.metrics.views),
        format_metric(bookmark.metrics.reposts),
        format_metric(bookmark.metrics.quotes)
    )
}

fn render_article_link_label(title: &str) -> String {
    render_plain_tweet_text(title).replace("<br/>", " ")
}

fn resolved_media(bookmark: &TweetBookmark) -> impl Iterator<Item = &str> {
    bookmark.media.iter().enumerate().map(|(index, remote)| {
        let expected_local = format!(
            "./media/{}_{}_{}.{}",
            bookmark.author_handle,
            bookmark.id,
            index + 1,
            media_extension(remote)
        );
        bookmark
            .local_media
            .iter()
            .find(|local| local.as_str() == expected_local)
            .map_or(remote.as_str(), String::as_str)
    })
}

fn format_subtypes_plain(subtypes: &[ContentSubtype]) -> String {
    if subtypes.is_empty() {
        "-".to_string()
    } else {
        subtypes
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn format_subtypes_markdown(subtypes: &[ContentSubtype]) -> String {
    if subtypes.is_empty() {
        "-".to_string()
    } else {
        subtypes
            .iter()
            .map(|subtype| format!("`{subtype}`"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub fn format_tweet_cell(bookmark: &TweetBookmark, link_only: bool) -> String {
    let body = render_plain_tweet_text(&bookmark.text);
    let title = (!link_only)
        .then(|| {
            article_link_index(bookmark)
                .and_then(|index| bookmark.links[index].title.as_deref())
                .or_else(|| {
                    bookmark
                        .links
                        .iter()
                        .find_map(|preview| preview.title.as_deref())
                })
        })
        .flatten()
        .map(render_plain_tweet_text)
        .filter(|title| !title.is_empty());

    if let Some(title) = title.as_deref() {
        if body.is_empty() || body == title {
            title.to_string()
        } else {
            format!("{title}<br/>{body}")
        }
    } else if body.is_empty() {
        "-".to_string()
    } else {
        body
    }
}

fn render_plain_tweet_text(value: &str) -> String {
    value
        .split("<br/>")
        .map(strip_markdown_and_web_links)
        .map(|fragment| escape_tweet_fragment(&fragment))
        .filter(|fragment| !fragment.is_empty())
        .collect::<Vec<_>>()
        .join("<br/>")
}

fn strip_markdown_and_web_links(value: &str) -> String {
    strip_web_urls(&strip_markdown_links(value))
}

fn strip_markdown_links(value: &str) -> String {
    let mut plain = String::with_capacity(value.len());
    let mut cursor = 0;

    while let Some((open, label_end, destination_end, is_image)) = next_markdown_link(value, cursor)
    {
        let prefix_end = if is_image { open - 1 } else { open };
        plain.push_str(&value[cursor..prefix_end]);
        if !is_image {
            plain.push_str(&value[open + 1..label_end]);
        }
        cursor = destination_end + 1;
    }
    plain.push_str(&value[cursor..]);
    plain
}

fn next_markdown_link(value: &str, cursor: usize) -> Option<(usize, usize, usize, bool)> {
    let mut search_from = cursor;
    loop {
        let open = search_from + value[search_from..].find('[')?;
        if is_escaped(value.as_bytes(), open) {
            search_from = open + 1;
            continue;
        }
        let Some(label_end) = find_label_end(value, open + 1) else {
            search_from = open + 1;
            continue;
        };
        let destination_start = label_end + 2;
        let Some(destination_end) = find_destination_end(value, destination_start) else {
            search_from = open + 1;
            continue;
        };
        let is_image = open > cursor
            && value.as_bytes()[open - 1] == b'!'
            && !is_escaped(value.as_bytes(), open - 1);
        return Some((open, label_end, destination_end, is_image));
    }
}

fn find_label_end(value: &str, start: usize) -> Option<usize> {
    value[start..]
        .char_indices()
        .find_map(|(offset, character)| {
            let index = start + offset;
            (character == ']'
                && !is_escaped(value.as_bytes(), index)
                && value[index + 1..].starts_with('('))
            .then_some(index)
        })
}

fn find_destination_end(value: &str, start: usize) -> Option<usize> {
    let mut depth = 1_u32;
    for (offset, character) in value[start..].char_indices() {
        let index = start + offset;
        if is_escaped(value.as_bytes(), index) {
            continue;
        }
        match character {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_escaped(bytes: &[u8], index: usize) -> bool {
    let backslashes = bytes[..index]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count();
    backslashes % 2 == 1
}

fn strip_web_urls(value: &str) -> String {
    let mut cleaned = String::with_capacity(value.len());
    let mut offset = 0;
    let mut pending_space = false;

    while offset < value.len() {
        let remaining = &value[offset..];
        if let Some(mut url_length) = web_url_length(remaining) {
            let closing_wrapper = remaining[url_length..].chars().next();
            if matches!(
                (cleaned.chars().next_back(), closing_wrapper),
                (Some('<'), Some('>')) | (Some('('), Some(')'))
            ) {
                cleaned.pop();
                url_length += closing_wrapper.map_or(0, char::len_utf8);
            }
            offset += url_length;
            pending_space = !cleaned.is_empty();
            continue;
        }

        let Some(character) = remaining.chars().next() else {
            break;
        };
        if character.is_whitespace() {
            pending_space = !cleaned.is_empty();
            offset += character.len_utf8();
            continue;
        }
        if pending_space
            && !matches!(
                character,
                '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}'
            )
        {
            cleaned.push(' ');
        }
        pending_space = false;
        cleaned.push(character);
        offset += character.len_utf8();
    }

    cleaned
}

fn web_url_length(value: &str) -> Option<usize> {
    let prefix_length = ["https://", "http://", "www."]
        .into_iter()
        .find_map(|prefix| {
            value
                .get(..prefix.len())
                .filter(|candidate| candidate.eq_ignore_ascii_case(prefix))
                .map(str::len)
        })?;

    let mut end = prefix_length;
    for (offset, character) in value[prefix_length..].char_indices() {
        if !is_web_url_character(character) {
            break;
        }
        end = prefix_length + offset + character.len_utf8();
    }
    if !value[prefix_length..end]
        .bytes()
        .any(|byte| byte.is_ascii_alphanumeric())
    {
        return None;
    }

    while end > prefix_length
        && matches!(
            value.as_bytes()[end - 1],
            b'.' | b',' | b';' | b':' | b'!' | b'?'
        )
    {
        end -= 1;
    }
    Some(end)
}

const fn is_web_url_character(character: char) -> bool {
    character.is_ascii()
        && !character.is_ascii_whitespace()
        && !matches!(
            character,
            '<' | '>' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}'
        )
}

fn escape_tweet_fragment(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '[' => escaped.push_str("\\["),
            ']' => escaped.push_str("\\]"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '|' => escaped.push_str("\\|"),
            '\r' | '\n' => escaped.push(' '),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn escape_markdown_table_cell(value: &str) -> String {
    escape_tweet_fragment(value)
}

fn format_published_at(value: &str) -> String {
    if value.is_empty() {
        return "-".to_string();
    }

    let formatted = DateTime::parse_from_str(value, "%a %b %d %H:%M:%S %z %Y").map_or_else(
        |_| value.to_string(),
        |value| value.format("%Y-%m-%d %H:%M:%S").to_string(),
    );
    format!("`{formatted}`")
}

fn format_metric(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_string(), |value| value.to_string())
}
