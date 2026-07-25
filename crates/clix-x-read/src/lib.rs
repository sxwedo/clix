use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use clix_core::ui;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const TWITTER_BEARER_TOKEN: &str = "Bearer AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs%3D1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA";

const TWEET_DETAIL_QUERY_IDS: &[&str] = &[
    "Lq1caG5YPcdhpTdS2ZRx7Q",
    "_NvJCnIjOW__EP5-RF197A",
    "97JF30KziU00483E_8elBA",
    "aFvUsJm2c-oDkJV75blV6g",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReadOutputFormat {
    Markdown,
    Mdx,
    Json,
}

#[derive(Debug, Args)]
pub struct ReadArgs {
    /// X (Twitter) status URL or Tweet ID (e.g., https://x.com/user/status/123456789)
    pub url_or_id: String,

    /// Twitter auth_token cookie (or set X_AUTH_TOKEN / TWITTER_AUTH_TOKEN env)
    #[arg(long)]
    pub auth_token: Option<String>,

    /// Twitter ct0 (CSRF) cookie (or set X_CT0 / TWITTER_CT0 env)
    #[arg(long)]
    pub ct0: Option<String>,

    /// Output file path (default: <author_handle>_<tweet_id>.md)
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
pub struct TweetDetail {
    pub id: String,
    pub tweet_type: String,
    pub author_name: String,
    pub author_handle: String,
    pub text: String,
    pub article_title: Option<String>,
    pub created_at: String,
    pub url: String,
    pub media_urls: Vec<String>,
    pub local_media: Vec<String>,
}

pub async fn run(args: ReadArgs) -> Result<()> {
    let tweet_id = extract_tweet_id(&args.url_or_id)?;

    let auth_token = resolve_auth_token(args.auth_token);
    let ct0 = resolve_ct0(args.ct0);

    let (auth_token, ct0) = match (auth_token, ct0) {
        (Some(a), Some(c)) => (a, c),
        _ => bail!(
            "Missing X authentication credentials!\n\n\
             Please provide both credentials via CLI flags or env vars:\n  \
             clix x read <URL> --auth-token \"<auth_token>\" --ct0 \"<ct0>\"\n\n\
             Or set environment variables:\n  \
             export X_AUTH_TOKEN=\"...\"\n  \
             export X_CT0=\"...\""
        ),
    };

    let client = build_http_client(&auth_token, &ct0)?;
    let spinner = ui::create_spinner(&format!("fetching X status {tweet_id}..."));

    let mut detail = fetch_tweet_detail(&client, &tweet_id).await?;
    spinner.finish_and_clear();

    let ext = match args.format {
        ReadOutputFormat::Markdown => "md",
        ReadOutputFormat::Mdx => "mdx",
        ReadOutputFormat::Json => "json",
    };
    let title_src = detail
        .article_title
        .as_deref()
        .unwrap_or_else(|| detail.text.lines().next().unwrap_or(&detail.id));
    let safe_title = sanitize_filename(title_src);
    let default_file_name = format!("{}_{}.{}", detail.author_handle, safe_title, ext);

    let output_path = match args.output {
        Some(out) => {
            let out_str = out.to_string_lossy();
            if out.is_dir() || out_str.ends_with('/') || out_str.ends_with('\\') {
                fs::create_dir_all(&out).ok();
                out.join(default_file_name)
            } else {
                out
            }
        }
        None => PathBuf::from(default_file_name),
    };

    if !args.no_media && !detail.media_urls.is_empty() {
        download_tweet_media(&client, &mut detail, &output_path).await?;
    }

    write_tweet_file(&detail, &output_path, args.format)?;

    ui::success(format!(
        "saved X {} by @{} to {}",
        ui::style_bold(&detail.tweet_type),
        ui::style_bold(&detail.author_handle),
        ui::style_bold(&output_path.display().to_string())
    ));

    Ok(())
}

fn extract_tweet_id(input: &str) -> Result<String> {
    let input = input.trim();
    if input.chars().all(|c| c.is_ascii_digit()) {
        return Ok(input.to_string());
    }

    if let Ok(url) = reqwest::Url::parse(input)
        && let Some(segments) = url.path_segments()
    {
        let parts: Vec<&str> = segments.collect();
        if let Some(pos) = parts.iter().position(|&p| p == "status")
            && let Some(id) = parts.get(pos + 1)
        {
            let clean_id = id.split('?').next().unwrap_or(id);
            if clean_id.chars().all(|c| c.is_ascii_digit()) {
                return Ok(clean_id.to_string());
            }
        }
    }

    bail!("invalid X status URL or Tweet ID: `{input}`")
}

fn resolve_auth_token(cli_val: Option<String>) -> Option<String> {
    cli_val
        .filter(|s| !s.trim().is_empty())
        .or_else(|| env::var("X_AUTH_TOKEN").ok())
        .or_else(|| env::var("TWITTER_AUTH_TOKEN").ok())
        .or_else(|| env::var("AUTH_TOKEN").ok())
        .map(|s| s.trim().to_string())
}

fn resolve_ct0(cli_val: Option<String>) -> Option<String> {
    cli_val
        .filter(|s| !s.trim().is_empty())
        .or_else(|| env::var("X_CT0").ok())
        .or_else(|| env::var("TWITTER_CT0").ok())
        .or_else(|| env::var("CT0").ok())
        .map(|s| s.trim().to_string())
}

fn build_http_client(auth_token: &str, ct0: &str) -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_str(TWITTER_BEARER_TOKEN).context("invalid bearer token")?,
    );
    headers.insert(
        "cookie",
        HeaderValue::from_str(&format!("auth_token={auth_token}; ct0={ct0}"))
            .context("invalid cookie header")?,
    );
    headers.insert(
        "x-csrf-token",
        HeaderValue::from_str(ct0).context("invalid csrf token header")?,
    );
    headers.insert("x-twitter-active-user", HeaderValue::from_static("yes"));
    headers.insert("x-twitter-client-language", HeaderValue::from_static("en"));
    headers.insert(
        "user-agent",
        HeaderValue::from_static(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
        ),
    );

    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .context("failed to build HTTP client for X")
}

async fn fetch_tweet_detail(client: &reqwest::Client, tweet_id: &str) -> Result<TweetDetail> {
    let variables_json = serde_json::json!({
        "focalTweetId": tweet_id,
        "with_rux_injections": false,
        "rankingMode": "Relevance",
        "includePromotedContent": true,
        "withCommunity": true,
        "withQuickPromoteEligibilityTweetFields": true,
        "withBirdwatchNotes": true,
        "withVoice": true
    })
    .to_string();

    let features_json = serde_json::json!({
        "rweb_video_screen_enabled": true,
        "profile_label_improvements_pcf_label_in_post_enabled": true,
        "responsive_web_profile_redirect_enabled": true,
        "rweb_tipjar_consumption_enabled": true,
        "verified_phone_label_enabled": false,
        "creator_subscriptions_tweet_preview_api_enabled": true,
        "responsive_web_graphql_timeline_navigation_enabled": true,
        "responsive_web_graphql_exclude_directive_enabled": true,
        "responsive_web_graphql_skip_user_profile_image_extensions_enabled": false,
        "premium_content_api_read_enabled": false,
        "communities_web_enable_tweet_community_results_fetch": true,
        "c9s_tweet_anatomy_moderator_badge_enabled": true,
        "responsive_web_grok_analyze_button_fetch_trends_enabled": false,
        "responsive_web_grok_analyze_post_followups_enabled": false,
        "responsive_web_grok_annotations_enabled": false,
        "responsive_web_jetfuel_frame": true,
        "post_ctas_fetch_enabled": true,
        "responsive_web_grok_share_attachment_enabled": true,
        "articles_preview_enabled": true,
        "responsive_web_edit_tweet_api_enabled": true,
        "graphql_is_translatable_rweb_tweet_is_translatable_enabled": true,
        "view_counts_everywhere_api_enabled": true,
        "longform_notetweets_consumption_enabled": true,
        "responsive_web_twitter_article_tweet_consumption_enabled": true,
        "tweet_awards_web_tipping_enabled": false,
        "responsive_web_grok_show_grok_translated_post": false,
        "responsive_web_grok_analysis_button_from_backend": true,
        "creator_subscriptions_quote_tweet_preview_enabled": false,
        "freedom_of_speech_not_reach_fetch_enabled": true,
        "standardized_nudges_misinfo": true,
        "tweet_with_visibility_results_prefer_gql_limited_actions_policy_enabled": true,
        "longform_notetweets_rich_text_read_enabled": true,
        "longform_notetweets_inline_media_enabled": true,
        "responsive_web_grok_image_annotation_enabled": true,
        "responsive_web_grok_imagine_annotation_enabled": true,
        "responsive_web_grok_community_note_auto_translation_is_enabled": false,
        "responsive_web_enhance_cards_enabled": false,
        "responsive_web_twitter_article_plain_text_enabled": true,
        "responsive_web_twitter_article_seed_tweet_detail_enabled": true,
        "responsive_web_twitter_article_seed_tweet_summary_enabled": true
    })
    .to_string();

    let field_toggles_json = serde_json::json!({
        "withPayments": false,
        "withAuxiliaryUserLabels": false,
        "withArticleRichContentState": true,
        "withArticlePlainText": true,
        "withGrokAnalyze": false,
        "withDisallowedReplyControls": false
    })
    .to_string();

    let mut last_err = String::from("unknown error");

    for query_id in TWEET_DETAIL_QUERY_IDS {
        let url = format!("https://x.com/i/api/graphql/{query_id}/TweetDetail");
        let resp = match client
            .get(&url)
            .query(&[
                ("variables", &variables_json),
                ("features", &features_json),
                ("fieldToggles", &field_toggles_json),
            ])
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_err.push_str(&format!("\n [{query_id} -> send error: {e}]"));
                continue;
            }
        };

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            last_err.push_str(&format!("\n [{query_id} -> HTTP {status}: {body_text}]"));
            continue;
        }

        let body: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                last_err.push_str(&format!("\n [{query_id} -> JSON parse error: {e}]"));
                continue;
            }
        };

        if let Some(detail) = parse_tweet_detail_response(&body, tweet_id) {
            return Ok(detail);
        }
    }

    bail!("Failed to fetch X status {tweet_id}: {last_err}")
}

fn parse_tweet_detail_response(val: &Value, tweet_id: &str) -> Option<TweetDetail> {
    // 1. Try direct tweetResult pointer
    if let Some(res) = val.pointer("/data/tweetResult/result")
        && let Some(detail) = extract_tweet_detail_from_result(res, tweet_id)
    {
        return Some(detail);
    }

    // 2. Try threaded_conversation instructions
    let instructions = val
        .pointer("/data/threaded_conversation_with_injections_v2/instructions")
        .or_else(|| val.pointer("/data/threaded_conversation_with_injections/instructions"))
        .and_then(|v| v.as_array());

    if let Some(instructions) = instructions {
        for inst in instructions {
            let entries = inst.get("entries").and_then(|v| v.as_array());
            if let Some(entries) = entries {
                for entry in entries {
                    let item_result = entry
                        .pointer("/content/itemContent/tweet_results/result")
                        .or_else(|| entry.pointer("/item/itemContent/tweet_results/result"));

                    if let Some(res) = item_result
                        && let Some(detail) = extract_tweet_detail_from_result(res, tweet_id)
                    {
                        return Some(detail);
                    }
                }
            }
        }
    }

    None
}

fn extract_tweet_detail_from_result(res: &Value, target_id: &str) -> Option<TweetDetail> {
    let target =
        if res.get("__typename").and_then(|v| v.as_str()) == Some("TweetWithVisibilityResults") {
            res.get("tweet")?
        } else {
            res
        };

    let res_id = target.get("rest_id")?.as_str()?;
    if res_id != target_id {
        return None;
    }

    let (author_name, author_handle) = extract_author(target);

    let article_title = target
        .pointer("/article/article_results/result/title")
        .or_else(|| target.pointer("/article/title"))
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned);

    let full_text = extract_full_text(target);

    let created_at = target
        .pointer("/legacy/created_at")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let media_urls = extract_media_urls(target);
    let tweet_type = if article_title.is_some() || target.pointer("/article").is_some() {
        "Article".to_string()
    } else if target.pointer("/note_tweet").is_some() {
        "NoteTweet".to_string()
    } else if !media_urls.is_empty() {
        "Media".to_string()
    } else {
        "Tweet".to_string()
    };

    let url = format!("https://x.com/{author_handle}/status/{target_id}");

    Some(TweetDetail {
        id: target_id.to_string(),
        tweet_type,
        author_name,
        author_handle,
        text: full_text,
        article_title,
        created_at,
        url,
        media_urls,
        local_media: Vec::new(),
    })
}

fn extract_full_text(target: &Value) -> String {
    // 1. Try rendering from Article content_state (Draft.js AST)
    if let Some(content_state) = target
        .pointer("/article/article_results/result/content_state")
        .or_else(|| target.pointer("/article/content_state"))
        && let Some(rich_text) = render_content_state(content_state)
        && !rich_text.trim().is_empty()
    {
        return rich_text;
    }

    // 2. Try Article plain_text / body text pointers
    let article_plain = target
        .pointer("/article/article_results/result/plain_text")
        .or_else(|| target.pointer("/article/plain_text"))
        .or_else(|| target.pointer("/article/article_results/result/body/text"))
        .or_else(|| target.pointer("/article/body/text"))
        .or_else(|| target.pointer("/article/article_results/result/content/text"))
        .or_else(|| target.pointer("/article/content/text"))
        .or_else(|| target.pointer("/article/preview_text"))
        .and_then(|v| v.as_str());

    if let Some(text) = article_plain
        && !text.trim().is_empty()
    {
        return text.to_string();
    }

    // 3. Try NoteTweet text pointer
    if let Some(note_text) = target
        .pointer("/note_tweet/note_tweet_results/result/text")
        .or_else(|| target.pointer("/note_tweet/text"))
        .and_then(|v| v.as_str())
        && !note_text.trim().is_empty()
    {
        return note_text.to_string();
    }

    // 4. Fallback to legacy full_text
    target
        .pointer("/legacy/full_text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn render_content_state(cs: &Value) -> Option<String> {
    let blocks = cs.get("blocks")?.as_array()?;
    let entity_map: HashMap<String, &Value> = cs
        .get("entityMap")
        .and_then(|v| v.as_object())
        .map(|obj| obj.iter().map(|(k, v)| (k.clone(), v)).collect())
        .unwrap_or_default();

    let mut lines: Vec<String> = Vec::new();

    for block in blocks {
        let block_type = block
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unstyled");
        let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("");

        let line = match block_type {
            "header-one" => format!("# {text}"),
            "header-two" => format!("## {text}"),
            "header-three" => format!("### {text}"),
            "header-four" => format!("#### {text}"),
            "code-block" => format!("```\n{text}\n```"),
            "unordered-list-item" => format!("- {text}"),
            "ordered-list-item" => format!("1. {text}"),
            "blockquote" => format!("> {text}"),
            "atomic" => {
                if let Some(atomic_str) = render_atomic_block(block, &entity_map) {
                    atomic_str
                } else {
                    continue;
                }
            }
            _ => text.to_string(),
        };

        if !line.trim().is_empty() {
            lines.push(line);
        }
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n\n"))
    }
}

fn render_atomic_block(block: &Value, entity_map: &HashMap<String, &Value>) -> Option<String> {
    let ranges = block.get("entityRanges")?.as_array()?;
    if ranges.is_empty() {
        return None;
    }

    let key = ranges.first()?.get("key")?.as_str()?.to_string();
    let entity = entity_map.get(&key)?;
    let entity_type = entity.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match entity_type {
        "IMAGE" => {
            let img_url = entity
                .pointer("/data/media_url_https")
                .or_else(|| entity.pointer("/data/url"))
                .or_else(|| entity.pointer("/data/src"))
                .or_else(|| entity.pointer("/data/image/url"))
                .and_then(|v| v.as_str());
            if let Some(url) = img_url {
                Some(format!("![Image]({url})"))
            } else {
                Some("[Image]".to_string())
            }
        }
        "MARKDOWN" => entity
            .pointer("/data/markdown")
            .and_then(|v| v.as_str())
            .map(ToString::to_string),
        "DIVIDER" => Some("---".to_string()),
        "TWEET" => entity
            .pointer("/data/tweetId")
            .and_then(|v| v.as_str())
            .map(|id| format!("[Embedded Tweet: https://x.com/i/status/{id}]")),
        "LINK" => entity
            .pointer("/data/url")
            .and_then(|v| v.as_str())
            .map(|url| format!("[{url}]({url})")),
        _ => None,
    }
}

fn extract_author(target: &Value) -> (String, String) {
    let user_res = target
        .pointer("/core/user_results/result")
        .or_else(|| target.pointer("/user_results/result"))
        .or_else(|| target.pointer("/core/user_result/result"))
        .or_else(|| target.pointer("/user_result/result"));

    if let Some(res) = user_res {
        let user_target = if res.get("__typename").and_then(|v| v.as_str())
            == Some("UserWithVisibilityResults")
        {
            res.get("user").unwrap_or(res)
        } else {
            res
        };

        let handle = user_target
            .pointer("/core/screen_name")
            .or_else(|| user_target.pointer("/legacy/screen_name"))
            .or_else(|| user_target.get("screen_name"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let name = user_target
            .pointer("/core/name")
            .or_else(|| user_target.pointer("/legacy/name"))
            .or_else(|| user_target.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or(&handle)
            .to_string();

        return (name, handle);
    }

    ("unknown".to_string(), "unknown".to_string())
}

fn extract_media_urls(target: &Value) -> Vec<String> {
    let mut urls = Vec::new();

    // 1. Article cover media
    if let Some(url) = target
        .pointer("/article/cover_media/media_url_https")
        .or_else(|| target.pointer("/article/article_results/result/cover_media/media_url_https"))
        .and_then(|v| v.as_str())
    {
        urls.push(url.to_string());
    }

    // 2. Draft.js content_state entityMap images
    let content_state = target
        .pointer("/article/article_results/result/content_state")
        .or_else(|| target.pointer("/article/content_state"));

    if let Some(cs) = content_state
        && let Some(entity_map) = cs.get("entityMap").and_then(|v| v.as_object())
    {
        for (_key, entity) in entity_map {
            if entity.get("type").and_then(|v| v.as_str()) == Some("IMAGE") {
                let img_url = entity
                    .pointer("/data/media_url_https")
                    .or_else(|| entity.pointer("/data/url"))
                    .or_else(|| entity.pointer("/data/src"))
                    .or_else(|| entity.pointer("/data/image/url"))
                    .and_then(|v| v.as_str());

                if let Some(url) = img_url
                    && !urls.contains(&url.to_string())
                {
                    urls.push(url.to_string());
                }
            }
        }
    }

    // 3. Legacy / extended_entities media
    let media_array = target
        .pointer("/legacy/extended_entities/media")
        .or_else(|| target.pointer("/legacy/entities/media"))
        .and_then(|v| v.as_array());

    if let Some(arr) = media_array {
        for m in arr {
            if let Some(url) = m.get("media_url_https").and_then(|v| v.as_str())
                && !urls.contains(&url.to_string())
            {
                urls.push(url.to_string());
            }
        }
    }

    urls
}

async fn download_tweet_media(
    client: &reqwest::Client,
    detail: &mut TweetDetail,
    output_path: &Path,
) -> Result<()> {
    let base_dir = output_path.parent().unwrap_or_else(|| Path::new("."));
    let media_dir = base_dir.join("media");
    fs::create_dir_all(&media_dir).context("failed to create media directory")?;

    let spinner = ui::create_spinner("downloading status images...");

    let mut downloaded = 0;
    for (idx, media_url) in detail.media_urls.iter().enumerate() {
        let ext = media_url
            .split('.')
            .next_back()
            .unwrap_or("jpg")
            .split('?')
            .next()
            .unwrap_or("jpg");
        let file_name = format!("{}_{}_{}.{}", detail.author_handle, detail.id, idx + 1, ext);
        let dest_path = media_dir.join(&file_name);

        if !dest_path.exists()
            && let Ok(resp) = client.get(media_url).send().await
            && let Ok(bytes) = resp.bytes().await
        {
            let _ = fs::write(&dest_path, bytes);
        }

        if dest_path.exists() {
            downloaded += 1;
            let rel_path = format!("./media/{file_name}");
            detail.local_media.push(rel_path.clone());
            // Replace remote image url with local relative path in detail.text!
            detail.text = detail.text.replace(media_url, &rel_path);
        }
    }

    spinner.finish_and_clear();
    ui::success(format!(
        "downloaded {downloaded} media images to {}",
        ui::style_bold(&media_dir.display().to_string())
    ));
    Ok(())
}

fn write_tweet_file(detail: &TweetDetail, path: &Path, format: ReadOutputFormat) -> Result<()> {
    match format {
        ReadOutputFormat::Markdown | ReadOutputFormat::Mdx => {
            let mut content = String::new();

            let display_title = detail.article_title.clone().unwrap_or_else(|| {
                let first_line = detail.text.lines().next().unwrap_or(&detail.author_name);
                first_line.chars().take(80).collect()
            });

            content.push_str("---\n");
            content.push_str(&format!(
                "title: {}\n",
                serde_json::to_string(&display_title).unwrap_or_default()
            ));
            content.push_str(&format!(
                "author: \"{} (@{})\"\n",
                detail.author_name, detail.author_handle
            ));
            content.push_str(&format!("url: \"{}\"\n", detail.url));
            content.push_str(&format!("date: \"{}\"\n", detail.created_at));
            content.push_str(&format!("type: \"{}\"\n", detail.tweet_type));
            content.push_str("---\n\n");

            if let Some(ref title) = detail.article_title {
                content.push_str(&format!("# 📰 {title}\n\n"));
            }

            content.push_str(&detail.text);
            content.push_str("\n\n");

            if !detail.text.contains("![Image") {
                if !detail.local_media.is_empty() {
                    content.push_str("### 🖼️ Attached Media\n\n");
                    for (idx, img_path) in detail.local_media.iter().enumerate() {
                        content.push_str(&format!("![Image {}]({img_path})\n\n", idx + 1));
                    }
                } else if !detail.media_urls.is_empty() {
                    content.push_str("### 🖼️ Attached Media\n\n");
                    for (idx, img_url) in detail.media_urls.iter().enumerate() {
                        content.push_str(&format!("![Image {}]({img_url})\n\n", idx + 1));
                    }
                }
            }

            fs::write(path, content).context("failed to write Markdown/MDX file")?;
        }
        ReadOutputFormat::Json => {
            let json =
                serde_json::to_string_pretty(detail).context("failed to serialize JSON output")?;
            fs::write(path, json).context("failed to write JSON file")?;
        }
    }

    Ok(())
}
fn sanitize_filename(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' || ('\u{4e00}'..='\u{9fa5}').contains(&c)
            {
                c
            } else {
                '_'
            }
        })
        .collect();

    let parts: Vec<&str> = clean.split('_').filter(|s| !s.is_empty()).collect();
    let result = parts.join("_");
    let truncated: String = result.chars().take(50).collect();
    if truncated.is_empty() {
        "post".to_string()
    } else {
        truncated
    }
}
