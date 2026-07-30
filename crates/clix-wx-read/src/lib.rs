use std::{
    fmt::Write as _,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::LazyLock,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{Args, ValueEnum};
use clix_core::{
    fs::{atomic_write, parent_or_current},
    ui,
};
use regex::Regex;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};

const MAX_CONCURRENT_MEDIA_REQUESTS: usize = 4;

static TITLE_SELECTORS: LazyLock<[(Selector, bool); 3]> = LazyLock::new(|| {
    [
        (
            Selector::parse("#activity-name").expect("valid title selector"),
            false,
        ),
        (
            Selector::parse("meta[property=\"og:title\"]").expect("valid title meta selector"),
            true,
        ),
        (
            Selector::parse("title").expect("valid title selector"),
            false,
        ),
    ]
});
static AUTHOR_SELECTORS: LazyLock<[(Selector, bool); 4]> = LazyLock::new(|| {
    [
        (
            Selector::parse("#js_name").expect("valid author selector"),
            false,
        ),
        (
            Selector::parse("meta[property=\"og:article:author\"]")
                .expect("valid author meta selector"),
            true,
        ),
        (
            Selector::parse("meta[name=\"author\"]").expect("valid author meta selector"),
            true,
        ),
        (
            Selector::parse(".profile_meta_value").expect("valid author selector"),
            false,
        ),
    ]
});
static PUBLISH_TIME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:var\s+createTime\s*=\s*"(\d+)"|var\s+ct\s*=\s*"(\d+)")"#)
        .expect("valid publish-time regex")
});
static PUBLISH_TIME_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("#publish_time").expect("valid publish-time selector"));
static CANONICAL_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"var\s+msg_link\s*=\s*"(https?://[^"]+)""#).expect("valid canonical URL regex")
});
static CONTENT_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("#js_content").expect("valid article-content selector"));
static IMAGE_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("img").expect("valid image selector"));
static DATA_SRC_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<img\s+[^>]*?data-src="([^"]+)"[^>]*>"#).expect("valid image data-src regex")
});
static SRC_ATTRIBUTE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\ssrc\s*=\s*"([^"]*)""#).expect("valid image src-attribute regex")
});
static HEADING_CANDIDATE_SELECTOR: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse("p, h1, h2, h3, h4, h5, h6").expect("valid heading-candidate selector")
});
static CODE_BLOCK_SELECTOR: LazyLock<Selector> = LazyLock::new(|| {
    Selector::parse("pre, ul.code-snippet__list, section.code-snippet")
        .expect("valid code-block selector")
});
static CODE_LINE_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("code, li").expect("valid code-line selector"));
static HEADING_CHILD_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse("span, strong, b").expect("valid heading-child selector"));
static HEADING_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:[一二三四五六七八九十0-9]+[、.．]|（[一二三四五六七八九十0-9]+）|\([一二三四五六七八九十0-9]+\)|第[一二三四五六七八九十0-9]+[章部分节篇]|引言|结语|总结|前言|目录)",
    )
    .expect("valid heading regex")
});
static SUB_HEADING_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:\d+\.\d+|（[一二三四五六七八九十0-9]+）|\([一二三四五六七八九十0-9]+\))")
        .expect("valid sub-heading regex")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
pub enum ReadOutputFormat {
    Markdown,
    Mdx,
    Json,
}

#[derive(Debug, Args)]
pub struct ReadArgs {
    /// `WeChat` article URL or article ID (for example, <https://mp.weixin.qq.com/s/abcdef123456>)
    pub url_or_id: String,

    /// Output path (default: `<author>:<title>.md`)
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Output format: markdown, mdx, or json
    #[arg(short, long, value_enum, default_value_t = ReadOutputFormat::Markdown)]
    pub format: ReadOutputFormat,

    /// Skip downloading media images locally into `media/` folder
    #[arg(long)]
    pub no_media: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WxArticleDetail {
    pub id: String,
    pub title: String,
    pub author: String,
    pub publish_time: Option<String>,
    pub url: String,
    pub markdown_body: String,
    pub media_urls: Vec<String>,
    pub local_media: Vec<String>,
}

struct MediaJob {
    media_index: usize,
    url: String,
    destination: PathBuf,
    relative_path: String,
}

/// Download one `WeChat` article and render it in the requested local format.
///
/// # Errors
///
/// Returns an error for network failures, DOM parsing failures,
/// failed media downloads, or file write errors.
pub async fn run(args: ReadArgs) -> Result<()> {
    let article_url = normalize_article_url(&args.url_or_id);
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36")
        .build()?;

    let spinner = ui::create_spinner(&format!("fetching WeChat article {article_url}..."));
    let mut detail = fetch_article_detail(&client, &article_url).await?;
    spinner.finish_and_clear();

    let extension = match args.format {
        ReadOutputFormat::Markdown => "md",
        ReadOutputFormat::Mdx => "mdx",
        ReadOutputFormat::Json => "json",
    };
    let default_file_name = default_output_file_name(&detail.author, &detail.title, extension);
    let output_path = resolve_output_path(args.output, default_file_name)?;

    if !args.no_media && !detail.media_urls.is_empty() {
        download_article_media(&client, &mut detail, &output_path).await?;
    }

    write_article_file(&detail, &output_path, args.format)?;

    ui::success(format!(
        "saved WeChat article {} by {} to {}",
        ui::style_bold(&detail.title),
        ui::style_bold(&detail.author),
        ui::style_bold(&output_path.display().to_string())
    ));

    Ok(())
}

#[must_use]
pub fn normalize_article_url(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        trimmed.to_string()
    } else if trimmed.starts_with("mp.weixin.qq.com/") {
        format!("https://{trimmed}")
    } else if trimmed.starts_with("s/") {
        format!("https://mp.weixin.qq.com/{trimmed}")
    } else {
        format!("https://mp.weixin.qq.com/s/{trimmed}")
    }
}

#[must_use]
pub fn extract_article_id(url: &str) -> String {
    if let Some(pos) = url.find("/s/") {
        let after = &url[pos + 3..];
        let end = after.find(['?', '#', '/']).unwrap_or(after.len());
        let candidate = &after[..end];
        if !candidate.is_empty() {
            return candidate.to_string();
        }
    }
    if let Some(pos) = url.find("sn=") {
        let after = &url[pos + 3..];
        let end = after.find(['&', '#', '?']).unwrap_or(after.len());
        let candidate = &after[..end];
        if !candidate.is_empty() {
            return candidate.to_string();
        }
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    format!("wx_{:x}", hasher.finish())
}

/// Fetch and parse a `WeChat` article from its URL.
///
/// # Errors
///
/// Returns an error if the HTTP request fails, the response status is not successful,
/// or the body cannot be read.
pub async fn fetch_article_detail(client: &reqwest::Client, url: &str) -> Result<WxArticleDetail> {
    let response_text = client
        .get(url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
        )
        .send()
        .await
        .with_context(|| format!("failed to request WeChat article at {url}"))?
        .error_for_status()
        .with_context(|| format!("received error response from WeChat article at {url}"))?
        .text()
        .await
        .with_context(|| format!("failed to read response body from {url}"))?;

    let article_id = extract_article_id(url);
    let document = Html::parse_document(&response_text);

    let title = extract_title(&document);
    let author = extract_author(&document);
    let publish_time = extract_publish_time(&response_text);
    let canonical_url = extract_canonical_url(&response_text).unwrap_or_else(|| url.to_string());

    let (body_html, media_urls) = extract_content_and_media(&document);
    let preprocessed_html = preprocess_html_for_markdown(&body_html);
    let raw_markdown = html2md::parse_html(&preprocessed_html);
    let markdown_body = postprocess_markdown(&raw_markdown);

    Ok(WxArticleDetail {
        id: article_id,
        title,
        author,
        publish_time,
        url: canonical_url,
        markdown_body,
        media_urls,
        local_media: Vec::new(),
    })
}

fn extract_title(document: &Html) -> String {
    for (selector, is_meta) in TITLE_SELECTORS.iter() {
        if let Some(element) = document.select(selector).next() {
            let text = if *is_meta {
                element.value().attr("content").unwrap_or("").to_string()
            } else {
                element.text().collect::<Vec<_>>().join(" ")
            };
            let clean = text.trim();
            if !clean.is_empty() {
                return clean.to_string();
            }
        }
    }
    "Untitled Article".to_string()
}

fn extract_author(document: &Html) -> String {
    for (selector, is_meta) in AUTHOR_SELECTORS.iter() {
        if let Some(element) = document.select(selector).next() {
            let text = if *is_meta {
                element.value().attr("content").unwrap_or("").to_string()
            } else {
                element.text().collect::<Vec<_>>().join(" ")
            };
            let clean = text.trim();
            if !clean.is_empty() {
                return clean.to_string();
            }
        }
    }
    "wx".to_string()
}

fn extract_publish_time(raw_html: &str) -> Option<String> {
    if let Some(caps) = PUBLISH_TIME_RE.captures(raw_html) {
        let ts_str = caps.get(1).or_else(|| caps.get(2)).map(|m| m.as_str())?;
        if let Ok(ts) = ts_str.parse::<i64>()
            && let Some(dt) = DateTime::<Utc>::from_timestamp(ts, 0)
        {
            return Some(dt.format("%Y-%m-%d %H:%M:%S").to_string());
        }
    }
    let document = Html::parse_document(raw_html);
    if let Some(element) = document.select(&PUBLISH_TIME_SELECTOR).next() {
        let text = element.text().collect::<Vec<_>>().join(" ");
        let clean = text.trim();
        if !clean.is_empty() {
            return Some(clean.to_string());
        }
    }
    None
}

fn extract_canonical_url(raw_html: &str) -> Option<String> {
    let caps = CANONICAL_URL_RE.captures(raw_html)?;
    Some(caps.get(1)?.as_str().replace("\\x26", "&"))
}

fn extract_content_and_media(document: &Html) -> (String, Vec<String>) {
    let Some(container) = document.select(&CONTENT_SELECTOR).next() else {
        return (String::new(), Vec::new());
    };

    let mut media_urls = Vec::new();

    for img in container.select(&IMAGE_SELECTOR) {
        let val = img.value();
        let src = val.attr("data-src").or_else(|| val.attr("src"));
        if let Some(url) = src {
            let trimmed = url.trim();
            if !trimmed.is_empty()
                && !trimmed.starts_with("data:image/")
                && !media_urls.contains(&trimmed.to_string())
            {
                media_urls.push(trimmed.to_string());
            }
        }
    }

    (container.html(), media_urls)
}

#[must_use]
fn preprocess_html_for_markdown(html: &str) -> String {
    // 1. Convert data-src="..." to src="..." if src is missing or empty
    let html_with_src = DATA_SRC_RE
        .replace_all(html, |caps: &regex::Captures| {
            let img_tag = &caps[0];
            let data_src = &caps[1];
            if SRC_ATTRIBUTE_RE
                .captures(img_tag)
                .is_none_or(|src| src[1].trim().is_empty())
            {
                format!(r#"<img src="{data_src}">"#)
            } else {
                img_tag.to_string()
            }
        })
        .to_string();
    // 2. Convert WeChat <section> wrappers to <div> block wrappers so html2md treats child <p> tags as block elements
    let html_with_divs = html_with_src
        .replace("<section", "<div")
        .replace("</section>", "</div>");

    // 3. Clean and preserve code blocks (<pre>, <ul class="code-snippet__list">, <section class="code-snippet">)
    let html_with_code = preprocess_code_blocks(&html_with_divs);

    // 3. Wrap WeChat pseudo-headings (<p>/<section>/<div> with bold/large text or section numbering) into <h2> or <h3>
    let document = Html::parse_fragment(&html_with_code);
    let mut replacements = Vec::new();
    let mut in_toc_block = false;

    for element in document.select(&HEADING_CANDIDATE_SELECTOR) {
        let text = element.text().collect::<Vec<_>>().join(" ");
        let clean = text.trim();
        if clean.is_empty() || clean.contains('\n') {
            continue;
        }

        if clean == "目录" || clean == "## 目录" || clean == "【目录】" || clean == "目录："
        {
            in_toc_block = true;
            continue;
        }

        if in_toc_block {
            if has_heading_style(&element) || clean.len() > 100 {
                in_toc_block = false;
            } else {
                let outer_html = element.html();
                let indent = if is_sub_heading_pattern(clean) {
                    "  - "
                } else {
                    "- "
                };
                replacements.push((outer_html, format!("<p>{indent}{clean}</p>")));
                continue;
            }
        }

        let is_sec_num = is_heading_pattern(clean);
        let is_styled = has_heading_style(&element);

        if is_sec_num || is_styled {
            let outer_html = element.html();
            let tag = if is_sub_heading_pattern(clean) {
                "h3"
            } else {
                "h2"
            };
            replacements.push((outer_html, format!("<{tag}>{clean}</{tag}>")));
        }
    }

    let mut result = html_with_code;
    for (old_html, new_html) in replacements {
        result = result.replace(&old_html, &new_html);
    }

    result
}

#[must_use]
fn preprocess_code_blocks(html: &str) -> String {
    let document = Html::parse_fragment(html);
    let mut replacements: Vec<(String, String)> = Vec::new();

    for element in document.select(&CODE_BLOCK_SELECTOR) {
        let outer_html = element.html();

        if replacements
            .iter()
            .any(|(old, _)| old.contains(&outer_html))
        {
            continue;
        }

        let val = element.value();
        let lang = val
            .attr("data-lang")
            .or_else(|| val.attr("lang"))
            .unwrap_or("")
            .trim();

        let mut lines = Vec::new();
        for code_elem in element.select(&CODE_LINE_SELECTOR) {
            let line_text = code_elem.text().collect::<Vec<_>>().join("");
            let clean_line = line_text
                .replace("&nbsp;", " ")
                .replace('\u{a0}', " ")
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&");
            lines.push(clean_line);
        }

        if lines.is_empty() {
            let text = element.text().collect::<Vec<_>>().join("");
            let clean_text = text.replace("&nbsp;", " ").replace('\u{a0}', " ");
            lines = clean_text.lines().map(ToString::to_string).collect();
        }

        let clean_code = lines.join("\n");
        let lang_class = if lang.is_empty() {
            String::new()
        } else {
            format!(" class=\"language-{lang}\"")
        };
        let replacement = format!("<pre><code{lang_class}>\n{clean_code}\n</code></pre>");
        replacements.push((outer_html, replacement));
    }

    let mut result = html.to_string();
    for (old_html, new_html) in replacements {
        result = result.replace(&old_html, &new_html);
    }

    result
}

#[must_use]
fn has_heading_style(element: &scraper::ElementRef) -> bool {
    let val = element.value();
    let style = val.attr("style").unwrap_or("");
    let is_bold = style.contains("font-weight: bold")
        || style.contains("font-weight: 700")
        || style.contains("font-weight: 600");
    let is_large = style.contains("font-size: 18px")
        || style.contains("font-size: 19px")
        || style.contains("font-size: 20px")
        || style.contains("font-size: 21px")
        || style.contains("font-size: 22px")
        || style.contains("font-size: 24px");

    if is_bold && is_large {
        return true;
    }

    for child in element.select(&HEADING_CHILD_SELECTOR) {
        let child_style = child.value().attr("style").unwrap_or("");
        let c_name = child.value().name();
        let c_bold = child_style.contains("font-weight: bold")
            || child_style.contains("font-weight: 700")
            || c_name == "strong"
            || c_name == "b";
        let c_large = child_style.contains("font-size: 18px")
            || child_style.contains("font-size: 19px")
            || child_style.contains("font-size: 20px")
            || child_style.contains("font-size: 21px")
            || child_style.contains("font-size: 22px")
            || child_style.contains("font-size: 24px");
        if c_bold && c_large {
            return true;
        }
    }

    false
}

#[must_use]
fn is_heading_pattern(text: &str) -> bool {
    HEADING_RE.is_match(text)
}

#[must_use]
fn is_sub_heading_pattern(text: &str) -> bool {
    SUB_HEADING_RE.is_match(text)
}

#[must_use]
#[allow(clippy::too_many_lines)]
fn postprocess_markdown(raw_md: &str) -> String {
    let lines: Vec<&str> = raw_md.lines().collect();
    let mut result_lines: Vec<String> = Vec::with_capacity(lines.len());
    let mut idx = 0;
    let mut in_toc = false;

    while idx < lines.len() {
        let line = lines[idx];
        let trimmed = line.trim();

        // Filter out empty list items (* or -) right before a code block
        if (trimmed == "*" || trimmed == "-") && idx + 1 < lines.len() {
            let mut next_idx = idx + 1;
            while next_idx < lines.len()
                && (lines[next_idx].trim() == "*"
                    || lines[next_idx].trim() == "-"
                    || lines[next_idx].trim().is_empty())
            {
                next_idx += 1;
            }
            if next_idx < lines.len() && lines[next_idx].trim().starts_with("```") {
                idx = next_idx;
                continue;
            }
        }
        // 1. Check for "目录" heading (ATX or Setext style or bold **目录**)
        let mut is_toc_header = false;
        let clean_toc_check = trimmed.trim_matches('*').trim();
        if clean_toc_check == "目录" || clean_toc_check == "## 目录" || clean_toc_check == "# 目录"
        {
            is_toc_header = true;
        } else if idx + 1 < lines.len() {
            let next_line = lines[idx + 1].trim();
            if (clean_toc_check == "目录" || clean_toc_check == "目录：")
                && !next_line.is_empty()
                && (next_line.chars().all(|c| c == '-') || next_line.chars().all(|c| c == '='))
            {
                is_toc_header = true;
                idx += 1;
            }
        }

        if is_toc_header {
            result_lines.push("## 目录".to_string());
            in_toc = true;
            idx += 1;
            continue;
        }

        // 2. Handle TOC formatting
        if in_toc {
            if (trimmed.starts_with('#') && !trimmed.starts_with("## 目录"))
                || trimmed.starts_with("**一**")
                || trimmed.starts_with("**二**")
                || trimmed.starts_with("**三**")
            {
                in_toc = false;
            } else {
                let unescaped = trimmed.replace(r"\-", "-");
                let clean_item = unescaped
                    .trim_start_matches('-')
                    .trim_start_matches('*')
                    .trim();
                if !clean_item.is_empty() {
                    let indent =
                        if is_sub_heading_pattern(clean_item) || unescaped.starts_with("  ") {
                            "  - "
                        } else {
                            "- "
                        };
                    result_lines.push(format!("{indent}{clean_item}"));
                    idx += 1;
                    continue;
                }
            }
        }

        // 3. Convert Setext-style headings (Heading\n--- or Heading\n===) to ATX-style (## Heading)
        if idx + 1 < lines.len() {
            let next_line = lines[idx + 1].trim();
            if !trimmed.is_empty()
                && !next_line.is_empty()
                && next_line.chars().all(|c| c == '=')
                && next_line.len() >= 3
            {
                result_lines.push(format!("# {trimmed}"));
                idx += 2;
                continue;
            }
            if !trimmed.is_empty()
                && !next_line.is_empty()
                && next_line.chars().all(|c| c == '-')
                && next_line.len() >= 3
            {
                let prefix = if is_sub_heading_pattern(trimmed) {
                    "###"
                } else {
                    "##"
                };
                result_lines.push(format!("{prefix} {trimmed}"));
                idx += 2;
                continue;
            }
        }

        // 3. Handle split Chinese numeral headings: **一** on current line, **Title** on next line
        if (trimmed == "**一**"
            || trimmed == "**二**"
            || trimmed == "**三**"
            || trimmed == "**四**"
            || trimmed == "**五**"
            || trimmed == "**六**"
            || trimmed == "**七**"
            || trimmed == "**八**"
            || trimmed == "**九**"
            || trimmed == "**十**"
            || trimmed == "一"
            || trimmed == "二"
            || trimmed == "三"
            || trimmed == "四"
            || trimmed == "五")
            && idx + 1 < lines.len()
        {
            let num = trimmed.trim_matches('*');
            let next_raw = lines[idx + 1].trim();
            if !next_raw.is_empty() && next_raw.len() <= 80 {
                let clean_title = next_raw.trim_matches('*').trim();
                result_lines.push(format!("## {num}、{clean_title}"));
                idx += 2;
                continue;
            }
        }

        // 4. Handle standalone bold text lines in body (e.g. **当前使用情况**) as H3
        if trimmed.starts_with("**")
            && trimmed.ends_with("**")
            && trimmed.len() <= 60
            && !trimmed.contains('\n')
        {
            let inner_text = trimmed.trim_matches('*').trim();
            if !inner_text.is_empty() && !is_heading_pattern(inner_text) {
                result_lines.push(format!("### {inner_text}"));
                idx += 1;
                continue;
            }
        }

        // 5. Add ATX heading prefix (## / ###) to standalone section titles
        if !trimmed.starts_with('#')
            && !trimmed.starts_with('-')
            && !trimmed.starts_with('*')
            && !trimmed.starts_with('>')
            && !trimmed.starts_with("```")
            && !trimmed.is_empty()
            && trimmed.len() <= 100
            && is_heading_pattern(trimmed)
        {
            let prefix = if is_sub_heading_pattern(trimmed) {
                "###"
            } else {
                "##"
            };
            result_lines.push(format!("{prefix} {trimmed}"));
            idx += 1;
            continue;
        }

        result_lines.push(line.to_string());
        idx += 1;
    }

    result_lines.join("\n")
}

fn media_extension(url: &str) -> &'static str {
    if url.contains("wx_fmt=png") {
        "png"
    } else if url.contains("wx_fmt=jpeg") || url.contains("wx_fmt=jpg") {
        "jpg"
    } else if url.contains("wx_fmt=gif") {
        "gif"
    } else if url.contains("wx_fmt=webp") {
        "webp"
    } else if url.contains("wx_fmt=svg") {
        "svg"
    } else {
        "png"
    }
}

async fn download_article_media(
    client: &reqwest::Client,
    detail: &mut WxArticleDetail,
    output_path: &Path,
) -> Result<()> {
    let base_dir = parent_or_current(output_path);
    let media_dir = base_dir.join("media");
    fs::create_dir_all(&media_dir).context("failed to create media directory")?;

    let spinner = ui::create_spinner("downloading article images...");
    let mut downloaded = 0;
    let mut skipped = 0;
    let mut failed = 0;
    let mut available_media = Vec::new();
    let mut jobs = Vec::new();

    let author_slug = sanitize_filename(&detail.author);
    let id_slug = sanitize_filename(&detail.id);

    for (index, media_url) in detail.media_urls.iter().enumerate() {
        let extension = media_extension(media_url);
        let file_name = format!("{author_slug}_{id_slug}_{}.{extension}", index + 1);
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
            "downloading article images ({completed}/{job_count})"
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
        detail.markdown_body = detail.markdown_body.replace(&media_url, &relative_path);
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
            let bytes = client
                .get(&job.url)
                .header("Referer", "https://mp.weixin.qq.com/")
                .header(
                    "User-Agent",
                    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
                )
                .send()
                .await
                .with_context(|| format!("failed to request media {}", job.url))?
                .error_for_status()
                .with_context(|| format!("received error response for media {}", job.url))?
                .bytes()
                .await
                .with_context(|| format!("failed to download bytes for media {}", job.url))?;

            atomic_write(&job.destination, &bytes)
                .with_context(|| format!("failed to write media {}", job.destination.display()))?;
            Ok(())
        }
        .await;

        (job, result)
    });
}

fn write_article_file(
    detail: &WxArticleDetail,
    path: &Path,
    format: ReadOutputFormat,
) -> Result<()> {
    let content = match format {
        ReadOutputFormat::Markdown => render_markdown(detail)?,
        ReadOutputFormat::Mdx => render_mdx(detail)?,
        ReadOutputFormat::Json => serde_json::to_string_pretty(detail)
            .context("failed to serialize WeChat article to JSON")?,
    };

    atomic_write(path, content.as_bytes())
        .with_context(|| format!("failed to write WeChat article output {}", path.display()))
}

fn render_markdown(detail: &WxArticleDetail) -> Result<String> {
    render_markup(detail, false)
}

fn render_mdx(detail: &WxArticleDetail) -> Result<String> {
    render_markup(detail, true)
}

fn render_markup(detail: &WxArticleDetail, mdx_safe: bool) -> Result<String> {
    let mut content = String::new();
    let author = format!("{} (@wx)", detail.author);

    content.push_str("---\n");
    write_frontmatter_value(&mut content, "title", &detail.title)?;
    write_frontmatter_value(&mut content, "author", &author)?;
    write_frontmatter_value(&mut content, "url", &detail.url)?;
    if let Some(date) = &detail.publish_time {
        write_frontmatter_value(&mut content, "date", date)?;
    }
    content.push_str("---\n\n");

    writeln!(content, "# 📰 {}\n", escape_markdown_heading(&detail.title))
        .context("failed to render article title")?;

    let body = if mdx_safe {
        sanitize_mdx(&detail.markdown_body)
    } else {
        detail.markdown_body.clone()
    };

    content.push_str(&body);
    content.push_str("\n\n");

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
            _ => escaped.push(character),
        }
    }
    escaped
}

fn sanitize_mdx(value: &str) -> String {
    // Basic sanitization of JSX-like tags for MDX compatibility
    value.replace('<', "&lt;").replace('>', "&gt;")
}

fn write_frontmatter_value(content: &mut String, key: &str, value: &str) -> Result<()> {
    writeln!(content, "{key}: {value:?}").context("failed to write frontmatter field")
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

fn default_output_file_name(author: &str, title: &str, extension: &str) -> String {
    let author_clean = sanitize_filename(author);
    let title_clean = sanitize_filename(title);
    if author_clean == "wx" || author_clean.is_empty() {
        format!("{title_clean}.{extension}")
    } else {
        format!("{author_clean}:{title_clean}.{extension}")
    }
}

#[must_use]
pub fn sanitize_filename(name: &str) -> String {
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

    let collapsed = clean
        .split('_')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("_");

    if collapsed.is_empty() {
        "article".to_string()
    } else {
        collapsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_article_url() {
        assert_eq!(
            normalize_article_url("https://mp.weixin.qq.com/s/123456"),
            "https://mp.weixin.qq.com/s/123456"
        );
        assert_eq!(
            normalize_article_url("mp.weixin.qq.com/s/123456"),
            "https://mp.weixin.qq.com/s/123456"
        );
        assert_eq!(
            normalize_article_url("s/123456"),
            "https://mp.weixin.qq.com/s/123456"
        );
        assert_eq!(
            normalize_article_url("123456"),
            "https://mp.weixin.qq.com/s/123456"
        );
    }

    #[test]
    fn test_extract_article_id() {
        assert_eq!(
            extract_article_id("https://mp.weixin.qq.com/s/abcdef123?chksm=123#rd"),
            "abcdef123"
        );
        assert_eq!(
            extract_article_id("https://mp.weixin.qq.com/s?__biz=123&sn=abcdef#rd"),
            "abcdef"
        );
    }

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("  hello / 世界  "), "hello_世界");
        assert_eq!(sanitize_filename("***"), "article");
        assert_eq!(
            default_output_file_name("公众号", "微信文章标题测试", "md"),
            "公众号:微信文章标题测试.md"
        );
    }

    #[test]
    fn cached_metadata_parsers_preserve_wechat_fields() {
        let raw_html = r#"
            <html>
              <head><meta property="og:title" content="Cached title"></head>
              <body><div id="js_name">Cached author</div></body>
              <script>
                var ct = "1753873200";
                var msg_link = "https://mp.weixin.qq.com/s/example\x26scene=1";
              </script>
            </html>
        "#;
        let document = Html::parse_document(raw_html);

        assert_eq!(extract_title(&document), "Cached title");
        assert_eq!(extract_author(&document), "Cached author");
        assert_eq!(
            extract_publish_time(raw_html).as_deref(),
            Some("2025-07-30 11:00:00")
        );
        assert_eq!(
            extract_canonical_url(raw_html).as_deref(),
            Some("https://mp.weixin.qq.com/s/example&scene=1")
        );
    }

    #[test]
    fn lazy_image_data_src_is_promoted_without_overwriting_a_real_src() {
        let promoted =
            preprocess_html_for_markdown(r#"<p><img data-src="https://example.com/lazy.png"></p>"#);
        assert!(promoted.contains(r#"<img src="https://example.com/lazy.png">"#));

        let preserved = preprocess_html_for_markdown(
            r#"<p><img src="https://example.com/eager.png" data-src="https://example.com/lazy.png"></p>"#,
        );
        assert!(preserved.contains(r#"src="https://example.com/eager.png""#));
    }

    #[test]
    fn cached_heading_patterns_keep_existing_classification() {
        assert!(is_heading_pattern("一、背景"));
        assert!(is_heading_pattern("第2部分"));
        assert!(is_sub_heading_pattern("2.1 方法"));
        assert!(!is_heading_pattern("普通正文"));
    }
}
