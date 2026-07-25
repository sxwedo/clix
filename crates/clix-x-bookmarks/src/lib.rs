use std::{
    env, fmt, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use clix_core::ui;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const TWITTER_BEARER_TOKEN: &str = "Bearer AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs%3D1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA";

const QUERY_IDS: &[&str] = &["RV1g3b8n_SGOHwkqKYSCFw", "tmd4ifV8RHltzn8ymGg1aw"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Markdown,
    Urls,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TweetType {
    Article,
    NoteTweet,
    Video,
    Photo,
    Quote,
    Reply,
    Tweet,
}

impl fmt::Display for TweetType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Article => write!(f, "Article"),
            Self::NoteTweet => write!(f, "NoteTweet"),
            Self::Video => write!(f, "Video"),
            Self::Photo => write!(f, "Photo"),
            Self::Quote => write!(f, "Quote"),
            Self::Reply => write!(f, "Reply"),
            Self::Tweet => write!(f, "Tweet"),
        }
    }
}

#[derive(Debug, Args)]
pub struct BookmarksArgs {
    /// Twitter auth_token cookie (or set X_AUTH_TOKEN / TWITTER_AUTH_TOKEN env)
    #[arg(long)]
    pub auth_token: Option<String>,

    /// Twitter ct0 (CSRF) cookie (or set X_CT0 / TWITTER_CT0 env)
    #[arg(long)]
    pub ct0: Option<String>,

    /// Output file path (default: x_bookmarks.md)
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Output format: markdown, urls, or json
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Markdown)]
    pub format: OutputFormat,

    /// Maximum number of bookmarks to fetch (default: all)
    #[arg(short = 'n', long)]
    pub count: Option<usize>,

    /// Download attached media (images/photos) locally into a `media/` folder
    #[arg(long)]
    pub download_media: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TweetBookmark {
    pub id: String,
    pub tweet_type: TweetType,
    pub author_name: String,
    pub author_handle: String,
    pub text: String,
    pub created_at: String,
    pub url: String,
    #[serde(default)]
    pub media: Vec<String>,
    #[serde(default)]
    pub local_media: Vec<String>,
}

pub async fn run(args: BookmarksArgs) -> Result<()> {
    let auth_token = resolve_auth_token(args.auth_token);
    let ct0 = resolve_ct0(args.ct0);

    let (auth_token, ct0) = match (auth_token, ct0) {
        (Some(a), Some(c)) => (a, c),
        _ => bail!(
            "Missing X authentication credentials!\n\n\
             Please provide both credentials via CLI flags or env vars:\n  \
             clix x bookmarks --auth-token \"<auth_token>\" --ct0 \"<ct0>\"\n\n\
             Or set environment variables:\n  \
             export X_AUTH_TOKEN=\"...\"\n  \
             export X_CT0=\"...\""
        ),
    };

    let output_path = args.output.unwrap_or_else(|| match args.format {
        OutputFormat::Markdown => PathBuf::from("x_bookmarks.md"),
        OutputFormat::Urls => PathBuf::from("x_bookmarks_urls.txt"),
        OutputFormat::Json => PathBuf::from("x_bookmarks.json"),
    });

    let client = build_http_client(&auth_token, &ct0)?;

    let spinner = ui::create_spinner("fetching X bookmarks...");

    let mut bookmarks = Vec::new();
    let mut cursor: Option<String> = None;
    let limit = args.count.unwrap_or(usize::MAX);

    let features_json = serde_json::json!({
        "graphql_timeline_v2_bookmark_timeline": true,
        "responsive_web_graphql_exclude_directive_enabled": true,
        "verified_phone_label_enabled": false,
        "creator_subscriptions_tweet_preview_api_enabled": true,
        "responsive_web_graphql_timeline_navigation_enabled": true,
        "responsive_web_graphql_skip_user_profile_image_extensions_enabled": false,
        "tweet_with_visibility_results_prefer_gql_limited_actions_policy_enabled": true,
        "tweet_awards_web_tipping_enabled": false,
        "freedom_of_speech_not_reach_fetch_enabled": true,
        "standardized_nudges_misinfo": true,
        "tweet_with_visibility_results_prefer_gql_media_interstitial_enabled": true,
        "rweb_video_timestamps_enabled": true,
        "longform_notetweets_rich_text_read_enabled": true,
        "longform_notetweets_inline_media_enabled": true,
        "responsive_web_enhance_cards_enabled": false
    })
    .to_string();

    loop {
        if bookmarks.len() >= limit {
            break;
        }

        spinner.set_message(format!(
            "fetching X bookmarks ({})",
            ui::style_bold(&format!("{} fetched", bookmarks.len()))
        ));

        let page_count = std::cmp::min(20, limit - bookmarks.len());

        let mut variables_json = serde_json::json!({
            "count": page_count,
            "includePromotedContent": false
        });
        if let Some(ref c) = cursor {
            variables_json["cursor"] = serde_json::Value::String(c.clone());
        }

        let mut fetched_page = false;
        let mut next_cursor: Option<String> = None;

        for query_id in QUERY_IDS {
            let url = format!(
                "https://x.com/i/api/graphql/{query_id}/Bookmarks?variables={}&features={}",
                urlencoding::encode(&variables_json.to_string()),
                urlencoding::encode(&features_json)
            );

            let resp = match client.get(&url).send().await {
                Ok(r) => r,
                Err(_) => continue,
            };

            let status = resp.status();
            if !status.is_success() {
                continue;
            }

            let body: Value = match resp.json().await {
                Ok(v) => v,
                Err(_) => continue,
            };

            let (page_tweets, page_cursor) = parse_bookmarks_response(&body);
            if page_tweets.is_empty() && page_cursor.is_none() {
                continue;
            }

            for tweet in page_tweets {
                if bookmarks.len() < limit
                    && !bookmarks.iter().any(|b: &TweetBookmark| b.id == tweet.id)
                {
                    bookmarks.push(tweet);
                }
            }

            next_cursor = page_cursor;
            fetched_page = true;
            break;
        }

        if !fetched_page || next_cursor.is_none() || next_cursor == cursor {
            break;
        }

        cursor = next_cursor;
    }

    spinner.finish_and_clear();

    if bookmarks.is_empty() {
        bail!(
            "No bookmarks found or failed to authenticate with X. Please verify your auth_token and ct0 values."
        );
    }

    if args.download_media {
        download_all_media(&client, &mut bookmarks, &output_path).await?;
    }

    write_output(&bookmarks, &output_path, args.format)?;

    ui::success(format!(
        "exported {} X bookmarks to {}",
        ui::style_bold(&bookmarks.len().to_string()),
        ui::style_bold(&output_path.display().to_string())
    ));

    Ok(())
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

fn parse_bookmarks_response(val: &Value) -> (Vec<TweetBookmark>, Option<String>) {
    let mut tweets = Vec::new();
    let mut next_cursor = None;

    let instructions = val
        .pointer("/data/bookmark_timeline_v2/timeline/instructions")
        .or_else(|| val.pointer("/data/bookmark_timeline/timeline/instructions"))
        .and_then(|v| v.as_array());

    if let Some(instructions) = instructions {
        for inst in instructions {
            let entries = inst.get("entries").and_then(|v| v.as_array());
            if let Some(entries) = entries {
                for entry in entries {
                    // Check for bottom cursor
                    let entry_id = entry.get("entryId").and_then(|v| v.as_str()).unwrap_or("");
                    let cursor_type = entry
                        .pointer("/content/cursorType")
                        .and_then(|v| v.as_str());

                    if (entry_id.starts_with("cursor-bottom-") || cursor_type == Some("Bottom"))
                        && let Some(val) = entry.pointer("/content/value").and_then(|v| v.as_str())
                    {
                        next_cursor = Some(val.to_string());
                    }

                    // Extract tweet item
                    let tweet_result = entry
                        .pointer("/content/itemContent/tweet_results/result")
                        .or_else(|| entry.pointer("/item/itemContent/tweet_results/result"));

                    if let Some(tweet) = tweet_result.and_then(extract_tweet) {
                        tweets.push(tweet);
                    }
                }
            }
        }
    }

    (tweets, next_cursor)
}

fn extract_tweet(res: &Value) -> Option<TweetBookmark> {
    // Handle TweetWithVisibilityResults wrapper
    let target =
        if res.get("__typename").and_then(|v| v.as_str()) == Some("TweetWithVisibilityResults") {
            res.get("tweet")?
        } else {
            res
        };

    let id = target.get("rest_id")?.as_str()?.to_string();

    let user_legacy = target.pointer("/core/user_results/result/legacy");
    let author_handle = user_legacy
        .and_then(|u| u.get("screen_name"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let author_name = user_legacy
        .and_then(|u| u.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or(&author_handle)
        .to_string();

    let article_title = target
        .pointer("/article/article_results/result/title")
        .or_else(|| target.pointer("/article/title"))
        .and_then(|v| v.as_str());

    let raw_text = target
        .pointer("/note_tweet/note_tweet_results/result/text")
        .and_then(|v| v.as_str())
        .or_else(|| target.pointer("/legacy/full_text").and_then(|v| v.as_str()))
        .unwrap_or("");

    let text = if let Some(title) = article_title {
        format!("📰 [{title}] {raw_text}")
    } else {
        raw_text.to_string()
    };

    let created_at = target
        .pointer("/legacy/created_at")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let media = extract_media_urls(target);
    let tweet_type = determine_tweet_type(target, article_title.is_some(), &media);
    let url = format!("https://x.com/{author_handle}/status/{id}");

    Some(TweetBookmark {
        id,
        tweet_type,
        author_name,
        author_handle,
        text,
        created_at,
        url,
        media,
        local_media: Vec::new(),
    })
}

fn determine_tweet_type(target: &Value, is_article: bool, media: &[String]) -> TweetType {
    if is_article {
        return TweetType::Article;
    }

    if target.pointer("/note_tweet").is_some() {
        return TweetType::NoteTweet;
    }

    let media_array = target
        .pointer("/legacy/extended_entities/media")
        .or_else(|| target.pointer("/legacy/entities/media"))
        .and_then(|v| v.as_array());

    if let Some(arr) = media_array {
        for m in arr {
            if let Some(kind) = m.get("type").and_then(|v| v.as_str())
                && (kind == "video" || kind == "animated_gif")
            {
                return TweetType::Video;
            }
        }
        if !media.is_empty() {
            return TweetType::Photo;
        }
    }

    if target
        .pointer("/legacy/is_quote_status")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || target.pointer("/quoted_status_result").is_some()
    {
        return TweetType::Quote;
    }

    if target
        .pointer("/legacy/in_reply_to_status_id_str")
        .and_then(|v| v.as_str())
        .is_some()
    {
        return TweetType::Reply;
    }

    TweetType::Tweet
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

    // 2. Legacy / extended_entities media
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

async fn download_all_media(
    client: &reqwest::Client,
    bookmarks: &mut [TweetBookmark],
    output_path: &Path,
) -> Result<()> {
    let base_dir = output_path.parent().unwrap_or_else(|| Path::new("."));
    let media_dir = base_dir.join("media");
    fs::create_dir_all(&media_dir).context("failed to create media directory")?;

    let spinner = ui::create_spinner("downloading attached media images...");

    let mut downloaded = 0;
    for b in bookmarks.iter_mut() {
        for (idx, media_url) in b.media.iter().enumerate() {
            let ext = media_url
                .split('.')
                .next_back()
                .unwrap_or("jpg")
                .split('?')
                .next()
                .unwrap_or("jpg");
            let file_name = format!("{}_{}_{}.{}", b.author_handle, b.id, idx + 1, ext);
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
                if !b.local_media.contains(&rel_path) {
                    b.local_media.push(rel_path);
                }
            }
        }
    }

    spinner.finish_and_clear();
    ui::success(format!(
        "downloaded {downloaded} media images to {}",
        ui::style_bold(&media_dir.display().to_string())
    ));
    Ok(())
}

fn write_output(bookmarks: &[TweetBookmark], path: &Path, format: OutputFormat) -> Result<()> {
    match format {
        OutputFormat::Markdown => {
            let mut content = String::new();
            content.push_str("# X (Twitter) Bookmarks\n\n");
            content.push_str(&format!("Total: {} bookmarks\n\n", bookmarks.len()));
            content.push_str("| Type | Author | Tweet | Media / Links |\n");
            content.push_str("| --- | --- | --- | --- |\n");
            for b in bookmarks {
                let clean_text = b.text.replace('\n', " ").replace('|', "\\|");
                let mut media_links = Vec::new();

                if !b.local_media.is_empty() {
                    for m in &b.local_media {
                        media_links.push(format!("[![Img]({m})]({m})"));
                    }
                } else {
                    for m in &b.media {
                        media_links.push(format!("[![Img]({m})]({m})"));
                    }
                }

                let media_col = if media_links.is_empty() {
                    format!("[View Status]({})", b.url)
                } else {
                    format!("[View Status]({})<br/>🖼️ {}", b.url, media_links.join(" "))
                };

                content.push_str(&format!(
                    "| `{}` | {} (@{}) | {} | {} |\n",
                    b.tweet_type, b.author_name, b.author_handle, clean_text, media_col
                ));
            }
            fs::write(path, content).context("failed to write Markdown output")?;
        }
        OutputFormat::Urls => {
            let mut content = String::new();
            for b in bookmarks {
                content.push_str(&format!("[{}] {}\n", b.tweet_type, b.url));
                for m in &b.media {
                    content.push_str(&format!("  └─ {}\n", m));
                }
            }
            fs::write(path, content).context("failed to write URLs output")?;
        }
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(bookmarks)
                .context("failed to serialize JSON output")?;
            fs::write(path, json).context("failed to write JSON output")?;
        }
    }
    Ok(())
}
