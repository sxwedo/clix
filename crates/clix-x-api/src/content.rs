use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::markdown_destination;

pub fn extract_full_text(target: &Value, media_urls: &[String]) -> String {
    let mut media_map: HashMap<String, String> = HashMap::new();
    let article_target = target
        .pointer("/article/article_results/result")
        .unwrap_or(target);

    if let Some(media_entities) = article_target
        .get("media_entities")
        .and_then(Value::as_array)
    {
        for media in media_entities {
            let media_id = media.get("media_id").and_then(value_as_string);
            let image_url = media
                .pointer("/media_info/original_img_url")
                .or_else(|| media.get("media_url_https"))
                .and_then(Value::as_str);

            if let (Some(id), Some(url)) = (media_id, image_url)
                && is_safe_web_url(url)
            {
                media_map.insert(id, url.to_string());
            }
        }
    }

    if let Some(content_state) = target
        .pointer("/article/article_results/result/content_state")
        .or_else(|| target.pointer("/article/content_state"))
        && let Some(rich_text) = render_content_state(content_state, &media_map)
        && !rich_text.trim().is_empty()
    {
        return rich_text;
    }

    let article_plain = target
        .pointer("/article/article_results/result/plain_text")
        .or_else(|| target.pointer("/article/plain_text"))
        .or_else(|| target.pointer("/article/article_results/result/body/text"))
        .or_else(|| target.pointer("/article/body/text"))
        .or_else(|| target.pointer("/article/article_results/result/content/text"))
        .or_else(|| target.pointer("/article/content/text"))
        .or_else(|| target.pointer("/article/preview_text"))
        .and_then(Value::as_str);
    if let Some(text) = article_plain
        && !text.trim().is_empty()
    {
        return escape_markdown_text(text);
    }

    let note_result = target
        .pointer("/note_tweet/note_tweet_results/result")
        .or_else(|| target.pointer("/note_tweet"));
    if let Some(note_text) = note_result
        .and_then(|note| note.get("text"))
        .and_then(Value::as_str)
        && !note_text.trim().is_empty()
    {
        let urls = note_result
            .and_then(|note| note.pointer("/entity_set/urls"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        return render_url_entities(note_text, urls);
    }

    let legacy_source = target
        .pointer("/legacy/full_text")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut legacy_text = render_legacy_entities(target, legacy_source, !media_urls.is_empty());
    if !media_urls.is_empty() && !legacy_text.contains("![Image") {
        for url in media_urls.iter().filter(|url| is_safe_web_url(url)) {
            legacy_text.push_str("\n\n![Image](");
            legacy_text.push_str(&markdown_destination(url));
            legacy_text.push(')');
        }
    }
    legacy_text
}

fn render_content_state(
    content_state: &Value,
    media_map: &HashMap<String, String>,
) -> Option<String> {
    let blocks = content_state.get("blocks")?.as_array()?;
    let entity_map_value = content_state
        .get("entityMap")
        .or_else(|| content_state.get("entity_map"));
    let mut entity_map: HashMap<String, &Value> = HashMap::new();

    if let Some(entity_map_value) = entity_map_value {
        if let Some(items) = entity_map_value.as_array() {
            for item in items {
                if let Some(key) = item.get("key").and_then(value_as_string) {
                    entity_map.insert(key, item.get("value").unwrap_or(item));
                }
            }
        } else if let Some(items) = entity_map_value.as_object() {
            for (key, value) in items {
                entity_map.insert(key.clone(), value.get("value").unwrap_or(value));
            }
        }
    }

    let mut lines = Vec::new();
    for block in blocks {
        let block_type = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unstyled");
        let text = if block_type == "code-block" {
            block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        } else {
            render_inline_text(block, &entity_map)
        };
        let line = match block_type {
            "header-one" => format!("# {text}"),
            "header-two" => format!("## {text}"),
            "header-three" => format!("### {text}"),
            "header-four" => format!("#### {text}"),
            "code-block" => render_code_block(&text),
            "unordered-list-item" => format!("- {text}"),
            "ordered-list-item" => format!("1. {text}"),
            "blockquote" => format!("> {text}"),
            "atomic" => {
                let Some(rendered) = render_atomic_block(block, &entity_map, media_map) else {
                    continue;
                };
                rendered
            }
            _ => text,
        };

        if !line.trim().is_empty() {
            lines.push(line);
        }
    }

    (!lines.is_empty()).then(|| lines.join("\n\n"))
}

struct RangedDecoration {
    start: usize,
    end: usize,
    decoration: InlineDecoration,
}

enum InlineDecoration {
    Link(String),
    Bold,
    Italic,
    Strikethrough,
    Underline,
    Code(String),
}

impl InlineDecoration {
    const fn rank(&self) -> u8 {
        match self {
            Self::Link(_) => 0,
            Self::Bold => 1,
            Self::Italic => 2,
            Self::Strikethrough => 3,
            Self::Underline => 4,
            Self::Code(_) => 5,
        }
    }

    fn push_opening(&self, output: &mut String) {
        match self {
            Self::Link(_) => output.push('['),
            Self::Bold => output.push_str("**"),
            Self::Italic => output.push('_'),
            Self::Strikethrough => output.push_str("~~"),
            Self::Underline => output.push_str("<u>"),
            Self::Code(delimiter) => {
                output.push_str(delimiter);
                output.push(' ');
            }
        }
    }

    fn push_closing(&self, output: &mut String) {
        match self {
            Self::Link(url) => {
                output.push_str("](");
                output.push_str(url);
                output.push(')');
            }
            Self::Bold => output.push_str("**"),
            Self::Italic => output.push('_'),
            Self::Strikethrough => output.push_str("~~"),
            Self::Underline => output.push_str("</u>"),
            Self::Code(delimiter) => {
                output.push(' ');
                output.push_str(delimiter);
            }
        }
    }
}

fn render_inline_text(block: &Value, entity_map: &HashMap<String, &Value>) -> String {
    let text = block.get("text").and_then(Value::as_str).unwrap_or("");
    let decorations = collect_inline_decorations(block, text, entity_map);

    if decorations.is_empty() {
        return escape_markdown_text(text);
    }

    let mut boundaries = Vec::with_capacity(decorations.len() * 2 + 2);
    boundaries.extend([0, text.len()]);
    for decoration in &decorations {
        boundaries.extend([decoration.start, decoration.end]);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut output = String::with_capacity(text.len() + decorations.len() * 8);
    let mut active = Vec::<usize>::new();
    for segment in boundaries.windows(2) {
        let start = segment[0];
        let end = segment[1];
        if start == end {
            continue;
        }

        let mut next_active = decorations
            .iter()
            .enumerate()
            .filter_map(|(index, decoration)| {
                (decoration.start <= start && decoration.end >= end).then_some(index)
            })
            .collect::<Vec<_>>();
        next_active.sort_unstable_by_key(|index| {
            let decoration = &decorations[*index];
            (
                decoration.decoration.rank(),
                decoration.start,
                usize::MAX - decoration.end,
                *index,
            )
        });

        let shared_prefix = active
            .iter()
            .zip(&next_active)
            .take_while(|(left, right)| left == right)
            .count();
        for index in active[shared_prefix..].iter().rev() {
            decorations[*index].decoration.push_closing(&mut output);
        }
        for index in &next_active[shared_prefix..] {
            decorations[*index].decoration.push_opening(&mut output);
        }

        let fragment = &text[start..end];
        if next_active
            .iter()
            .any(|index| matches!(decorations[*index].decoration, InlineDecoration::Code(_)))
        {
            output.push_str(fragment);
        } else {
            output.push_str(&escape_markdown_text(fragment));
        }
        active = next_active;
    }
    for index in active.iter().rev() {
        decorations[*index].decoration.push_closing(&mut output);
    }
    output
}

fn collect_inline_decorations(
    block: &Value,
    text: &str,
    entity_map: &HashMap<String, &Value>,
) -> Vec<RangedDecoration> {
    let mut decorations = Vec::new();
    if let Some(ranges) = block
        .get("entityRanges")
        .or_else(|| block.get("entity_ranges"))
        .and_then(Value::as_array)
    {
        for range in ranges {
            let Some(key) = range.get("key").and_then(value_as_string) else {
                continue;
            };
            let Some(url) = entity_map
                .get(&key)
                .and_then(|entity| entity_url(entity))
                .filter(|url| is_safe_web_url(url))
            else {
                continue;
            };
            if let Some((start, end)) = draft_range_bytes(text, range) {
                decorations.push(RangedDecoration {
                    start,
                    end,
                    decoration: InlineDecoration::Link(markdown_destination(url)),
                });
            }
        }
    }
    collect_style_decorations(block, text, &mut decorations);
    decorations
}

fn collect_style_decorations(block: &Value, text: &str, decorations: &mut Vec<RangedDecoration>) {
    let Some(ranges) = block
        .get("inlineStyleRanges")
        .or_else(|| block.get("inline_style_ranges"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for range in ranges {
        let Some((start, end)) = draft_range_bytes(text, range) else {
            continue;
        };
        let decoration = match range.get("style").and_then(Value::as_str) {
            Some("BOLD") => Some(InlineDecoration::Bold),
            Some("ITALIC") => Some(InlineDecoration::Italic),
            Some("STRIKETHROUGH") => Some(InlineDecoration::Strikethrough),
            Some("UNDERLINE") => Some(InlineDecoration::Underline),
            Some("CODE" | "MONOSPACE") => Some(InlineDecoration::Code(code_span_delimiter(
                &text[start..end],
            ))),
            _ => None,
        };
        if let Some(decoration) = decoration {
            decorations.push(RangedDecoration {
                start,
                end,
                decoration,
            });
        }
    }
}

fn entity_url(entity: &Value) -> Option<&str> {
    entity
        .pointer("/data/url")
        .or_else(|| entity.pointer("/data/expanded_url"))
        .or_else(|| entity.pointer("/data/href"))
        .or_else(|| entity.pointer("/data/link"))
        .and_then(Value::as_str)
}

fn draft_range_bytes(text: &str, range: &Value) -> Option<(usize, usize)> {
    let offset = range.get("offset").and_then(value_as_usize)?;
    let length = range.get("length").and_then(value_as_usize)?;
    if length == 0 {
        return None;
    }
    let start = utf16_offset_to_byte(text, offset)?;
    let end = utf16_offset_to_byte(text, offset.checked_add(length)?)?;
    (start < end).then_some((start, end))
}

fn utf16_offset_to_byte(value: &str, target: usize) -> Option<usize> {
    let mut utf16_offset = 0;
    for (byte_offset, character) in value.char_indices() {
        if utf16_offset == target {
            return Some(byte_offset);
        }
        utf16_offset += character.len_utf16();
        if utf16_offset > target {
            return None;
        }
    }
    (utf16_offset == target).then_some(value.len())
}

fn escape_markdown_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '{' | '}' => escaped.push(character),
            character if character.is_ascii_punctuation() => {
                escaped.push('\\');
                escaped.push(character);
            }
            _ => escaped.push(character),
        }
    }
    escaped
}

fn code_span_delimiter(text: &str) -> String {
    "`".repeat(longest_backtick_run(text).saturating_add(1).max(1))
}

fn render_code_block(text: &str) -> String {
    let fence = "`".repeat(longest_backtick_run(text).saturating_add(1).max(3));
    format!("{fence}\n{text}\n{fence}")
}

fn longest_backtick_run(text: &str) -> usize {
    text.split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0)
}

fn is_safe_web_url(value: &str) -> bool {
    reqwest::Url::parse(value)
        .ok()
        .is_some_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
}

fn render_atomic_block(
    block: &Value,
    entity_map: &HashMap<String, &Value>,
    media_map: &HashMap<String, String>,
) -> Option<String> {
    let ranges = block
        .get("entityRanges")
        .or_else(|| block.get("entity_ranges"))
        .and_then(Value::as_array)?;
    let mut rendered = Vec::new();
    let mut seen = HashSet::new();

    for range in ranges {
        let Some(key) = range.get("key").and_then(value_as_string) else {
            continue;
        };
        let Some(entity) = entity_map.get(&key) else {
            continue;
        };
        for item in render_atomic_entity(entity, media_map) {
            if seen.insert(item.clone()) {
                rendered.push(item);
            }
        }
    }

    (!rendered.is_empty()).then(|| rendered.join("\n\n"))
}

fn render_atomic_entity(entity: &Value, media_map: &HashMap<String, String>) -> Vec<String> {
    let entity_type = entity.get("type").and_then(Value::as_str).unwrap_or("");

    match entity_type {
        "MEDIA" | "IMAGE" => {
            let media_items = entity
                .pointer("/data/mediaItems")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if media_items.is_empty() {
                media_markdown(entity.pointer("/data").unwrap_or(entity), media_map)
                    .into_iter()
                    .collect()
            } else {
                media_items
                    .iter()
                    .filter_map(|item| media_markdown(item, media_map))
                    .collect()
            }
        }
        "MARKDOWN" => entity
            .pointer("/data/markdown")
            .and_then(Value::as_str)
            .map(escape_markdown_text)
            .into_iter()
            .collect(),
        "DIVIDER" => vec!["---".to_string()],
        "TWEET" => entity
            .pointer("/data/tweetId")
            .and_then(Value::as_str)
            .map(|id| format!("[Embedded Tweet: https://x.com/i/status/{id}]"))
            .into_iter()
            .collect(),
        "LINK" => entity
            .pointer("/data/url")
            .and_then(Value::as_str)
            .map(|url| {
                if is_safe_web_url(url) {
                    format!(
                        "[{}]({})",
                        escape_markdown_text(url),
                        markdown_destination(url)
                    )
                } else {
                    escape_markdown_text(url)
                }
            })
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}

fn media_markdown(item: &Value, media_map: &HashMap<String, String>) -> Option<String> {
    let media_id = item
        .get("mediaId")
        .or_else(|| item.get("media_id"))
        .and_then(value_as_string);
    let image_url = media_id
        .as_deref()
        .and_then(|id| media_map.get(id))
        .map(String::as_str)
        .or_else(|| {
            item.pointer("/media_info/original_img_url")
                .or_else(|| item.get("media_url_https"))
                .or_else(|| item.get("url"))
                .or_else(|| item.get("src"))
                .or_else(|| item.pointer("/image/url"))
                .and_then(Value::as_str)
        })?;
    is_safe_web_url(image_url).then(|| format!("![Image]({})", markdown_destination(image_url)))
}

fn render_url_entities(text: &str, entities: &[Value]) -> String {
    render_text_replacements(text, link_replacements(text, entities))
}

fn render_legacy_entities(target: &Value, text: &str, omit_media: bool) -> String {
    let urls = target
        .pointer("/legacy/entities/urls")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let media = target
        .pointer("/legacy/extended_entities/media")
        .or_else(|| target.pointer("/legacy/entities/media"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut replacements = link_replacements(text, urls);
    if omit_media {
        for entity in media {
            push_entity_replacements(text, entity, String::new(), &mut replacements);
        }
    } else {
        replacements.extend(link_replacements(text, media));
    }
    render_text_replacements(text, replacements)
}

fn link_replacements(text: &str, entities: &[Value]) -> Vec<(usize, usize, String)> {
    let mut replacements = Vec::new();
    for entity in entities {
        let Some(replacement) = render_link_entity(entity) else {
            continue;
        };
        push_entity_replacements(text, entity, replacement, &mut replacements);
    }
    replacements
}

fn render_link_entity(entity: &Value) -> Option<String> {
    let target = entity
        .get("expanded_url")
        .or_else(|| entity.get("unwound_url"))
        .or_else(|| entity.get("url"))
        .and_then(Value::as_str)?;
    let label = entity
        .get("display_url")
        .and_then(Value::as_str)
        .unwrap_or(target);
    Some(if is_safe_web_url(target) {
        format!(
            "[{}]({})",
            escape_markdown_text(label),
            markdown_destination(target)
        )
    } else {
        escape_markdown_text(label)
    })
}

fn push_entity_replacements(
    text: &str,
    entity: &Value,
    replacement: String,
    replacements: &mut Vec<(usize, usize, String)>,
) {
    if let Some((start, end)) = entity_range_bytes(text, entity) {
        replacements.push((start, end, replacement));
    } else if let Some(source) = entity
        .get("url")
        .and_then(Value::as_str)
        .filter(|source| !source.is_empty())
    {
        replacements.extend(
            text.match_indices(source)
                .map(|(start, source)| (start, start + source.len(), replacement.clone())),
        );
    }
}

fn render_text_replacements(text: &str, mut replacements: Vec<(usize, usize, String)>) -> String {
    replacements.sort_unstable_by_key(|(start, end, _)| (*start, std::cmp::Reverse(*end)));
    let mut rendered = String::with_capacity(text.len());
    let mut next_boundary = 0;
    for (start, end, replacement) in replacements {
        if start >= next_boundary && start < end && end <= text.len() {
            rendered.push_str(&escape_markdown_text(&text[next_boundary..start]));
            rendered.push_str(&replacement);
            next_boundary = end;
        }
    }
    rendered.push_str(&escape_markdown_text(&text[next_boundary..]));
    rendered
}

fn entity_range_bytes(text: &str, entity: &Value) -> Option<(usize, usize)> {
    let indices = entity.get("indices").and_then(Value::as_array);
    let start = indices
        .and_then(|indices| indices.first())
        .and_then(value_as_usize)
        .or_else(|| entity.get("from_index").and_then(value_as_usize))
        .or_else(|| entity.get("start").and_then(value_as_usize))?;
    let end = indices
        .and_then(|indices| indices.get(1))
        .and_then(value_as_usize)
        .or_else(|| entity.get("to_index").and_then(value_as_usize))
        .or_else(|| entity.get("end").and_then(value_as_usize))?;
    let start = utf16_offset_to_byte(text, start)?;
    let end = utf16_offset_to_byte(text, end)?;
    (start < end).then_some((start, end))
}

fn value_as_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| value.as_u64().map(|number| number.to_string()))
        .or_else(|| value.as_i64().map(|number| number.to_string()))
}

fn value_as_usize(value: &Value) -> Option<usize> {
    value
        .as_u64()
        .and_then(|number| usize::try_from(number).ok())
        .or_else(|| value.as_str()?.parse().ok())
}

/// Extract an X author's display name and handle from supported payload shapes.
#[must_use]
pub fn extract_author(target: &Value) -> (String, String) {
    let user_result = target
        .pointer("/core/user_results/result")
        .or_else(|| target.pointer("/user_results/result"))
        .or_else(|| target.pointer("/core/user_result/result"))
        .or_else(|| target.pointer("/user_result/result"));

    let Some(result) = user_result else {
        return ("unknown".to_string(), "unknown".to_string());
    };
    let user =
        if result.get("__typename").and_then(Value::as_str) == Some("UserWithVisibilityResults") {
            result.get("user").unwrap_or(result)
        } else {
            result
        };
    let handle = user
        .pointer("/core/screen_name")
        .or_else(|| user.pointer("/legacy/screen_name"))
        .or_else(|| user.get("screen_name"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let name = user
        .pointer("/core/name")
        .or_else(|| user.pointer("/legacy/name"))
        .or_else(|| user.get("name"))
        .and_then(Value::as_str)
        .unwrap_or(&handle)
        .to_string();
    (name, handle)
}

/// Collect unique media URLs from X Article and legacy post payloads.
#[must_use]
pub fn extract_media_urls(target: &Value) -> Vec<String> {
    let mut urls = Vec::new();
    let mut seen = HashSet::new();
    let article_target = target
        .pointer("/article/article_results/result")
        .unwrap_or(target);

    if let Some(media_entities) = article_target
        .get("media_entities")
        .and_then(Value::as_array)
    {
        for media in media_entities {
            let image_url = media
                .pointer("/media_info/original_img_url")
                .or_else(|| media.get("media_url_https"))
                .and_then(Value::as_str);
            if let Some(url) = image_url
                && is_safe_web_url(url)
                && seen.insert(url)
            {
                urls.push(url.to_string());
            }
        }
    }

    if let Some(url) = article_target
        .pointer("/cover_media/media_url_https")
        .or_else(|| article_target.pointer("/cover_media/media_info/original_img_url"))
        .and_then(Value::as_str)
        && is_safe_web_url(url)
        && seen.insert(url)
    {
        urls.push(url.to_string());
    }

    let note_media = target
        .pointer("/note_tweet/note_tweet_results/result/media/inline_media")
        .or_else(|| target.pointer("/note_tweet/note_tweet_results/result/media/inlineMedia"))
        .or_else(|| target.pointer("/note_tweet/media/inline_media"))
        .and_then(Value::as_array);
    if let Some(media_items) = note_media {
        for media in media_items {
            if let Some(url) = media
                .pointer("/media_info/original_img_url")
                .or_else(|| media.get("media_url_https"))
                .or_else(|| media.pointer("/media/media_url_https"))
                .or_else(|| media.pointer("/media/url"))
                .and_then(Value::as_str)
                && is_safe_web_url(url)
                && seen.insert(url)
            {
                urls.push(url.to_string());
            }
        }
    }

    let legacy_media = target
        .pointer("/legacy/extended_entities/media")
        .or_else(|| target.pointer("/legacy/entities/media"))
        .and_then(Value::as_array);
    if let Some(media_items) = legacy_media {
        for media in media_items {
            if let Some(url) = media.get("media_url_https").and_then(Value::as_str)
                && is_safe_web_url(url)
                && seen.insert(url)
            {
                urls.push(url.to_string());
            }
        }
    }

    urls
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::{extract_full_text, extract_media_urls, render_content_state};

    #[test]
    fn unknown_atomic_blocks_do_not_discard_following_article_text() {
        let content_state = json!({
            "blocks": [
                {"type": "atomic", "text": "", "entityRanges": []},
                {"type": "unstyled", "text": "Visible paragraph"}
            ],
            "entityMap": {}
        });

        assert_eq!(
            render_content_state(&content_state, &HashMap::new()).as_deref(),
            Some("Visible paragraph")
        );
    }

    #[test]
    fn article_and_legacy_media_are_collected_once_in_source_order() {
        let post = json!({
            "article": {
                "article_results": {
                    "result": {
                        "media_entities": [
                            {"media_url_https": "https://img.example/inline.jpg"},
                            {"media_url_https": "data:image/svg+xml,unsafe"}
                        ],
                        "cover_media": {
                            "media_url_https": "https://img.example/cover.jpg"
                        }
                    }
                }
            },
            "legacy": {
                "extended_entities": {
                    "media": [
                        {"media_url_https": "https://img.example/cover.jpg"},
                        {"media_url_https": "https://img.example/legacy.jpg"}
                    ]
                }
            }
        });

        assert_eq!(
            extract_media_urls(&post),
            [
                "https://img.example/inline.jpg",
                "https://img.example/cover.jpg",
                "https://img.example/legacy.jpg"
            ]
        );
    }

    #[test]
    fn note_text_precedes_the_legacy_fallback() {
        let post = json!({
            "note_tweet": {"note_tweet_results": {"result": {"text": "Long note"}}},
            "legacy": {"full_text": "Truncated"}
        });
        assert_eq!(extract_full_text(&post, &[]), "Long note");
    }

    #[test]
    fn article_inline_styles_links_and_utf16_ranges_are_rendered() {
        let content_state = json!({
            "blocks": [{
                "type": "unstyled",
                "text": "😀 docs bold",
                "entityRanges": [{"offset": 3, "length": 4, "key": 0}],
                "inlineStyleRanges": [{"offset": 8, "length": 4, "style": "BOLD"}]
            }],
            "entityMap": {
                "0": {
                    "type": "LINK",
                    "data": {"url": "https://example.com/a_(b)"}
                }
            }
        });

        assert_eq!(
            render_content_state(&content_state, &HashMap::new()).as_deref(),
            Some("😀 [docs](https://example.com/a_%28b%29) **bold**")
        );
    }

    #[test]
    fn raw_html_is_escaped_but_generated_underline_and_code_remain_structured() {
        let content_state = json!({
            "blocks": [
                {"type": "unstyled", "text": "<u>literal</u>"},
                {
                    "type": "unstyled",
                    "text": "safe",
                    "inlineStyleRanges": [
                        {"offset": 0, "length": 4, "style": "UNDERLINE"}
                    ]
                },
                {
                    "type": "unstyled",
                    "text": "a`b",
                    "inlineStyleRanges": [
                        {"offset": 0, "length": 3, "style": "CODE"}
                    ]
                },
                {
                    "type": "unstyled",
                    "text": "`foo`",
                    "inlineStyleRanges": [
                        {"offset": 0, "length": 5, "style": "CODE"}
                    ]
                },
                {"type": "code-block", "text": "const tag = `<Tag>`; ```"}
            ],
            "entityMap": {}
        });

        assert_eq!(
            render_content_state(&content_state, &HashMap::new()).as_deref(),
            Some(
                "&lt;u&gt;literal&lt;\\/u&gt;\n\n\
                 <u>safe</u>\n\n`` a`b ``\n\n`` `foo` ``\n\n\
                 ````\nconst tag = `<Tag>`; ```\n````"
            )
        );
    }

    #[test]
    fn raw_markdown_and_unsafe_structured_links_are_inert() {
        let raw =
            "![track](https://attacker.example/pixel) # title *bold* [x](javascript:alert(1))";
        let legacy = json!({"legacy": {"full_text": raw}});
        let content_state = json!({
            "blocks": [
                {"type": "unstyled", "text": raw},
                {
                    "type": "unstyled",
                    "text": "unsafe",
                    "entityRanges": [{"offset": 0, "length": 6, "key": 0}]
                },
                {
                    "type": "atomic",
                    "text": "",
                    "entityRanges": [{"offset": 0, "length": 0, "key": 1}]
                }
            ],
            "entityMap": {
                "0": {"type": "LINK", "data": {"url": "javascript:alert(1)"}},
                "1": {"type": "MARKDOWN", "data": {"markdown": raw}}
            }
        });

        for rendered in [
            extract_full_text(&legacy, &[]),
            extract_full_text(&legacy, &["data:image/svg+xml,unsafe".to_string()]),
            render_content_state(&content_state, &HashMap::new())
                .expect("content state should render"),
        ] {
            assert!(!rendered.contains("![track]("));
            assert!(!rendered.contains("](javascript:"));
            assert!(!rendered.contains("data:image"));
            assert!(rendered.contains("\\!\\[track\\]"));
        }
    }

    #[test]
    fn atomic_media_entities_render_every_image() {
        let content_state = json!({
            "blocks": [{
                "type": "atomic",
                "text": "",
                "entityRanges": [{"offset": 0, "length": 0, "key": 0}]
            }],
            "entityMap": {
                "0": {
                    "type": "MEDIA",
                    "data": {
                        "mediaItems": [
                            {"mediaId": "1"},
                            {"mediaId": "2"}
                        ]
                    }
                }
            }
        });
        let media = HashMap::from([
            ("1".to_string(), "https://img.example/one.jpg".to_string()),
            ("2".to_string(), "https://img.example/two.jpg".to_string()),
        ]);

        assert_eq!(
            render_content_state(&content_state, &media).as_deref(),
            Some(
                "![Image](https://img.example/one.jpg)\n\n\
                 ![Image](https://img.example/two.jpg)"
            )
        );
    }

    #[test]
    fn note_entities_keep_expanded_links_and_inline_media() {
        let post = json!({
            "note_tweet": {
                "note_tweet_results": {
                    "result": {
                        "text": "😀 https://t.co/x end",
                        "entity_set": {
                            "urls": [{
                                "indices": [3, 17],
                                "url": "https://t.co/x",
                                "expanded_url": "https://example.com/x",
                                "display_url": "example.com/x"
                            }]
                        },
                        "media": {
                            "inline_media": [{
                                "media_info": {
                                    "original_img_url": "https://img.example/note.jpg"
                                }
                            }]
                        }
                    }
                }
            },
            "legacy": {"full_text": "truncated"}
        });

        assert_eq!(
            extract_full_text(&post, &[]),
            "😀 [example\\.com\\/x](https://example.com/x) end"
        );
        assert_eq!(extract_media_urls(&post), ["https://img.example/note.jpg"]);
    }

    #[test]
    fn legacy_entities_restore_links_and_drop_rendered_media_placeholders() {
        let post = json!({
            "legacy": {
                "full_text": "See https://t.co/x https://t.co/m",
                "entities": {
                    "urls": [{
                        "indices": [4, 18],
                        "url": "https://t.co/x",
                        "expanded_url": "https://example.com/x",
                        "display_url": "example.com/x"
                    }],
                    "media": [{
                        "indices": [19, 33],
                        "url": "https://t.co/m",
                        "expanded_url": "https://x.com/alice/status/1/photo/1"
                    }]
                },
                "extended_entities": {
                    "media": [{
                        "indices": [19, 33],
                        "url": "https://t.co/m",
                        "expanded_url": "https://x.com/alice/status/1/photo/1",
                        "media_url_https": "https://img.example/photo.jpg"
                    }]
                }
            }
        });

        let rendered = extract_full_text(&post, &["https://img.example/photo.jpg".to_string()]);
        assert!(rendered.contains("[example\\.com\\/x](https://example.com/x)"));
        assert!(!rendered.contains("t\\.co\\/m"));
        assert!(rendered.contains("![Image](https://img.example/photo.jpg)"));
    }
}
