use std::{
    borrow::Cow,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use clix_core::{
    fs::{atomic_write, parent_or_current},
    ui,
};
use clix_x_api::{XCredentials, build_media_client, markdown_destination, media_extension};

pub use clix_x_api::{TweetDetail, fetch_tweet_detail};

const MAX_CONCURRENT_MEDIA_REQUESTS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReadOutputFormat {
    Markdown,
    Mdx,
    Json,
}

#[derive(Debug, Args)]
pub struct ReadArgs {
    /// X status URL or post ID (for example, <https://x.com/user/status/123456789>)
    pub url_or_id: String,

    /// X `auth_token` cookie (or set `X_AUTH_TOKEN` / `TWITTER_AUTH_TOKEN`)
    #[arg(long)]
    pub auth_token: Option<String>,

    /// X `ct0` (CSRF) cookie (or set `X_CT0` / `TWITTER_CT0`)
    #[arg(long)]
    pub ct0: Option<String>,

    /// Output path (default: `<author_handle>:<title>.md`)
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Output format: markdown, mdx, or json
    #[arg(short, long, value_enum, default_value_t = ReadOutputFormat::Markdown)]
    pub format: ReadOutputFormat,

    /// Skip downloading media images locally into `media/` folder
    #[arg(long)]
    pub no_media: bool,
}

struct MediaJob {
    media_index: usize,
    url: String,
    destination: PathBuf,
    relative_path: String,
}

/// Download one X post and render it in the requested local format.
///
/// # Errors
///
/// Returns an error for an invalid post identifier, missing credentials,
/// failed X requests, media-directory failures, or output serialization and
/// write failures.
pub async fn run(args: ReadArgs) -> Result<()> {
    let tweet_id = extract_tweet_id(&args.url_or_id)?;

    let Some(credentials) = XCredentials::resolve(args.auth_token, args.ct0) else {
        bail!(
            "Missing X authentication credentials!\n\n\
             Please provide both credentials via CLI flags or env vars:\n  \
             clix x read <URL> --auth-token \"<auth_token>\" --ct0 \"<ct0>\"\n\n\
             Or set environment variables:\n  \
             export X_AUTH_TOKEN=\"...\"\n  \
             export X_CT0=\"...\""
        );
    };

    let client = credentials.build_client()?;
    let spinner = ui::create_spinner(&format!("fetching X status {tweet_id}..."));

    let mut detail = fetch_tweet_detail(&client, &tweet_id).await?;
    spinner.finish_and_clear();

    let extension = match args.format {
        ReadOutputFormat::Markdown => "md",
        ReadOutputFormat::Mdx => "mdx",
        ReadOutputFormat::Json => "json",
    };
    let title = detail.article_title.clone().unwrap_or_else(|| {
        markdown_text_to_plain(detail.text.lines().next().unwrap_or(&detail.id))
    });
    let default_file_name = default_output_file_name(&detail.author_handle, &title, extension);
    let output_path = resolve_output_path(args.output, default_file_name)?;

    if !args.no_media && !detail.media_urls.is_empty() {
        let media_client = build_media_client()?;
        download_tweet_media(&media_client, &mut detail, &output_path).await?;
    }

    write_tweet_file(&detail, &output_path, args.format)?;

    let classification = if detail.subtypes.is_empty() {
        detail.content_type.to_string()
    } else {
        format!(
            "{} [{}]",
            detail.content_type,
            detail
                .subtypes
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    ui::success(format!(
        "saved X {} by @{} to {}",
        ui::style_bold(&classification),
        ui::style_bold(&detail.author_handle),
        ui::style_bold(&output_path.display().to_string())
    ));

    Ok(())
}

fn resolve_output_path(output: Option<PathBuf>, default_file_name: String) -> Result<PathBuf> {
    let Some(output) = output else {
        return Ok(PathBuf::from(default_file_name));
    };

    let output_text = output.to_string_lossy();
    if output.is_dir() || output_text.ends_with('/') || output_text.ends_with('\\') {
        fs::create_dir_all(&output)
            .with_context(|| format!("failed to create output directory {}", output.display()))?;
        Ok(output.join(default_file_name))
    } else {
        Ok(output)
    }
}

fn default_output_file_name(author_handle: &str, title: &str, extension: &str) -> String {
    format!("{author_handle}:{}.{}", sanitize_filename(title), extension)
}

fn extract_tweet_id(input: &str) -> Result<String> {
    let input = input.trim();
    if input.is_empty() {
        bail!("X status URL or Tweet ID cannot be empty");
    }
    if input.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(input.to_string());
    }

    if let Ok(url) = reqwest::Url::parse(input)
        && matches!(
            url.host_str(),
            Some("x.com" | "www.x.com" | "twitter.com" | "www.twitter.com")
        )
        && let Some(mut segments) = url.path_segments()
    {
        while let Some(segment) = segments.next() {
            if segment == "status"
                && let Some(id) = segments.next()
                && !id.is_empty()
                && id.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Ok(id.to_string());
            }
        }
    }

    bail!("invalid X status URL or Tweet ID: `{input}`")
}

async fn download_tweet_media(
    client: &reqwest::Client,
    detail: &mut TweetDetail,
    output_path: &Path,
) -> Result<()> {
    let base_dir = parent_or_current(output_path);
    let media_dir = base_dir.join("media");
    fs::create_dir_all(&media_dir).context("failed to create media directory")?;

    let spinner = ui::create_spinner("downloading status images...");
    let mut downloaded = 0;
    let mut skipped = 0;
    let mut failed = 0;
    let mut available_media = Vec::new();
    let mut jobs = Vec::new();

    for (index, media_url) in detail.media_urls.iter().enumerate() {
        let extension = media_extension(media_url);
        let file_name = format!(
            "{}_{}_{}.{}",
            detail.author_handle,
            detail.id,
            index + 1,
            extension
        );
        let destination = media_dir.join(&file_name);
        let relative_path = format!("./media/{file_name}");

        if destination.exists() {
            skipped += 1;
            available_media.push((index, media_url.clone(), relative_path));
        } else {
            jobs.push(MediaJob {
                media_index: index,
                url: media_url.clone(),
                destination,
                relative_path,
            });
        }
    }

    let job_count = jobs.len();
    let mut pending = jobs.into_iter();
    let mut tasks = tokio::task::JoinSet::new();
    for job in pending.by_ref().take(MAX_CONCURRENT_MEDIA_REQUESTS) {
        spawn_media_download(&mut tasks, client.clone(), job);
    }

    let mut completed = 0;
    while let Some(joined) = tasks.join_next().await {
        completed += 1;
        spinner.set_message(format!(
            "downloading status images ({completed}/{job_count})"
        ));
        match joined {
            Ok((job, Ok(()))) => {
                downloaded += 1;
                available_media.push((job.media_index, job.url, job.relative_path));
            }
            Ok((job, Err(error))) => {
                failed += 1;
                ui::warn(format!("could not download {}: {error:#}", job.url));
            }
            Err(error) => {
                failed += 1;
                ui::warn(format!("media download task failed: {error}"));
            }
        }
        if let Some(job) = pending.next() {
            spawn_media_download(&mut tasks, client.clone(), job);
        }
    }

    available_media.sort_unstable_by_key(|(media_index, _, _)| *media_index);
    for (_, media_url, relative_path) in available_media {
        if !detail.local_media.contains(&relative_path) {
            detail.local_media.push(relative_path.clone());
        }
        detail.text = detail.text.replace(&media_url, &relative_path);
        let encoded_url = markdown_destination(&media_url);
        if encoded_url != media_url {
            detail.text = detail.text.replace(&encoded_url, &relative_path);
        }
    }

    spinner.finish_and_clear();
    ui::success(format!(
        "media: {downloaded} downloaded, {skipped} reused, {failed} failed; directory {}",
        ui::style_bold(&media_dir.display().to_string())
    ));
    Ok(())
}

fn spawn_media_download(
    tasks: &mut tokio::task::JoinSet<(MediaJob, Result<()>)>,
    client: reqwest::Client,
    job: MediaJob,
) {
    tasks.spawn(async move {
        let result = async {
            let response = client.get(&job.url).send().await?.error_for_status()?;
            let bytes = response.bytes().await?;
            let destination = job.destination.clone();
            tokio::task::spawn_blocking(move || atomic_write(&destination, &bytes))
                .await
                .context("media persistence task failed")?
        }
        .await;
        (job, result)
    });
}

fn write_tweet_file(detail: &TweetDetail, path: &Path, format: ReadOutputFormat) -> Result<()> {
    let content = match format {
        ReadOutputFormat::Markdown => render_markdown(detail)?,
        ReadOutputFormat::Mdx => render_mdx(detail)?,
        ReadOutputFormat::Json => {
            serde_json::to_string_pretty(detail).context("failed to serialize JSON output")?
        }
    };

    atomic_write(path, content.as_bytes())
        .with_context(|| format!("failed to write X status output {}", path.display()))
}

fn render_markdown(detail: &TweetDetail) -> Result<String> {
    render_markup(detail, false)
}

fn render_mdx(detail: &TweetDetail) -> Result<String> {
    render_markup(detail, true)
}

fn render_markup(detail: &TweetDetail, mdx_safe: bool) -> Result<String> {
    let mut content = String::new();
    let display_title = detail.article_title.as_deref().map_or_else(
        || {
            let first_line = detail.text.lines().next().unwrap_or(&detail.author_name);
            markdown_text_to_plain(first_line)
                .chars()
                .take(80)
                .collect()
        },
        ToOwned::to_owned,
    );
    let author = format!("{} (@{})", detail.author_name, detail.author_handle);

    content.push_str("---\n");
    write_frontmatter_value(&mut content, "title", &display_title)?;
    write_frontmatter_value(&mut content, "author", &author)?;
    write_frontmatter_value(&mut content, "url", &detail.url)?;
    write_frontmatter_value(&mut content, "date", &detail.created_at)?;
    write_frontmatter_value(
        &mut content,
        "content_type",
        &detail.content_type.to_string(),
    )?;
    write_frontmatter_sequence(
        &mut content,
        "subtypes",
        detail.subtypes.iter().map(ToString::to_string),
    )?;
    // Retained so existing consumers can migrate from the original reader schema.
    write_frontmatter_value(&mut content, "type", &detail.tweet_type.to_string())?;
    content.push_str("---\n\n");

    if let Some(title) = &detail.article_title {
        writeln!(content, "# 📰 {}\n", escape_markdown_heading(title))
            .context("failed to render article title")?;
    }

    let body = if mdx_safe {
        Cow::Owned(sanitize_mdx(&detail.text))
    } else {
        Cow::Borrowed(detail.text.as_str())
    };
    content.push_str(&body);
    content.push_str("\n\n");

    let missing_media = resolved_media(detail)
        .filter(|media| !media_is_embedded(&detail.text, media))
        .collect::<Vec<_>>();
    if !missing_media.is_empty() {
        content.push_str("### 🖼️ Attached Media\n\n");
        for (index, image) in missing_media.iter().enumerate() {
            writeln!(
                content,
                "![Image {}]({})\n",
                index + 1,
                markdown_destination(image)
            )
            .context("failed to render attached media")?;
        }
    }

    Ok(content)
}

fn escape_markdown_heading(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '`' | '*' | '_' | '[' | ']' => {
                escaped.push('\\');
                escaped.push(character);
            }
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '{' => escaped.push_str("&#123;"),
            '}' => escaped.push_str("&#125;"),
            '\r' | '\n' => escaped.push(' '),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn markdown_text_to_plain(value: &str) -> String {
    let value = value.replace("&lt;", "<").replace("&gt;", ">");
    let mut plain = String::with_capacity(value.len());
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\\' && characters.peek().is_some_and(char::is_ascii_punctuation) {
            if let Some(next) = characters.next() {
                plain.push(next);
            }
        } else {
            plain.push(character);
        }
    }
    plain
}

fn sanitize_mdx(value: &str) -> String {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    let mut sanitized = String::with_capacity(value.len());
    let mut open_fence = None;

    for line in normalized.split_inclusive('\n') {
        if let Some(fence) = open_fence {
            sanitized.push_str(line);
            if is_closing_fence(line, fence) {
                open_fence = None;
            }
            continue;
        }
        if let Some(fence) = opening_fence(line) {
            sanitized.push_str(line);
            open_fence = Some(fence);
            continue;
        }

        sanitize_mdx_line(line, &mut sanitized);
    }
    sanitized
}

#[derive(Clone, Copy)]
struct MdxFence {
    marker: u8,
    length: usize,
}

fn opening_fence(line: &str) -> Option<MdxFence> {
    let (marker, length, remainder) = fence_run(line)?;
    if marker == b'`' && remainder.contains('`') {
        return None;
    }
    Some(MdxFence { marker, length })
}

fn is_closing_fence(line: &str, opening: MdxFence) -> bool {
    fence_run(line).is_some_and(|(marker, length, remainder)| {
        marker == opening.marker
            && length >= opening.length
            && remainder.trim_matches([' ', '\t', '\r', '\n']).is_empty()
    })
}

fn fence_run(line: &str) -> Option<(u8, usize, &str)> {
    let bytes = line.as_bytes();
    let indentation = bytes.iter().take_while(|byte| **byte == b' ').count();
    if indentation > 3 {
        return None;
    }
    let marker = *bytes.get(indentation)?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let length = bytes[indentation..]
        .iter()
        .take_while(|byte| **byte == marker)
        .count();
    (length >= 3).then(|| (marker, length, &line[indentation + length..]))
}

fn sanitize_mdx_line(line: &str, sanitized: &mut String) {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let body = &line[indent..];
    sanitized.push_str(&line[..indent]);
    if starts_mdx_esm(body) {
        sanitized.push_str(if body.starts_with('i') {
            "&#105;"
        } else {
            "&#101;"
        });
        sanitize_mdx_inline(&body[1..], sanitized);
    } else {
        sanitize_mdx_inline(body, sanitized);
    }
}

fn starts_mdx_esm(value: &str) -> bool {
    value.starts_with("import") || value.starts_with("export")
}

fn sanitize_mdx_inline(value: &str, sanitized: &mut String) {
    let mut cursor = 0;
    let mut inline_code_delimiter = None;
    let mut underline_depth = 0_u32;
    let allow_underline = has_balanced_underline_tags(value);
    while cursor < value.len() {
        let remaining = &value[cursor..];
        if remaining.starts_with('`') {
            let run = remaining.bytes().take_while(|byte| *byte == b'`').count();
            let escaped = backtick_is_escaped(value, cursor);
            sanitized.push_str(&remaining[..run]);
            inline_code_delimiter =
                next_code_span_state(inline_code_delimiter, escaped, run, &remaining[run..]);
            cursor += run;
            continue;
        }

        let Some(character) = remaining.chars().next() else {
            break;
        };
        if inline_code_delimiter.is_some() {
            sanitized.push(character);
            cursor += character.len_utf8();
            continue;
        }
        if allow_underline && remaining.starts_with("<u>") {
            underline_depth += 1;
            sanitized.push_str("<u>");
            cursor += 3;
            continue;
        }
        if allow_underline && remaining.starts_with("</u>") && underline_depth > 0 {
            underline_depth -= 1;
            let tag_length = 4;
            sanitized.push_str(&remaining[..tag_length]);
            cursor += tag_length;
            continue;
        }
        match character {
            '{' => sanitized.push_str("&#123;"),
            '}' => sanitized.push_str("&#125;"),
            '<' => sanitized.push_str("&lt;"),
            '>' => sanitized.push_str("&gt;"),
            _ => sanitized.push(character),
        }
        cursor += character.len_utf8();
    }
}

fn has_balanced_underline_tags(value: &str) -> bool {
    let mut cursor = 0;
    let mut inline_code_delimiter = None;
    let mut depth = 0_u32;
    let mut found = false;

    while cursor < value.len() {
        let remaining = &value[cursor..];
        if remaining.starts_with('`') {
            let run = remaining.bytes().take_while(|byte| *byte == b'`').count();
            inline_code_delimiter = next_code_span_state(
                inline_code_delimiter,
                backtick_is_escaped(value, cursor),
                run,
                &remaining[run..],
            );
            cursor += run;
            continue;
        }
        let Some(character) = remaining.chars().next() else {
            break;
        };
        if inline_code_delimiter.is_some() {
            cursor += character.len_utf8();
            continue;
        }
        if remaining.starts_with("<u>") {
            depth += 1;
            found = true;
            cursor += 3;
            continue;
        }
        if remaining.starts_with("</u>") {
            if depth == 0 {
                return false;
            }
            depth -= 1;
            cursor += 4;
            continue;
        }
        cursor += character.len_utf8();
    }
    found && depth == 0
}

fn next_code_span_state(
    current: Option<usize>,
    escaped: bool,
    delimiter_length: usize,
    remainder: &str,
) -> Option<usize> {
    match current {
        Some(opening) if opening == delimiter_length => None,
        None if !escaped && has_closing_code_span(remainder, delimiter_length) => {
            Some(delimiter_length)
        }
        state => state,
    }
}

fn has_closing_code_span(mut value: &str, delimiter_length: usize) -> bool {
    while let Some(start) = value.find('`') {
        value = &value[start..];
        let run = value.bytes().take_while(|byte| *byte == b'`').count();
        if run == delimiter_length {
            return true;
        }
        value = &value[run..];
    }
    false
}

fn backtick_is_escaped(value: &str, index: usize) -> bool {
    value.as_bytes()[..index]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count()
        % 2
        == 1
}

fn media_is_embedded(text: &str, media: &str) -> bool {
    text.contains(media) || text.contains(&markdown_destination(media))
}

fn resolved_media(detail: &TweetDetail) -> impl Iterator<Item = &str> {
    detail.media_urls.iter().enumerate().map(|(index, remote)| {
        let expected_local = format!(
            "./media/{}_{}_{}.{}",
            detail.author_handle,
            detail.id,
            index + 1,
            media_extension(remote)
        );
        detail
            .local_media
            .iter()
            .find(|local| local.as_str() == expected_local)
            .map_or(remote.as_str(), String::as_str)
    })
}

fn write_frontmatter_value(content: &mut String, key: &str, value: &str) -> Result<()> {
    let value = serde_json::to_string(value)
        .with_context(|| format!("failed to serialize frontmatter {key}"))?;
    writeln!(content, "{key}: {value}")
        .with_context(|| format!("failed to render frontmatter {key}"))
}

fn write_frontmatter_sequence(
    content: &mut String,
    key: &str,
    values: impl IntoIterator<Item = String>,
) -> Result<()> {
    let values = values.into_iter().collect::<Vec<_>>();
    let value = serde_json::to_string(&values)
        .with_context(|| format!("failed to serialize frontmatter {key}"))?;
    writeln!(content, "{key}: {value}")
        .with_context(|| format!("failed to render frontmatter {key}"))
}

fn sanitize_filename(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|character| {
            if character.is_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect();

    let mut result = String::with_capacity(clean.len());
    for part in clean.split('_').filter(|part| !part.is_empty()) {
        if !result.is_empty() {
            result.push('_');
        }
        result.push_str(part);
    }
    let truncated: String = result.chars().take(50).collect();
    if truncated.is_empty() {
        "post".to_string()
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use clix_x_api::{ContentType, TweetType};

    use super::{
        ReadOutputFormat, TweetDetail, default_output_file_name, extract_tweet_id,
        markdown_text_to_plain, render_markdown, render_mdx, sanitize_filename, write_tweet_file,
    };

    #[test]
    fn tweet_id_requires_a_nonempty_id_and_an_x_host() {
        assert_eq!(
            extract_tweet_id("123456789").expect("numeric ID should parse"),
            "123456789"
        );
        assert_eq!(
            extract_tweet_id("https://x.com/alice/status/123456789?s=20")
                .expect("X URL should parse"),
            "123456789"
        );
        assert!(extract_tweet_id("").is_err());
        assert!(extract_tweet_id("https://example.com/alice/status/123456789").is_err());
        assert!(extract_tweet_id("https://x.com/alice/status/not-a-number").is_err());
    }

    #[test]
    fn frontmatter_escapes_special_characters_and_writes_atomically() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join("status.md");
        let detail = TweetDetail {
            id: "123".into(),
            content_type: ContentType::Post,
            subtypes: Vec::new(),
            tweet_type: TweetType::Tweet,
            author_name: "A \"quoted\"\nname".into(),
            author_handle: "alice".into(),
            text: "Body".into(),
            article_title: None,
            created_at: "Wed Oct 10 20:19:24 +0000 2018".into(),
            url: "https://x.com/alice/status/123".into(),
            media_urls: Vec::new(),
            local_media: Vec::new(),
        };

        write_tweet_file(&detail, &path, ReadOutputFormat::Markdown)
            .expect("Markdown should be written");
        let output = std::fs::read_to_string(path).expect("Markdown should be readable");
        assert!(output.contains("author: \"A \\\"quoted\\\"\\nname (@alice)\""));
        assert!(output.contains("url: \"https://x.com/alice/status/123\""));
        assert!(output.contains("content_type: \"post\""));
        assert!(output.contains("subtypes: []"));
        assert!(output.contains("type: \"Tweet\""));
    }

    #[test]
    fn filename_formatting_is_bounded_and_stable() {
        assert_eq!(sanitize_filename("  hello / 世界  "), "hello_世界");
        assert_eq!(sanitize_filename("***"), "post");
        assert_eq!(
            default_output_file_name("raft_hq", "Don't talk to me, talk to my agents", "md"),
            "raft_hq:Don_t_talk_to_me_talk_to_my_agents.md"
        );
        assert_eq!(
            markdown_text_to_plain(r"Cost \$100 for GPT\-5\.6 &lt;today&gt;"),
            "Cost $100 for GPT-5.6 <today>"
        );
    }

    #[test]
    fn markdown_keeps_unembedded_and_failed_media_sources() {
        let detail = TweetDetail {
            id: "123".into(),
            content_type: ContentType::Article,
            subtypes: Vec::new(),
            tweet_type: TweetType::Article,
            author_name: "Alice".into(),
            author_handle: "alice".into(),
            text: "Inline\n\n![Image](./media/alice_123_1.jpg)".into(),
            article_title: Some("Article".into()),
            created_at: "Wed Oct 10 20:19:24 +0000 2018".into(),
            url: "https://x.com/alice/status/123".into(),
            media_urls: vec![
                "https://pbs.twimg.com/media/inline.jpg".into(),
                "https://pbs.twimg.com/media/cover.png".into(),
                "https://pbs.twimg.com/media/failed.webp".into(),
            ],
            local_media: vec![
                "./media/alice_123_1.jpg".into(),
                "./media/alice_123_2.png".into(),
            ],
        };

        let output = render_markdown(&detail).expect("Markdown should render");
        assert_eq!(
            output.matches("./media/alice_123_1.jpg").count(),
            1,
            "an inline image must not be duplicated"
        );
        assert!(output.contains("./media/alice_123_2.png"));
        assert!(output.contains("https://pbs.twimg.com/media/failed.webp"));
    }

    #[test]
    fn mdx_neutralizes_expressions_jsx_and_esm_but_preserves_code() {
        let detail = TweetDetail {
            id: "123".into(),
            content_type: ContentType::Article,
            subtypes: Vec::new(),
            tweet_type: TweetType::Article,
            author_name: "Alice".into(),
            author_handle: "alice".into(),
            text: concat!(
                "export const value = {danger}\n\n",
                "import/**/x from \"pkg\"\nexport/**/const y = 1\n\n",
                "<Widget /> {name} `<Code /> {literal}`\n\n",
                "```\nconst safe = {inside: true};\n```\n\n",
                "```text\n~~~\n```\n<AfterFence /> {after}\n\n",
                "```text\n```\u{00a0}\n```\n<AfterNbsp /> {nbsp}\n\n",
                "~~~\r~~~\r<AfterCr /> {cr}\n\n",
                "\\`literal <AfterEscape /> {escape}\\`\n\n",
                "`code \\` <AfterCode /> {code}`\n\n",
                "<u>paired</u>\n<u>orphan\n</u>orphan-close\n\n",
                "<u><u>nested-orphan</u>\n\n",
                "`unclosed <AfterTick /> {tick}"
            )
            .into(),
            article_title: Some("<script>{title}</script>".into()),
            created_at: "Wed Oct 10 20:19:24 +0000 2018".into(),
            url: "https://x.com/alice/status/123".into(),
            media_urls: Vec::new(),
            local_media: Vec::new(),
        };

        let output = render_mdx(&detail).expect("MDX should render");
        assert!(output.contains("# 📰 &lt;script&gt;&#123;title&#125;&lt;/script&gt;"));
        assert!(output.contains("&#101;xport const value = &#123;danger&#125;"));
        assert!(output.contains("&#105;mport/**/x from \"pkg\""));
        assert!(output.contains("&#101;xport/**/const y = 1"));
        assert!(output.contains("&lt;Widget /&gt; &#123;name&#125;"));
        assert!(output.contains("`<Code /> {literal}`"));
        assert!(output.contains("const safe = {inside: true};"));
        assert!(output.contains("&lt;AfterFence /&gt; &#123;after&#125;"));
        assert!(output.contains("&lt;AfterNbsp /&gt; &#123;nbsp&#125;"));
        assert!(output.contains("&lt;AfterCr /&gt; &#123;cr&#125;"));
        assert!(output.contains("\\`literal &lt;AfterEscape /&gt; &#123;escape&#125;\\`"));
        assert!(output.contains("`code \\` &lt;AfterCode /&gt; &#123;code&#125;`"));
        assert!(output.contains("<u>paired</u>\n&lt;u&gt;orphan"));
        assert!(output.contains("&lt;/u&gt;orphan-close"));
        assert!(output.contains("&lt;u&gt;&lt;u&gt;nested-orphan&lt;/u&gt;"));
        assert!(output.contains("`unclosed &lt;AfterTick /&gt; &#123;tick&#125;"));
    }
}
